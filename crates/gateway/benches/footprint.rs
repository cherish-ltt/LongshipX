//! 连接内存足迹基准:测量常驻 TLS 连接的每连接内存成本。
//!
//! 不使用 criterion(内存足迹不是时间量):以"阶梯建连 + 快照"输出报告。
//! 两个指标:
//! * 进程 RSS —— macOS 走 `proc_pidinfo`,Linux 走 `/proc/self/statm`;
//! * 活跃堆分配字节数 —— mimalloc 当前线程的活跃块字节数(包含分配器尺寸取整)。
//!
//! 单进程内同时容纳服务端与客户端两端,因此"每连接成本"包含连接的两半。
//! 服务端启用并发握手管线、客户端建连压成固定并发,规避系统 backlog 限制
//! (macOS `kern.ipc.somaxconn=128`);进程先提升 fd 上限(默认 256 不够用)。
//! 运行:`cargo bench --bench footprint -p longshipx-gateway`

use std::sync::Arc;
use std::time::Duration;

use longshipx_application::ports::SessionTokenStore as _;
use longshipx_application::{GetPlayerProfile, RoomService, RoomServiceConfig};
use longshipx_gateway::tcp::config::{RateLimitSettings, TcpGatewayConfig};
use longshipx_gateway::tcp::router_setup::build_router;
use longshipx_gateway::tcp::{AuthGate, ConnectionRegistry, GatewayDeps, TcpGateway};
use longshipx_infrastructure::cache::InMemoryTokenStore;
use longshipx_infrastructure::events::InMemoryEventPublisher;
use longshipx_infrastructure::persistence::repositories::InMemoryPlayerRepository;
use longshipx_net_kit::codec::{Codec as _, read_frame, write_frame};
use longshipx_net_kit::tls::server_acceptor_from_pem_bytes;
use longshipx_protocol::generated as pb;
use longshipx_protocol::{ClientCodec, GameCodec, InboundMessage, OutboundMessage};
use rustls::pki_types::pem::PemObject as _;
use tokio::io::{ReadHalf, WriteHalf};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

mod support;

use support::{ConnectGate, SETUP_CONCURRENCY, raise_fd_limit, tcp_connect_reuse};
/// 累计连接数档位:64 → 128 → 256(每连接占两个临时端口,封顶 256 档)。
const STAGES: [usize; 3] = [64, 128, 256];
/// 每档建连完成后的稳定等待(让分配与映射落定)。
const SETTLE: Duration = Duration::from_millis(500);
/// 与网关基准一致的单帧上限。
const MAX_FRAME_SIZE: usize = 65_536;
/// 服务端 TLS 握手并行度(同 tcp_room 基准)。
const ACCEPT_CONCURRENCY: u32 = 512;

#[path = "../../net-kit/tests/support/allocations.rs"]
mod allocations;

#[cfg(target_os = "macos")]
fn resident_bytes() -> u64 {
    unsafe {
        let mut info: libc::proc_taskinfo = std::mem::zeroed();
        let size = std::mem::size_of::<libc::proc_taskinfo>() as i32;
        let written = libc::proc_pidinfo(
            libc::getpid(),
            libc::PROC_PIDTASKINFO,
            0,
            &mut info as *mut libc::proc_taskinfo as *mut core::ffi::c_void,
            size,
        );
        assert_eq!(written, size, "proc_pidinfo failed");
        info.pti_resident_size as u64
    }
}

