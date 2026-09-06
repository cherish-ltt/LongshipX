//! TCP 网关端到端基准:真实 loopback TLS 链路上的会话建立、心跳与房间聊天广播。
//!
//! 完整装配 TcpGateway(内存版基础设施),覆盖 TLS 握手、鉴权绑定、路由分发、
//! 房间命令通道与事件广播的全部框架路径。
//!
//! 运行:`cargo bench --bench tcp_room -p longshipx-gateway`

use std::hint::black_box;
use std::sync::Arc;
use std::time::Duration;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
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
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

/// 与 e2e 基线一致的单帧上限。
const MAX_FRAME_SIZE: usize = 65_536;

type ClientTls = TlsStream<TcpStream>;
type ClientReader = ReadHalf<ClientTls>;
type ClientWriter = WriteHalf<ClientTls>;

fn bench_config(addr: std::net::SocketAddr) -> TcpGatewayConfig {
    TcpGatewayConfig {
        bind_addr: addr,
        max_frame_size: MAX_FRAME_SIZE,
        send_queue_capacity: 256,
        room_event_capacity: 256,
        unauth_timeout: Duration::from_secs(10),
        heartbeat_timeout: Duration::from_secs(120),
        rate: RateLimitSettings {
            enabled: false,
            per_second: 100_000,
            burst: 100_000,
        },
        max_connections: 1024,
        backlog: 256,
        nodelay: true,
        keepalive: Duration::from_secs(30),
    }
}

fn runtime() -> Runtime {
    Builder::new_multi_thread()
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
    _shutdown_tx: tokio::sync::watch::Sender<bool>,
    _handle: JoinHandle<()>,
}

/// 完整装配 TCP 网关(内存基础设施),返回句柄供基准签发 token。
fn spawn_server(rt: &Runtime) -> BenchServer {
    rt.block_on(async {
        let (cert, key) = cert_pems();
        let acceptor = server_acceptor_from_pem_bytes(cert, key).expect("构建 TLS acceptor");
        let tokens = Arc::new(InMemoryTokenStore::new());
        let players = Arc::new(InMemoryPlayerRepository::new());
        let publisher = Arc::new(InMemoryEventPublisher::new(256));
        let rooms = Arc::new(RoomService::new(
            RoomServiceConfig {
                command_capacity: 256,
                max_players: 64,
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
            auth_gate: Arc::new(AuthGate::new(64)),
            connections: Arc::new(ConnectionRegistry::new()),
        });
        let gateway =
            TcpGateway::bind(bench_config("127.0.0.1:0".parse().unwrap()), acceptor, deps)
                .expect("绑定网关");
        let addr = gateway.local_addr().expect("本地地址");
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
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
    let tcp = TcpStream::connect(addr).await.expect("连接失败");
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
    server: &BenchServer,
    connector: &TlsConnector,
    nickname: &str,
) -> BenchClient {
    let mut client = connect_client(server.addr, connector).await;
    let player = server.players.seed(nickname);
    let token = server
        .tokens
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

/// 接收端后台任务:过滤房间事件,只把 Chat 广播转发给基准循环。
fn spawn_chat_reader(reader: ClientReader, tx: mpsc::Sender<OutboundMessage>) -> JoinHandle<()> {
    let mut reader = reader;
    tokio::spawn(async move {
        loop {
            let frame = match read_frame(&mut reader, MAX_FRAME_SIZE).await {
                Ok(Some(frame)) => frame,
                Ok(None) | Err(_) => break,
            };
            let Ok(message) = ClientCodec.decode(&frame) else {
                continue;
            };
            let is_chat = matches!(&message, OutboundMessage::RoomEvent(event)
                if matches!(event.event, Some(pb::room_event_notification::Event::Chat(_))));
            if is_chat && tx.send(message).await.is_err() {
                break;
            }
        }
    })
}

/// 丢弃读半部的后台任务:给不参与测量的成员(如发送者自己)排空事件。
fn spawn_drainer(reader: ClientReader) -> JoinHandle<()> {
    let mut reader = reader;
    tokio::spawn(
        async move { while let Ok(Some(_)) = read_frame(&mut reader, MAX_FRAME_SIZE).await {} },
    )
}

fn extract_room_id(event: &pb::RoomEventNotification) -> String {
    match &event.event {
        Some(pb::room_event_notification::Event::MemberJoined(joined)) => joined.room_id.clone(),
        _ => panic!("应为 MemberJoined"),
    }
}

/// 建房 + N 名成员入房;返回发送端写半部与聊天广播接收通道。
async fn setup_chat(
    server: &BenchServer,
    connector: &TlsConnector,
    members: usize,
) -> (ClientWriter, mpsc::Receiver<OutboundMessage>) {
    let mut owner = bind_new_client(server, connector, "房主").await;
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
    spawn_drainer(owner_reader);

    let (tx, rx) = mpsc::channel(8);
    for i in 0..members {
        let mut member = bind_new_client(server, connector, &format!("成员{i}")).await;
        member
            .send(&InboundMessage::JoinRoom(pb::JoinRoomRequest {
                room_id: Some(room_id.clone()),
            }))
            .await;
        let (reader, writer) = member.into_parts();
        spawn_chat_reader(reader, tx.clone());
        std::mem::drop(writer);
    }
    (owner_writer, rx)
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
                let mut client = connect_client(server.addr, &connector).await;
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
    let mut client = rt.block_on(bind_new_client(&server, &connector, "心跳员"));

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

/// 1 个发送者 + N 个接收者的聊天广播端到端延迟(一次迭代 = 一条消息触达全部成员)。
fn bench_chat_broadcast(c: &mut Criterion) {
    let rt = runtime();
    let server = spawn_server(&rt);
    let connector = client_connector();

    let mut group = c.benchmark_group("gateway/tcp_room");
    for &members in &[8usize, 32usize] {
        let (mut sender_writer, mut receipts) =
            rt.block_on(setup_chat(&server, &connector, members));
        group.throughput(Throughput::Elements(members as u64));
        group.bench_function(BenchmarkId::from_parameter(members), |b| {
            b.iter(|| {
                rt.block_on(async {
                    let message = InboundMessage::RoomChat(pb::RoomChatRequest {
                        text: "a".repeat(64),
                    });
                    let frame = ClientCodec.encode(&message).expect("编码失败");
                    write_frame(&mut sender_writer, &frame, MAX_FRAME_SIZE)
                        .await
                        .expect("发送失败");
                    for _ in 0..members {
                        black_box(receipts.recv().await.expect("接收通道关闭"));
                    }
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
);
criterion_main!(benches);
