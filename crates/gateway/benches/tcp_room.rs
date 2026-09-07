//! TCP 网关端到端基准:真实 loopback TLS 链路上的会话建立、心跳与房间广播。
//!
//! 覆盖三类规模问题:
//! * 单连接路径:建连绑定、心跳往返;
//! * 单房间大扇出:8~8192 人的房间各广播一条聊天(主城地图形态);
//! * 多连接并发:256~8192 条已绑定连接同时心跳的全量 tick。
//!
//! 大规模档位的工程前提:
//! * 服务端启用并发握手管线(`accept_concurrency`),内核 accept 队列即时排空,
//!   否则 macOS(`kern.ipc.somaxconn=128`)等系统 backlog 限制会丢弃/重置突发连接;
//! * 客户端建连经 `ConnectGate` 压成固定并发(64 < 128),permit 随建连完成即归还
//!   ——若随连接生命周期持有,第 65 个建连起会永久阻塞(历史缺陷);
//! * 每个档位独立运行时与网关,档位结束整体销毁,fd 与房间状态互不残留;
//! * 房间事件经有界队列 `try_send` 分发,读端必须持续排空;所有等待都带看门狗
//!   超时,事件丢失即报错而非挂死。入房风暴期间的 MemberJoined 允许极小比例
//!   丢失(见 `MEMBER_LOSS_TOLERANCE`),但聊天广播零容忍。
//!
//! 运行:`cargo bench --bench tcp_room -p longshipx-gateway`

use std::hint::black_box;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group};
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
use tokio::runtime::{Builder, Runtime};
use tokio::sync::{Semaphore, mpsc, watch};
use tokio::task::JoinHandle;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

mod support;

use support::{
    ConnectGate, SETUP_CONCURRENCY, raise_fd_limit, tcp_connect_reuse, wait_for_count, watchdog,
};

/// 与 e2e 基线一致的单帧上限。
const MAX_FRAME_SIZE: usize = 65_536;
/// 房间广播规模梯度:小队 → 中型房间(更大档位受本机端口/调度限制,
/// 见 AGENTS 10.9)。
const ROOM_SIZES: [usize; 4] = [8, 32, 64, 256];
/// 多连接并发心跳 tick 的连接数梯度。
const TICK_SIZES: [usize; 4] = [8, 32, 64, 256];
/// 服务端 TLS 握手并行度:必须能即时排空内核 accept 队列,否则突发建连
/// 会超出 macOS somaxconn=128 的系统上限导致连接被重置。
const ACCEPT_CONCURRENCY: u32 = 512;
/// 入房风暴允许的 MemberJoined 丢失比例(5%):回执经有界队列 try_send 分发,
/// 千人以上同时入房时极小比例丢失属预期(策略一,PRD 8.5);聊天广播零容忍。
const MEMBER_LOSS_TOLERANCE: usize = 5;

/// 基准入口:8K 档位需要每进程数千个 fd,先提升到硬上限。
fn main() {
    raise_fd_limit();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_target(false)
        .init();
    benches();
}

type ClientTls = TlsStream<TcpStream>;
type ClientReader = ReadHalf<ClientTls>;
type ClientWriter = WriteHalf<ClientTls>;