#[cfg(target_os = "linux")]
fn resident_bytes() -> u64 {
    let statm = std::fs::read_to_string("/proc/self/statm").expect("read statm");
    let pages = statm
        .split_whitespace()
        .nth(1)
        .and_then(|value| value.parse::<u64>().ok())
        .expect("resident page count");
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    assert!(page_size > 0, "page size unavailable");
    pages * page_size as u64
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn resident_bytes() -> u64 {
    0
}

fn bench_config(addr: std::net::SocketAddr) -> TcpGatewayConfig {
    TcpGatewayConfig {
        shutdown_timeout: Duration::from_secs(1),
        write_drain_timeout: Duration::from_millis(50),
        bind_addr: addr,
        max_frame_size: MAX_FRAME_SIZE,
        send_queue_capacity: 4096,
        room_event_capacity: 1024,
        unauth_timeout: Duration::from_secs(120),
        heartbeat_timeout: Duration::from_secs(3600),
        rate: RateLimitSettings {
            enabled: false,
            per_second: 1_000_000,
            burst: 1_000_000,
        },
        max_connections: 16_384,
        backlog: 4096,
        accept_concurrency: ACCEPT_CONCURRENCY,
        nodelay: true,
        keepalive: Duration::from_secs(30),
    }
}

/// 基准进程独立的自签证书装置。
fn cert_pems() -> (&'static str, &'static str) {
    static CERTS: std::sync::OnceLock<(String, String)> = std::sync::OnceLock::new();
    let pair = CERTS.get_or_init(|| {
        let key = rcgen::KeyPair::generate().expect("生成密钥");
        let params = rcgen::CertificateParams::new(vec!["localhost".to_string()]).expect("参数");
        let cert = params.self_signed(&key).expect("自签");
        (cert.pem(), key.serialize_pem())
    });
    (pair.0.as_str(), pair.1.as_str())
}

fn client_connector() -> TlsConnector {
    let (cert_pem, _) = cert_pems();
    let mut roots = rustls::RootCertStore::empty();
    for cert in rustls::pki_types::CertificateDer::pem_slice_iter(cert_pem.as_bytes()) {
        roots.add(cert.expect("合法证书")).expect("证书入库");
    }
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .expect("TLS 版本")
    .with_root_certificates(roots)
    .with_no_client_auth();
    TlsConnector::from(Arc::new(config))
}

struct FootprintServer {
    addr: std::net::SocketAddr,
    tokens: Arc<InMemoryTokenStore>,
    players: Arc<InMemoryPlayerRepository>,
    /// 保持停机通道开启:关闭会被 accept 循环解读为停机并关闭监听。
    _shutdown_tx: tokio::sync::watch::Sender<bool>,
}

async fn spawn_server() -> FootprintServer {
    let (cert, key) = cert_pems();
    let acceptor = server_acceptor_from_pem_bytes(cert, key).expect("构建 TLS acceptor");
    let tokens = Arc::new(InMemoryTokenStore::new());
    let players = Arc::new(InMemoryPlayerRepository::new());
    let publisher = Arc::new(InMemoryEventPublisher::new(4096));
    let rooms = Arc::new(RoomService::new(
        RoomServiceConfig {
            command_capacity: 1024,
            // 主城形态:容量直接给定(等同部署侧调大 APP_MAX_PLAYERS_PER_ROOM)。
            max_players: 16_384,
        },
        publisher,
    ));
    let deps = Arc::new(GatewayDeps {
        codec: Arc::new(GameCodec),
        router: build_router(),
        tokens: tokens.clone(),
        players: players.clone(),
        profile: Arc::new(GetPlayerProfile::new(players.clone())),
        rooms,
        auth_gate: Arc::new(AuthGate::new(4096)),
        connections: Arc::new(ConnectionRegistry::new()),
    });
    let gateway = TcpGateway::bind(bench_config("127.0.0.1:0".parse().unwrap()), acceptor, deps)
        .expect("绑定网关");
    let addr = gateway.local_addr().expect("本地地址");
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    tokio::spawn(async move { gateway.run(shutdown_rx).await });
    FootprintServer {
        addr,
        tokens,
        players,
        _shutdown_tx: shutdown_tx,
    }
}

type Halves = (
    ReadHalf<TlsStream<TcpStream>>,
    WriteHalf<TlsStream<TcpStream>>,
);

/// 绑定任务的共享装配参数。
struct BindTask {
    addr: std::net::SocketAddr,
    connector: TlsConnector,
    tokens: Arc<InMemoryTokenStore>,
    players: Arc<InMemoryPlayerRepository>,
    nickname: String,
    tx: mpsc::Sender<Halves>,
}

/// 绑定完成后把读写半部送回收集通道并保持打开(建连段结束即归还闸门名额)。
async fn bind_and_hold(task: BindTask) {
    // 足迹连接同为基准客户端:SO_LINGER=0 关闭不留 TIME_WAIT,
    // 防止阶梯建连累计的 TIME_WAIT 在后续档位耗尽临时端口。
    let tcp = tcp_connect_reuse(task.addr, true).await.expect("连接失败");
    let domain =
        rustls::pki_types::ServerName::try_from("localhost".to_string()).expect("合法域名");
    let tls = task
        .connector
        .connect(domain, tcp)
        .await
        .expect("TLS 握手失败");
    let (mut reader, mut writer) = tokio::io::split(tls);
    let player = task.players.seed(&task.nickname);
    let token = task
        .tokens
        .create(player.id(), Duration::from_secs(3600))
        .await
        .expect("签发令牌");
    let bind = InboundMessage::Bind(pb::BindRequest { token });
    let frame = ClientCodec.encode(&bind).expect("编码失败");
    write_frame(&mut writer, &frame, MAX_FRAME_SIZE)
        .await
        .expect("发送失败");
    let ack = read_frame(&mut reader, MAX_FRAME_SIZE)
        .await
        .expect("读失败")
        .expect("对端关闭");
    assert!(
        matches!(
            ClientCodec.decode(&ack),
            Ok(OutboundMessage::BindResult(result)) if result.ok
        ),
        "绑定失败"
    );
    task.tx.send((reader, writer)).await.expect("收集通道关闭");
}

/// 并发建连 + 绑定,把客户端读写半部保活到 `held` 中(连接保持打开)。
///
/// 建连段受 `ConnectGate` 固定并发约束;半部保活不占闸门名额,
/// 否则超过并发度的建连会永久阻塞(permit 持有到连接结束的历史缺陷)。
async fn connect_batch(
    server: &FootprintServer,
    connector: &TlsConnector,
    count: usize,
    held: &mut Vec<Halves>,
) {
    let (tx, mut rx) = mpsc::channel(count.max(1));
    let gate = ConnectGate::new(SETUP_CONCURRENCY);
    gate.run((0..count).map(|i| {
        let task = BindTask {
            addr: server.addr,
            connector: connector.clone(),
            tokens: server.tokens.clone(),
            players: server.players.clone(),
            nickname: format!("足迹客户端{i}"),
            tx: tx.clone(),
        };
        move || bind_and_hold(task)
    }))
    .await;
    drop(tx);
    for _ in 0..count {
        let halves = rx.recv().await.expect("建连通道关闭");
        held.push(halves);
    }
}

struct Snapshot {
    rss: u64,
    live: u64,
}

/// 打印一行相对基线的增量报告。
fn report_row(baseline: &Snapshot, conns: usize) {
    let rss = resident_bytes().saturating_sub(baseline.rss);
    let live = allocations::live_bytes().saturating_sub(baseline.live);
    let conns_f64 = conns.max(1) as f64;
    println!(
        "| {:>6} | {:>10.1} | {:>14.1} | {:>14.1} | {:>15.1} |",
        conns,
        rss as f64 / 1024.0 / 1024.0,
        rss as f64 / conns_f64 / 1024.0,
        live as f64 / 1024.0 / 1024.0,
        live as f64 / conns_f64 / 1024.0,
    );
}

async fn run() {
    let server = spawn_server().await;
    tokio::time::sleep(SETTLE).await;
    let baseline = Snapshot {
        rss: resident_bytes(),
        live: allocations::live_bytes(),
    };

    println!("连接内存足迹(loopback TLS,单线程运行时,单进程含客户端与服务端):");
    println!("| 连接数 | ΔRSS (MiB) | 每连接 RSS (KiB) | 活跃堆分配 (MiB) | 每连接分配 (KiB) |");
    println!("| --- | --- | --- | --- | --- |");
    report_row(&baseline, 0);

    let connector = client_connector();
    let mut held: Vec<Halves> = Vec::new();
    let mut total = 0usize;
    for &stage in &STAGES {
        let delta = stage - total;
        connect_batch(&server, &connector, delta, &mut held).await;
        total = stage;
        tokio::time::sleep(SETTLE).await;
        report_row(&baseline, total);
    }
    println!(
        "\n说明:增量为相对空载网关(0 连接)的差值;本进程当前持有 {} 条连接。",
        held.len()
    );
}

fn main() {
    raise_fd_limit();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("构建运行时");
    rt.block_on(run());
}
