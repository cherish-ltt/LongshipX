//! 端到端测试(PRD 14 "Gateway/端到端"):真实 TCP+TLS 客户端连接网关,
//! 验证协议编解码、鉴权流程、房间广播、心跳与优雅停机。

use longshipx_application::ports::SessionTokenStore as _;
use longshipx_application::{RoomService, RoomServiceConfig};
use longshipx_gateway::tcp::config::{RateLimitSettings, TcpGatewayConfig};
use longshipx_gateway::tcp::router_setup::build_router;
use longshipx_gateway::tcp::{AuthGate, ConnectionRegistry, GatewayDeps, TcpGateway};
use longshipx_infrastructure::cache::InMemoryTokenStore;
use longshipx_infrastructure::events::InMemoryEventPublisher;
use longshipx_infrastructure::persistence::repositories::InMemoryPlayerRepository;
use longshipx_net_kit::codec::{Codec as _, read_frame, write_frame};
use longshipx_protocol::generated as pb;
use longshipx_protocol::{ClientCodec, GameCodec, InboundMessage, OutboundMessage};
use rustls::pki_types::pem::PemObject;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{ReadHalf, WriteHalf};
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;

const RATE_OFF: RateLimitSettings = RateLimitSettings {
    enabled: false,
    per_second: 1000,
    burst: 1000,
};

struct TestServer {
    addr: std::net::SocketAddr,
    shutdown_tx: tokio::sync::watch::Sender<bool>,
    tokens: Arc<InMemoryTokenStore>,
    players: Arc<InMemoryPlayerRepository>,
    handle: tokio::task::JoinHandle<()>,
    gateway: Arc<TcpGateway>,
    registry: Arc<ConnectionRegistry>,
    auth_gate: Arc<AuthGate>,
    rooms: Arc<RoomService>,
}

fn test_config(addr: std::net::SocketAddr) -> TcpGatewayConfig {
    TcpGatewayConfig {
        shutdown_timeout: Duration::from_secs(1),
        write_drain_timeout: Duration::from_millis(50),
        bind_addr: addr,
        max_frame_size: 65_536,
        send_queue_capacity: 64,
        room_event_capacity: 64,
        unauth_timeout: Duration::from_secs(2),
        heartbeat_timeout: Duration::from_secs(5),
        rate: RATE_OFF,
        max_connections: 64,
        backlog: 16,
        accept_concurrency: 4,
        nodelay: true,
        keepalive: Duration::from_secs(30),
    }
}

async fn spawn_server() -> TestServer {
    spawn_server_with(test_config("127.0.0.1:0".parse().unwrap())).await
}

async fn spawn_server_with(config: TcpGatewayConfig) -> TestServer {
    spawn_server_with_router(config, build_router()).await
}

async fn spawn_server_with_router(
    config: TcpGatewayConfig,
    router: longshipx_protocol::Router<longshipx_gateway::tcp::ConnContext>,
) -> TestServer {
    let certs = test_certs();
    let acceptor =
        longshipx_net_kit::tls::server_acceptor_from_pem_bytes(certs.0, certs.1).unwrap();
    let tokens = Arc::new(InMemoryTokenStore::new());
    let players = Arc::new(InMemoryPlayerRepository::new());
    let publisher = Arc::new(InMemoryEventPublisher::new(64));
    let rooms = Arc::new(RoomService::new(
        RoomServiceConfig {
            command_capacity: 64,
            max_players: 8,
        },
        publisher,
    ));
    let registry = Arc::new(ConnectionRegistry::new());
    let auth_gate = Arc::new(AuthGate::new(8));
    let deps = Arc::new(GatewayDeps {
        codec: Arc::new(GameCodec),
        router,
        tokens: tokens.clone(),
        players: players.clone(),
        profile: Arc::new(longshipx_application::GetPlayerProfile::new(
            players.clone(),
        )),
        rooms: rooms.clone(),
        auth_gate: auth_gate.clone(),
        connections: registry.clone(),
    });
    let gateway = Arc::new(TcpGateway::bind(config, acceptor, deps).unwrap());
    let addr = gateway.local_addr().unwrap();
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let gateway_task = gateway.clone();
    let handle = tokio::spawn(async move { gateway_task.run(shutdown_rx).await });
    TestServer {
        addr,
        shutdown_tx,
        tokens,
        players,
        handle,
        gateway,
        registry,
        auth_gate,
        rooms,
    }
}

fn test_certs() -> (&'static str, &'static str) {
    // 网关测试进程独立的证书装置(与 net-kit 的测试装置互不影响)。
    static CERTS: std::sync::OnceLock<(String, String)> = std::sync::OnceLock::new();
    let pair = CERTS.get_or_init(|| {
        let key = rcgen::KeyPair::generate().unwrap();
        let params = rcgen::CertificateParams::new(vec!["localhost".to_string()]).unwrap();
        let cert = params.self_signed(&key).unwrap();
        (cert.pem(), key.serialize_pem())
    });
    (pair.0.as_str(), pair.1.as_str())
}

struct TestClient {
    reader: ReadHalf<TlsStream<TcpStream>>,
    writer: WriteHalf<TlsStream<TcpStream>>,
}