fn bench_config(addr: std::net::SocketAddr) -> TcpGatewayConfig {
    TcpGatewayConfig {
        bind_addr: addr,
        max_frame_size: MAX_FRAME_SIZE,
        send_queue_capacity: 4096,
        room_event_capacity: 1024,
        unauth_timeout: Duration::from_secs(120),
        heartbeat_timeout: Duration::from_secs(600),
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

fn runtime() -> Runtime {
    // 大扇出档位:同一 tick 内要并发读数千条 TLS 流,默认核数的工作线程
    // 会把收发吞吐限制在少数几核;放宽到 2× 核数提升交叠度。
    Builder::new_multi_thread()
        .worker_threads(
            std::thread::available_parallelism()
                .map_or(4, |n| n.get() * 2)
                .max(4),
        )
        .enable_all()
        .build()
        .expect("构建运行时")
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

struct BenchServer {
    addr: std::net::SocketAddr,
    tokens: Arc<InMemoryTokenStore>,
    players: Arc<InMemoryPlayerRepository>,
    _shutdown_tx: watch::Sender<bool>,
    _handle: JoinHandle<()>,
}

/// 完整装配 TCP 网关(内存基础设施);防护类上限按大规模基准放宽。
fn spawn_server(rt: &Runtime) -> BenchServer {
    rt.block_on(async {
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
        let gateway =
            TcpGateway::bind(bench_config("127.0.0.1:0".parse().unwrap()), acceptor, deps)
                .expect("绑定网关");
        let addr = gateway.local_addr().expect("本地地址");
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let handle = tokio::spawn(async move { gateway.run(shutdown_rx).await });
        BenchServer {
            addr,
            tokens,
            players,
            _shutdown_tx: shutdown_tx,
            _handle: handle,
        }
    })
}

struct BenchClient {
    reader: ClientReader,
    writer: ClientWriter,
}

async fn connect_client(addr: std::net::SocketAddr, connector: &TlsConnector) -> BenchClient {
    // 所有基准客户端一律 SO_LINGER=0:连接数高的档位(数千)关闭时若走 FIN,
    // TIME_WAIT 会在本轮内直接占满临时端口区间,后续档位的建连全部
    // EADDRNOTAVAIL;基准进程退出即回收,无正常关机语义需求。
    connect_client_with(addr, connector, true).await
}

/// `linger_reset`:短命连接(建连-测量-立刻关闭)置 true,绕过 TIME_WAIT
/// 端口耗尽;长连接(房间成员/心跳)必须走 FIN 关闭。
async fn connect_client_with(
    addr: std::net::SocketAddr,
    connector: &TlsConnector,
    linger_reset: bool,
) -> BenchClient {
    let tcp = tcp_connect_reuse(addr, linger_reset)
        .await
        .expect("连接失败");
    let domain =
        rustls::pki_types::ServerName::try_from("localhost".to_string()).expect("合法域名");
    let tls = connector.connect(domain, tcp).await.expect("TLS 握手失败");
    let (reader, writer) = tokio::io::split(tls);
    BenchClient { reader, writer }
}

impl BenchClient {
    async fn send(&mut self, message: &InboundMessage) {
        let frame = ClientCodec.encode(message).expect("编码失败");
        write_frame(&mut self.writer, &frame, MAX_FRAME_SIZE)
            .await
            .expect("发送失败");
    }

    async fn recv(&mut self) -> OutboundMessage {
        let frame = read_frame(&mut self.reader, MAX_FRAME_SIZE)
            .await
            .expect("读失败")
            .expect("对端关闭");
        ClientCodec.decode(&frame).expect("解码失败")
    }

    fn into_parts(self) -> (ClientReader, ClientWriter) {
        (self.reader, self.writer)
    }
}

/// 颁发 token 并完成真实链路绑定,返回可用客户端。
async fn bind_new_client(
    addr: std::net::SocketAddr,
    connector: &TlsConnector,
    tokens: &Arc<InMemoryTokenStore>,
    players: &Arc<InMemoryPlayerRepository>,
    nickname: &str,
) -> BenchClient {
    let mut client = connect_client(addr, connector).await;
    let player = players.seed(nickname);
    let token = tokens
        .create(player.id(), Duration::from_secs(600))
        .await
        .expect("签发令牌");
    client
        .send(&InboundMessage::Bind(pb::BindRequest { token }))
        .await;
    match client.recv().await {
        OutboundMessage::BindResult(result) if result.ok => client,
        other => panic!("绑定失败: {other:?}"),
    }
}

/// 判定消息是否为携带匹配事件的 RoomEvent。
fn is_room_event(
    message: &OutboundMessage,
    predicate: impl Fn(Option<&pb::room_event_notification::Event>) -> bool,
) -> bool {
    matches!(message, OutboundMessage::RoomEvent(event) if predicate(event.event.as_ref()))
}

/// 丢弃读半部:给不参与测量的连接(如发送者自己)持续排空事件。
async fn drain_forever(mut reader: ClientReader) {
    while let Ok(Some(_)) = read_frame(&mut reader, MAX_FRAME_SIZE).await {}
}

fn extract_room_id(event: &pb::RoomEventNotification) -> String {
    match &event.event {
        Some(pb::room_event_notification::Event::MemberJoined(joined)) => joined.room_id.clone(),
        _ => panic!("应为 MemberJoined"),
    }
}

/// 成员任务的共享装配参数(避免超长参数列表)。
struct MemberCtx {
    addr: std::net::SocketAddr,
    connector: TlsConnector,
    tokens: Arc<InMemoryTokenStore>,
    players: Arc<InMemoryPlayerRepository>,
    nickname: String,
    room_id: String,
    chats: mpsc::Sender<OutboundMessage>,
    joined: Arc<AtomicUsize>,
}

/// 成员任务:绑定 → 入房(等自身回执)→ 派生常驻聊天转发。
///
/// 只承担建连 + 入房段:常驻转发转交独立 task,建连闸门名额随即释放,
/// 允许后续成员继续建连(permit 持有到连接结束会让第 65 个建连永久阻塞)。
async fn join_member(ctx: MemberCtx) {
    let mut member = bind_new_client(
        ctx.addr,
        &ctx.connector,
        &ctx.tokens,
        &ctx.players,
        &ctx.nickname,
    )
    .await;
    member
        .send(&InboundMessage::JoinRoom(pb::JoinRoomRequest {
            room_id: Some(ctx.room_id.clone()),
        }))
        .await;
    // 自身 MemberJoined 回执 = 已入房(Actor 先插入回执再广播,首条房间事件必为它);
    // 出错帧(如超限被拒)时 watchdog 会以收到的消息内容报错,便于定位。
    let mut received = false;
    watchdog(&format!("成员 {} 入房回执", ctx.nickname), async {
        while !received {
            let message = member.recv().await;
            received = is_room_event(&message, |event| {
                matches!(
                    event,
                    Some(pb::room_event_notification::Event::MemberJoined(_))
                )
            });
            if !received {
                panic!("入房被拒: {message:?}");
            }
        }
    })
    .await;
    ctx.joined.fetch_add(1, Ordering::Release);
    tokio::spawn(run_chat_forwarder(ctx, member));
}

/// 常驻聊天转发:把本连接收到的聊天广播汇入基准收集通道(持续排空事件流)。
async fn run_chat_forwarder(ctx: MemberCtx, member: BenchClient) {
    let (mut reader, _writer) = member.into_parts();
    while let Ok(Some(frame)) = read_frame(&mut reader, MAX_FRAME_SIZE).await {
        let Ok(message) = ClientCodec.decode(&frame) else {
            continue;
        };
        let is_chat = is_room_event(&message, |event| {
            matches!(event, Some(pb::room_event_notification::Event::Chat(_)))
        });
        if is_chat && ctx.chats.send(message).await.is_err() {
            break;
        }
    }
}

/// 建房 + N 名成员并发入房;返回发送端写半部与聊天广播接收通道。
async fn setup_chat(
    server: &BenchServer,
    connector: &TlsConnector,
    members: usize,
) -> (ClientWriter, mpsc::Receiver<OutboundMessage>) {
    let mut owner = bind_new_client(
        server.addr,
        connector,
        &server.tokens,
        &server.players,
        "房主",
    )
    .await;
    owner
        .send(&InboundMessage::JoinRoom(pb::JoinRoomRequest {
            room_id: None,
        }))
        .await;
    let room_id = match owner.recv().await {
        OutboundMessage::RoomEvent(event) => extract_room_id(&event),
        other => panic!("房主应收到 MemberJoined: {other:?}"),
    };
    let (owner_reader, owner_writer) = owner.into_parts();
    tokio::spawn(drain_forever(owner_reader));

    let (chats, receipts) = mpsc::channel(256);
    let joined = Arc::new(AtomicUsize::new(0));
    let gate = ConnectGate::new(SETUP_CONCURRENCY);
    gate.run((0..members).map(|i| {
        let ctx = MemberCtx {
            addr: server.addr,
            connector: connector.clone(),
            tokens: server.tokens.clone(),
            players: server.players.clone(),
            nickname: format!("成员{i}"),
            room_id: room_id.clone(),
            chats: chats.clone(),
            joined: joined.clone(),
        };
        move || join_member(ctx)
    }))
    .await;
    // 允许极小比例的入房回执丢失(有界队列策略一),但聊天广播零容忍。
    let tolerance = members * MEMBER_LOSS_TOLERANCE / 100;
    wait_for_count(&joined, members - tolerance, "成员入房").await;
    (owner_writer, receipts)
}

/// 会话建立:TLS 握手 + 签发 token + 绑定往返(新玩家进场完整成本)。
fn bench_connect_bind(c: &mut Criterion) {
    let rt = runtime();
    let server = spawn_server(&rt);
    let connector = client_connector();

    let mut group = c.benchmark_group("gateway/tcp_room");
    group.bench_function("connect_tls_bind", |b| {
        b.iter(|| {
            rt.block_on(async {
                let mut client = connect_client_with(server.addr, &connector, true).await;
                let player = server.players.seed("新玩家");
                let token = server
                    .tokens
                    .create(player.id(), Duration::from_secs(600))
                    .await
                    .expect("签发令牌");
                client
                    .send(&InboundMessage::Bind(pb::BindRequest { token }))
                    .await;
                black_box(client.recv().await)
            })
        });
    });
    group.finish();
}

fn bench_heartbeat(c: &mut Criterion) {
    let rt = runtime();
    let server = spawn_server(&rt);
    let connector = client_connector();
    let mut client = rt.block_on(bind_new_client(
        server.addr,
        &connector,
        &server.tokens,
        &server.players,
        "心跳员",
    ));

    let mut group = c.benchmark_group("gateway/tcp_room");
    group.bench_function("heartbeat_rtt", |b| {
        b.iter(|| {
            rt.block_on(async {
                client
                    .send(&InboundMessage::Heartbeat(pb::HeartbeatPing {
                        client_ts_ms: 1,
                    }))
                    .await;
                black_box(client.recv().await)
            })
        });
    });
    group.finish();
}

/// 单房间广播:1 个发送者 + N 个成员,一次迭代 = 一条聊天触达全部成员。
/// 每个 N 独立装配运行时与网关,避免跨档位的连接与房间残留互相污染。
fn bench_chat_broadcast(c: &mut Criterion) {
    let connector = client_connector();

    let mut group = c.benchmark_group("gateway/tcp_room");
    for &members in &ROOM_SIZES {
        group.throughput(Throughput::Elements(members as u64));
        group.sample_size(30);
        group.bench_function(BenchmarkId::from_parameter(members), |b| {
            // 装置阶段:每档位独立运行时,基准结束整体销毁,fd 与房间不残留。
            let rt = runtime();
            let server = spawn_server(&rt);
            let (mut sender_writer, mut receipts) =
                rt.block_on(setup_chat(&server, &connector, members));
            b.iter(|| {
                rt.block_on(async {
                    let message = InboundMessage::RoomChat(pb::RoomChatRequest {
                        text: "a".repeat(64),
                    });
                    let frame = ClientCodec.encode(&message).expect("编码失败");
                    write_frame(&mut sender_writer, &frame, MAX_FRAME_SIZE)
                        .await
                        .expect("发送失败");
                    // 广播零容差:有界队列容量 ≥ 最大档位人数,零丢失可达。
                    let mut received = 0usize;
                    watchdog(
                        &format!("广播接收:{members} 人未全部送达(疑似事件被有界队列丢弃)"),
                        async {
                            while received < members {
                                black_box(receipts.recv().await.expect("接收通道关闭"));
                                received += 1;
                            }
                        },
                    )
                    .await;
                })
            });
        });
    }
    group.finish();
}

/// 心跳客户端任务的共享装配参数。
struct TickClientCtx {
    addr: std::net::SocketAddr,
    connector: TlsConnector,
    tokens: Arc<InMemoryTokenStore>,
    players: Arc<InMemoryPlayerRepository>,
    nickname: String,
    rounds: watch::Receiver<u64>,
    acked: Arc<AtomicUsize>,
    ready: Arc<AtomicUsize>,
}

/// 心跳客户端:每轮收到号令后发送心跳并等待应答,应答计数 +1。
///
/// 绑定段在 `ConnectGate` 名额内执行,**绑定完成立即返回**并把连接交给
/// 常驻 tick task——gate.run 依赖任务返回来推进下一批建连,若常驻循环
/// 占着任务体,4096 条连接的装配段会永久卡死(实测缺陷)。
async fn run_tick_client(ctx: TickClientCtx, tick_slots: Arc<Semaphore>) {
    let client = bind_new_client(
        ctx.addr,
        &ctx.connector,
        &ctx.tokens,
        &ctx.players,
        &ctx.nickname,
    )
    .await;
    // tick 名额在绑定段内领取:名额数 = 连接数,领取必成功且不阻塞。
    let _slot = tick_slots.acquire_owned().await.expect("tick 名额关闭");
    ctx.ready.fetch_add(1, Ordering::Release);
    tokio::spawn(run_tick_loop(ctx, client));
}

/// 常驻心跳循环:每轮号令 → 心跳 → 等应答,直至号令通道关闭。
async fn run_tick_loop(ctx: TickClientCtx, client: BenchClient) {
    let (mut reader, mut writer) = client.into_parts();
    let mut rounds = ctx.rounds;
    let mut last = 0u64;
    loop {
        if rounds.changed().await.is_err() {
            return;
        }
        let round = *rounds.borrow_and_update();
        if round <= last {
            continue;
        }
        last = round;
        let message = InboundMessage::Heartbeat(pb::HeartbeatPing { client_ts_ms: 1 });
        let frame = ClientCodec.encode(&message).expect("编码失败");
        if write_frame(&mut writer, &frame, MAX_FRAME_SIZE)
            .await
            .is_err()
        {
            return;
        }
        loop {
            match read_frame(&mut reader, MAX_FRAME_SIZE).await {
                Ok(Some(frame)) => {
                    if matches!(
                        ClientCodec.decode(&frame),
                        Ok(OutboundMessage::HeartbeatAck(_))
                    ) {
                        break;
                    }
                },
                _ => return,
            }
        }
        ctx.acked.fetch_add(1, Ordering::Release);
    }
}

/// K 条已绑定连接同时心跳:一轮号令 → 全部收到应答(全量 tick)。
fn bench_concurrent_heartbeat(c: &mut Criterion) {
    let connector = client_connector();

    let mut group = c.benchmark_group("gateway/tcp_concurrency");
    for &conns in &TICK_SIZES {
        group.throughput(Throughput::Elements(conns as u64));
        group.sample_size(20);
        group.bench_function(BenchmarkId::from_parameter(conns), |b| {
            let rt = runtime();
            let server = spawn_server(&rt);
            let (rounds_tx, acked) = rt.block_on(async {
                let (rounds_tx, rounds_rx) = watch::channel(0u64);
                let acked = Arc::new(AtomicUsize::new(0));
                let ready = Arc::new(AtomicUsize::new(0));
                let gate = ConnectGate::new(SETUP_CONCURRENCY);
                let tick_slots = Arc::new(Semaphore::new(conns));
                gate.run((0..conns).map(|i| {
                    let ctx = TickClientCtx {
                        addr: server.addr,
                        connector: connector.clone(),
                        tokens: server.tokens.clone(),
                        players: server.players.clone(),
                        nickname: format!("心跳客户端{i}"),
                        rounds: rounds_rx.clone(),
                        acked: acked.clone(),
                        ready: ready.clone(),
                    };
                    let tick_slots = tick_slots.clone();
                    move || run_tick_client(ctx, tick_slots)
                }))
                .await;
                wait_for_count(&ready, conns, "心跳客户端就绪").await;
                (rounds_tx, acked)
            });

            let mut round = 0u64;
            b.iter(|| {
                rt.block_on(async {
                    acked.store(0, Ordering::Release);
                    round += 1;
                    rounds_tx.send_replace(round);
                    watchdog(
                        &format!("心跳 tick:{conns} 连接未全部收到应答"),
                        async {
                            while acked.load(Ordering::Acquire) < conns {
                                tokio::time::sleep(Duration::from_micros(200)).await;
                            }
                        },
                    )
                    .await;
                })
            });
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_connect_bind,
    bench_heartbeat,
    bench_chat_broadcast,
    bench_concurrent_heartbeat,
);