impl TestClient {
    async fn connect(addr: std::net::SocketAddr) -> Self {
        let mut roots = rustls::RootCertStore::empty();
        let (cert_pem, _) = test_certs();
        for cert in rustls::pki_types::CertificateDer::pem_slice_iter(cert_pem.as_bytes()) {
            roots.add(cert.unwrap()).unwrap();
        }
        let config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
        let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
        let tcp = TcpStream::connect(addr).await.unwrap();
        let domain = rustls::pki_types::ServerName::try_from("localhost".to_string()).unwrap();
        let tls = connector.connect(domain, tcp).await.unwrap();
        let (reader, writer) = tokio::io::split(tls);
        Self { reader, writer }
    }

    async fn send(&mut self, message: &InboundMessage) {
        let frame = ClientCodec.encode(message).unwrap();
        write_frame(&mut self.writer, &frame, 65_536).await.unwrap();
    }

    async fn recv(&mut self) -> Option<OutboundMessage> {
        let frame = read_frame(&mut self.reader, 65_536).await.unwrap()?;
        ClientCodec.decode(&frame).ok()
    }

    async fn recv_expect(&mut self, what: &str) -> OutboundMessage {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_millis(500), self.recv()).await {
                Ok(Some(message)) => return message,
                Ok(None) => panic!("连接在对端关闭,等待 {what}"),
                Err(_) => continue,
            }
        }
        panic!("等待 {what} 超时");
    }
}

async fn make_player_with_token(server: &TestServer, nickname: &str) -> (pb::BindRequest, String) {
    let player = server.players.seed(nickname);
    let token = server
        .tokens
        .create(player.id(), Duration::from_secs(60))
        .await
        .unwrap();
    (pb::BindRequest { token }, nickname.to_string())
}

#[tokio::test]
async fn audit_shutdown_releases_registered_connections_and_auth_slots() {
    let server = spawn_server().await;
    let client = TestClient::connect(server.addr).await;
    for _ in 0..100 {
        if server.registry.active_count() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(server.registry.active_count(), 1);
    server.shutdown_tx.send_replace(true);
    tokio::time::timeout(Duration::from_secs(1), server.handle)
        .await
        .unwrap()
        .unwrap();
    drop(client);
    let drained = server
        .gateway
        .wait_until_drained(Duration::from_millis(150))
        .await;
    let auth_slots = server.auth_gate.unauth_count(server.addr.ip());
    assert!(
        drained && auth_slots == 0,
        "shutdown leaves active={} unauth_slots={auth_slots}",
        server.registry.active_count()
    );
}

#[tokio::test]
async fn bound_room_client_receives_notice_and_shutdown_reclaims_room() {
    let server = spawn_server().await;
    let mut client = TestClient::connect(server.addr).await;
    let (bind, _) = make_player_with_token(&server, "shutdown member").await;
    client.send(&InboundMessage::Bind(bind)).await;
    assert!(matches!(
        client.recv_expect("bind").await,
        OutboundMessage::BindResult(_)
    ));
    client
        .send(&InboundMessage::JoinRoom(pb::JoinRoomRequest {
            room_id: None,
        }))
        .await;
    client.recv_expect("join").await;
    server.shutdown_tx.send_replace(true);
    loop {
        if matches!(
            client.recv_expect("shutdown notice").await,
            OutboundMessage::Shutdown(_)
        ) {
            break;
        }
    }
    tokio::time::timeout(Duration::from_secs(2), server.handle)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(server.registry.active_count(), 0);
    assert_eq!(server.auth_gate.unauth_count(server.addr.ip()), 0);
    assert_eq!(server.rooms.active_rooms().await, 0);
}

#[tokio::test]
async fn panic_and_forced_deadline_do_not_leak_members_or_connection_tasks() {
    for panic in [false, true] {
        let mut router = build_router();
        let entered = Arc::new(tokio::sync::Notify::new());
        let entered_handler = entered.clone();
        router.route(
            longshipx_protocol::opcodes::OP_C2S_HEARTBEAT,
            move |_, _| {
                let entered = entered_handler.clone();
                async move {
                    entered.notify_one();
                    if panic {
                        panic!("injected handler panic");
                    }
                    std::future::pending::<()>().await;
                    Ok(None)
                }
            },
        );
        let mut config = test_config("127.0.0.1:0".parse().unwrap());
        config.shutdown_timeout = Duration::from_millis(100);
        let server = spawn_server_with_router(config, router).await;
        let mut client = TestClient::connect(server.addr).await;
        let (bind, _) = make_player_with_token(&server, "cancel member").await;
        client.send(&InboundMessage::Bind(bind)).await;
        client.recv_expect("bind").await;
        client
            .send(&InboundMessage::JoinRoom(pb::JoinRoomRequest {
                room_id: None,
            }))
            .await;
        client.recv_expect("join").await;
        client
            .send(&InboundMessage::Heartbeat(pb::HeartbeatPing {
                client_ts_ms: 0,
            }))
            .await;
        entered.notified().await;
        server.shutdown_tx.send_replace(true);
        tokio::time::timeout(Duration::from_secs(2), server.handle)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(server.registry.active_count(), 0);
        assert_eq!(server.auth_gate.unauth_count(server.addr.ip()), 0);
        assert_eq!(server.rooms.active_rooms().await, 0);
        let end = tokio::time::timeout(
            Duration::from_secs(1),
            read_frame(&mut client.reader, 65_536),
        )
        .await
        .unwrap();
        assert!(matches!(end, Ok(None) | Err(_)));
    }
}
