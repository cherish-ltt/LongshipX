//! 端到端测试(PRD 14 "Gateway/端到端"):真实 TCP/KCP + TLS 客户端连接网关,
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
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _, ReadHalf, WriteHalf};
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
    deps: Arc<GatewayDeps>,
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
    let tokens = Arc::new(InMemoryTokenStore::new());
    spawn_server_with_tokens(config, tokens.clone(), tokens).await
}

async fn spawn_server_with_tokens(
    config: TcpGatewayConfig,
    tokens: Arc<InMemoryTokenStore>,
    validation: Arc<dyn longshipx_application::ports::SessionTokenStore>,
) -> TestServer {
    spawn_server_with_router_tokens(config, tokens, validation, build_router()).await
}

async fn spawn_server_with_router_tokens(
    config: TcpGatewayConfig,
    tokens: Arc<InMemoryTokenStore>,
    validation: Arc<dyn longshipx_application::ports::SessionTokenStore>,
    router: longshipx_protocol::Router<longshipx_gateway::tcp::ConnContext>,
) -> TestServer {
    spawn_server_transport(config, tokens, validation, router, false).await
}

async fn spawn_server_transport(
    config: TcpGatewayConfig,
    tokens: Arc<InMemoryTokenStore>,
    validation: Arc<dyn longshipx_application::ports::SessionTokenStore>,
    router: longshipx_protocol::Router<longshipx_gateway::tcp::ConnContext>,
    kcp: bool,
) -> TestServer {
    let certs = test_certs();
    let acceptor =
        longshipx_net_kit::tls::server_acceptor_from_pem_bytes(certs.0, certs.1).unwrap();
    let players = Arc::new(InMemoryPlayerRepository::new());
    let publisher = Arc::new(InMemoryEventPublisher::new(64));
    let rooms = Arc::new(RoomService::new(
        RoomServiceConfig {
            command_capacity: 64,
            max_players: 8,
        },
        publisher,
    ));
    let deps = Arc::new(GatewayDeps {
        codec: Arc::new(GameCodec),
        router,
        tokens: validation,
        players: players.clone(),
        profile: Arc::new(longshipx_application::GetPlayerProfile::new(
            players.clone(),
        )),
        rooms,
        auth_gate: Arc::new(AuthGate::new(8)),
        connections: Arc::new(ConnectionRegistry::new()),
    });
    let gateway = if kcp {
        TcpGateway::bind_kcp(config, kcp_settings(), acceptor, deps.clone())
            .await
            .unwrap()
    } else {
        TcpGateway::bind(config, acceptor, deps.clone()).unwrap()
    };
    let addr = gateway.local_addr().unwrap();
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(async move { gateway.run(shutdown_rx).await });
    TestServer {
        addr,
        shutdown_tx,
        tokens,
        players,
        handle,
        deps,
    }
}

async fn spawn_server_with_rate(per_second: u64, burst: u64) -> (TestServer, TcpGatewayConfig) {
    let mut config = test_config("127.0.0.1:0".parse().unwrap());
    config.rate = RateLimitSettings {
        enabled: true,
        per_second,
        burst,
    };
    let server = spawn_server_with(config).await;
    (server, config)
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
    reader: ReadHalf<TlsStream<Box<dyn longshipx_net_kit::transport::ConnectionIo>>>,
    writer: WriteHalf<TlsStream<Box<dyn longshipx_net_kit::transport::ConnectionIo>>>,
}

impl TestClient {
    async fn connect(addr: std::net::SocketAddr) -> Self {
        Self::connect_transport(addr, false).await
    }

    async fn connect_transport(addr: std::net::SocketAddr, kcp: bool) -> Self {
        Self::connect_with_settings(addr, kcp, kcp_settings()).await
    }

    async fn connect_with_settings(
        addr: std::net::SocketAddr,
        kcp: bool,
        settings: longshipx_net_kit::kcp::KcpSettings,
    ) -> Self {
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
        let raw: Box<dyn longshipx_net_kit::transport::ConnectionIo> = if kcp {
            Box::new(
                longshipx_net_kit::kcp::KcpStream::connect(addr, settings)
                    .await
                    .unwrap(),
            )
        } else {
            let tcp = TcpStream::connect(addr).await.unwrap();
            tcp.set_nodelay(true).unwrap();
            Box::new(tcp)
        };
        let domain = rustls::pki_types::ServerName::try_from("localhost".to_string()).unwrap();
        let tls = tokio::time::timeout(Duration::from_secs(3), connector.connect(domain, raw))
            .await
            .unwrap()
            .unwrap();
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
async fn bind_heartbeat_and_room_flow_end_to_end() {
    let server = spawn_server().await;
    let (bind, _) = make_player_with_token(&server, "坦克一号").await;

    let mut alice = TestClient::connect(server.addr).await;
    alice.send(&InboundMessage::Bind(bind)).await;
    let reply = alice.recv_expect("BindResult").await;
    let OutboundMessage::BindResult(result) = reply else {
        panic!("应先收到 BindResult");
    };
    assert!(result.ok);
    assert!(result.player_id.is_some());

    // 心跳。
    alice
        .send(&InboundMessage::Heartbeat(pb::HeartbeatPing {
            client_ts_ms: 1,
        }))
        .await;
    assert!(matches!(
        alice.recv_expect("HeartbeatAck").await,
        OutboundMessage::HeartbeatAck(_)
    ));

    // 创建房间(广播自身 MemberJoined 作为回执)。
    alice
        .send(&InboundMessage::JoinRoom(pb::JoinRoomRequest {
            room_id: None,
        }))
        .await;
    let joined = alice.recv_expect("MemberJoined").await;
    let OutboundMessage::RoomEvent(event) = joined else {
        panic!("应收到房间事件");
    };
    let Some(pb::room_event_notification::Event::MemberJoined(j)) = event.event else {
        panic!("应为 MemberJoined");
    };
    let room_id = j.room_id.clone();

    // 聊天广播(包含发送者自己)。
    alice
        .send(&InboundMessage::RoomChat(pb::RoomChatRequest {
            text: "集合打龙".into(),
        }))
        .await;
    let chat = alice.recv_expect("Chat 广播").await;
    let OutboundMessage::RoomEvent(event) = chat else {
        panic!("应收到房间事件");
    };
    let Some(pb::room_event_notification::Event::Chat(c)) = event.event else {
        panic!("应为 Chat");
    };
    assert_eq!(c.text, "集合打龙");
    assert_eq!(c.room_id, room_id);

    // 第二名玩家加入同一房间,应看到第一条玩家与自己的加入事件。
    let (bind2, _) = make_player_with_token(&server, "奶妈一号").await;
    let mut bob = TestClient::connect(server.addr).await;
    bob.send(&InboundMessage::Bind(bind2)).await;
    let _ = bob.recv_expect("BindResult").await;
    bob.send(&InboundMessage::JoinRoom(pb::JoinRoomRequest {
        room_id: Some(room_id.clone()),
    }))
    .await;
    // bob 收到自己 MemberJoined;alice 也会收到。
    let _ = bob.recv_expect("bob MemberJoined").await;
    let _ = alice.recv_expect("alice MemberJoined(bob)").await;

    bob.send(&InboundMessage::RoomChat(pb::RoomChatRequest {
        text: "我来啦".into(),
    }))
    .await;
    let relayed = alice.recv_expect("alice 收到 bob 的聊天").await;
    let OutboundMessage::RoomEvent(event) = relayed else {
        panic!("应收到房间事件");
    };
    let Some(pb::room_event_notification::Event::Chat(c)) = event.event else {
        panic!("应为 Chat");
    };
    assert_eq!(c.text, "我来啦");
    assert_eq!(c.sender_nickname, "奶妈一号");

    server.shutdown_tx.send_replace(true);
}

#[tokio::test]
async fn bind_with_bad_token_is_rejected_but_connection_stays() {
    let server = spawn_server().await;
    let mut client = TestClient::connect(server.addr).await;
    client
        .send(&InboundMessage::Bind(pb::BindRequest {
            token: "bad-token".into(),
        }))
        .await;
    let OutboundMessage::BindResult(result) = client.recv_expect("BindResult").await else {
        panic!("应收到 BindResult");
    };
    assert!(!result.ok);
    assert!(result.error.is_some());
    server.shutdown_tx.send_replace(true);
}

/// 回归测试:连接断开后必须归还并发名额,否则累计连接数达到
/// `max_connections` 后所有新连接都会被拒(曾因连接处理器在
/// `writer.await` 处挂起而泄漏名额)。
#[tokio::test]
async fn connection_permits_are_released_after_disconnect() {
    let mut config = test_config("127.0.0.1:0".parse().unwrap());
    config.max_connections = 4;
    let server = spawn_server_with(config).await;

    for round in 0..8u32 {
        let (bind, _) = make_player_with_token(&server, &format!("轮回{round}")).await;
        let mut client = TestClient::connect(server.addr).await;
        client.send(&InboundMessage::Bind(bind)).await;
        let OutboundMessage::BindResult(result) = client.recv_expect("BindResult").await else {
            panic!("第 {round} 轮应收到 BindResult");
        };
        assert!(result.ok, "第 {round} 轮绑定应成功:连接名额应已归还");
        drop(client);
        // 等待服务端走完断开清理,避免与下一轮建连竞争名额归还时机。
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    server.shutdown_tx.send_replace(true);
}

#[tokio::test]
async fn message_before_bind_is_rejected_and_connection_closed() {
    let server = spawn_server().await;
    let mut client = TestClient::connect(server.addr).await;
    client
        .send(&InboundMessage::RoomChat(pb::RoomChatRequest {
            text: "未鉴权".into(),
        }))
        .await;
    let reply = client.recv_expect("错误通知").await;
    let OutboundMessage::Error(err) = reply else {
        panic!("应收到 ErrorNotification");
    };
    assert_eq!(err.code, longshipx_protocol::ERR_AUTH_REQUIRED_FIRST);
    // 服务端随后关闭连接。
    let closed = tokio::time::timeout(Duration::from_secs(2), client.reader.read_u8()).await;
    assert!(matches!(closed, Ok(Err(_)) | Err(_)));
    server.shutdown_tx.send_replace(true);
}

#[tokio::test]
async fn oversized_frame_disconnects_client() {
    let server = spawn_server().await;
    let mut client = TestClient::connect(server.addr).await;
    // 声明一个超过上限的帧长(不发送实际内容)。
    client
        .writer
        .write_all(&70_000u32.to_be_bytes())
        .await
        .unwrap();
    let closed = tokio::time::timeout(Duration::from_secs(2), client.reader.read_u8()).await;
    assert!(closed.is_ok(), "服务端应在校验帧长后断开");
    server.shutdown_tx.send_replace(true);
}

#[tokio::test]
async fn shutdown_notice_reaches_bound_clients() {
    let server = spawn_server().await;
    let (bind, _) = make_player_with_token(&server, "观察者").await;
    let mut client = TestClient::connect(server.addr).await;
    client.send(&InboundMessage::Bind(bind)).await;
    let _ = client.recv_expect("BindResult").await;

    server.shutdown_tx.send_replace(true);
    let reply = client.recv_expect("ServerShutdownNotice").await;
    assert!(matches!(reply, OutboundMessage::Shutdown(_)));
    let _ = tokio::time::timeout(Duration::from_secs(2), server.handle).await;
}

#[tokio::test]
async fn unauthenticated_connection_times_out() {
    let mut config = test_config("127.0.0.1:0".parse().unwrap());
    config.unauth_timeout = Duration::from_millis(300);
    let server = spawn_server_with(config).await;
    let mut client = TestClient::connect(server.addr).await;
    // 不发送 BIND,等待服务端超时断开。
    let reply = client.recv_expect("超时错误通知").await;
    let OutboundMessage::Error(err) = reply else {
        panic!("应收到 ErrorNotification");
    };
    assert_eq!(err.code, longshipx_protocol::ERR_TIMEOUT);
    server.shutdown_tx.send_replace(true);
}

#[tokio::test]
async fn rate_limit_exceeded_disconnects() {
    let (server, _config) = spawn_server_with_rate(5, 2).await;
    let (bind, _) = make_player_with_token(&server, "刷子").await;
    let mut client = TestClient::connect(server.addr).await;
    client.send(&InboundMessage::Bind(bind)).await;
    let _ = client.recv_expect("BindResult").await;
    // 突发容量 2:绑定消耗 1,再发 2 条后第 4 条触发限流断开。
    for _ in 0..4 {
        client
            .send(&InboundMessage::Heartbeat(pb::HeartbeatPing {
                client_ts_ms: 0,
            }))
            .await;
    }
    let outcome = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match client.recv().await {
                Some(OutboundMessage::Error(err))
                    if err.code == longshipx_protocol::ERR_RATE_LIMITED =>
                {
                    return true;
                },
                Some(_) => continue,
                None => return false,
            }
        }
    })
    .await;
    assert!(outcome.is_ok(), "应在限流后收到错误或被断开");
    server.shutdown_tx.send_replace(true);
}

#[tokio::test]
async fn revoked_session_cannot_read_profile() {
    let server = spawn_server().await;
    let player = server.players.seed("revoked");
    let token = server
        .tokens
        .create(player.id(), Duration::from_secs(60))
        .await
        .unwrap();
    let mut client = TestClient::connect(server.addr).await;
    client
        .send(&InboundMessage::Bind(pb::BindRequest { token }))
        .await;
    assert!(
        matches!(client.recv_expect("bind").await, OutboundMessage::BindResult(result) if result.ok)
    );
    server.tokens.revoke_player(player.id()).await.unwrap();
    client
        .send(&InboundMessage::GetProfile(pb::GetProfileRequest {}))
        .await;
    assert!(
        matches!(client.recv_expect("rejected revoked session").await, OutboundMessage::Error(error) if error.code == longshipx_protocol::ERR_NOT_AUTHENTICATED)
    );
    server.shutdown_tx.send_replace(true);
}

#[tokio::test]
async fn expired_session_cannot_execute_room_commands() {
    for kcp in [false, true] {
        let tokens = Arc::new(InMemoryTokenStore::new());
        let server = spawn_server_transport(
            test_config("127.0.0.1:0".parse().unwrap()),
            tokens.clone(),
            tokens,
            build_router(),
            kcp,
        )
        .await;
        let player = server.players.seed("expired");
        let token = server
            .tokens
            .create(player.id(), Duration::from_secs(1))
            .await
            .unwrap();
        let mut client = TestClient::connect_transport(server.addr, kcp).await;
        client
            .send(&InboundMessage::Bind(pb::BindRequest { token }))
            .await;
        assert!(
            matches!(client.recv_expect("bind").await, OutboundMessage::BindResult(result) if result.ok)
        );
        tokio::time::sleep(Duration::from_millis(1100)).await;
        client
            .send(&InboundMessage::JoinRoom(pb::JoinRoomRequest {
                room_id: None,
            }))
            .await;
        assert!(
            matches!(client.recv_expect("rejected expired session").await, OutboundMessage::Error(error) if error.code == longshipx_protocol::ERR_NOT_AUTHENTICATED)
        );
        kcp::stop(server).await;
    }
}

#[tokio::test]
async fn revoked_session_cannot_receive_new_room_broadcasts() {
    for kcp in [false, true] {
        let tokens = Arc::new(InMemoryTokenStore::new());
        let server = spawn_server_transport(
            test_config("127.0.0.1:0".parse().unwrap()),
            tokens.clone(),
            tokens,
            build_router(),
            kcp,
        )
        .await;
        let player = server.players.seed("revoked-broadcast");
        let token = server
            .tokens
            .create(player.id(), Duration::from_secs(60))
            .await
            .unwrap();
        let mut client = TestClient::connect_transport(server.addr, kcp).await;
        client
            .send(&InboundMessage::Bind(pb::BindRequest { token }))
            .await;
        let _ = client.recv_expect("bind").await;
        client
            .send(&InboundMessage::JoinRoom(pb::JoinRoomRequest {
                room_id: None,
            }))
            .await;
        let OutboundMessage::RoomEvent(joined) = client.recv_expect("join").await else {
            panic!("join event");
        };
        let Some(pb::room_event_notification::Event::MemberJoined(joined)) = joined.event else {
            panic!("joined");
        };
        let room_id = longshipx_domain::RoomId(uuid::Uuid::parse_str(&joined.room_id).unwrap());
        server.tokens.revoke_player(player.id()).await.unwrap();
        server
            .deps
            .rooms
            .chat(room_id, player.id(), "must not be delivered".into())
            .await
            .unwrap();
        let reply = tokio::time::timeout(
            Duration::from_secs(4),
            read_frame(&mut client.reader, 65_536),
        )
        .await
        .unwrap();
        assert!(
            !matches!(reply, Ok(Some(frame)) if matches!(ClientCodec.decode(&frame), Ok(OutboundMessage::RoomEvent(_)))),
            "revoked connection still receives room data"
        );
        kcp::stop(server).await;
    }
}

#[tokio::test]
async fn session_revocation_is_player_scoped_and_releases_connections() {
    for kcp in [false, true] {
        let tokens = Arc::new(InMemoryTokenStore::new());
        let server = spawn_server_transport(
            test_config("127.0.0.1:0".parse().unwrap()),
            tokens.clone(),
            tokens,
            build_router(),
            kcp,
        )
        .await;
        let (bind, _) = make_player_with_token(&server, "revoked-shared").await;
        let id = server.tokens.resolve(&bind.token).await.unwrap().unwrap();
        let (other, _) = make_player_with_token(&server, "unrelated").await;
        let mut alice = TestClient::connect_transport(server.addr, kcp).await;
        let mut second = TestClient::connect_transport(server.addr, kcp).await;
        let mut bob = TestClient::connect_transport(server.addr, kcp).await;
        for (client, request) in [
            (&mut alice, bind.clone()),
            (&mut second, bind),
            (&mut bob, other),
        ] {
            client.send(&InboundMessage::Bind(request)).await;
            client.recv_expect("bind").await;
        }
        server.tokens.revoke_player(id).await.unwrap();
        for client in [&mut alice, &mut second] {
            client
                .send(&InboundMessage::GetProfile(pb::GetProfileRequest {}))
                .await;
            assert!(
                matches!(client.recv_expect("rejected").await, OutboundMessage::Error(e) if e.code == longshipx_protocol::ERR_NOT_AUTHENTICATED)
            );
        }
        bob.send(&InboundMessage::GetProfile(pb::GetProfileRequest {}))
            .await;
        assert!(matches!(bob.recv_expect("profile").await, OutboundMessage::Profile(p) if p.ok));
        tokio::time::timeout(Duration::from_secs(1), async {
            while server.deps.connections.active_count() != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        kcp::stop(server).await;
    }
}

#[tokio::test]
async fn session_store_errors_and_stalls_fail_closed() {
    use longshipx_application::{AppError, ports::SessionTokenStore};
    use longshipx_domain::PlayerId;
    use std::sync::atomic::{AtomicBool, Ordering};
    struct FaultStore {
        inner: Arc<InMemoryTokenStore>,
        fail: AtomicBool,
        stall: bool,
    }
    #[async_trait::async_trait]
    impl SessionTokenStore for FaultStore {
        async fn create(&self, id: PlayerId, ttl: Duration) -> Result<String, AppError> {
            self.inner.create(id, ttl).await
        }
        async fn revoke_player(&self, id: PlayerId) -> Result<(), AppError> {
            self.inner.revoke_player(id).await
        }
        async fn resolve(&self, token: &str) -> Result<Option<PlayerId>, AppError> {
            if self.fail.load(Ordering::SeqCst) {
                if self.stall {
                    std::future::pending::<()>().await;
                }
                return Err(AppError::Storage("unavailable".into()));
            }
            self.inner.resolve(token).await
        }
    }
    for kcp in [false, true] {
        for stall in [false, true] {
            let tokens = Arc::new(InMemoryTokenStore::new());
            let store = Arc::new(FaultStore {
                inner: tokens.clone(),
                fail: AtomicBool::new(false),
                stall,
            });
            let mut config = test_config("127.0.0.1:0".parse().unwrap());
            config.heartbeat_timeout = Duration::from_millis(300);
            let server =
                spawn_server_transport(config, tokens, store.clone(), build_router(), kcp).await;
            let (bind, _) = make_player_with_token(&server, "store-failure").await;
            let mut client = TestClient::connect_transport(server.addr, kcp).await;
            client.send(&InboundMessage::Bind(bind)).await;
            client.recv_expect("bind").await;
            store.fail.store(true, Ordering::SeqCst);
            client
                .send(&InboundMessage::JoinRoom(pb::JoinRoomRequest {
                    room_id: None,
                }))
                .await;
            assert!(
                matches!(client.recv_expect("rejected").await, OutboundMessage::Error(e) if e.code == longshipx_protocol::ERR_NOT_AUTHENTICATED)
            );
            assert_eq!(server.deps.rooms.active_rooms().await, 0);
            server.shutdown_tx.send_replace(true);
        }
    }
}

#[tokio::test]
async fn invalid_bind_cannot_extend_authentication_deadline() {
    let mut config = test_config("127.0.0.1:0".parse().unwrap());
    config.unauth_timeout = Duration::from_millis(200);
    let server = spawn_server_with(config).await;
    let mut client = TestClient::connect(server.addr).await;
    let mut expired = false;
    for _ in 0..6 {
        client
            .send(&InboundMessage::Bind(pb::BindRequest {
                token: "invalid".into(),
            }))
            .await;
        if matches!(client.recv_expect("bind or timeout").await, OutboundMessage::Error(e) if e.code == longshipx_protocol::ERR_TIMEOUT)
        {
            expired = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(75)).await;
    }
    server.shutdown_tx.send_replace(true);
    assert!(
        expired,
        "invalid Bind keeps the unauthenticated connection alive beyond its lifetime"
    );
}

#[tokio::test]
async fn stalled_handshakes_release_admission_workers() {
    let mut config = test_config("127.0.0.1:0".parse().unwrap());
    config.accept_concurrency = 2;
    config.unauth_timeout = Duration::from_millis(150);
    let server = spawn_server_with(config).await;
    let first = TcpStream::connect(server.addr).await.unwrap();
    let second = TcpStream::connect(server.addr).await.unwrap();
    tokio::time::sleep(Duration::from_millis(250)).await;
    let healthy =
        tokio::time::timeout(Duration::from_millis(500), TestClient::connect(server.addr)).await;
    drop((first, second));
    server.shutdown_tx.send_replace(true);
    assert!(
        healthy.is_ok(),
        "silent TLS peers monopolize admission workers"
    );
}

#[tokio::test]
async fn tls_admission_counts_raw_sockets_and_recovers_after_timeout() {
    let mut config = test_config("127.0.0.1:0".parse().unwrap());
    config.max_connections = 2;
    config.unauth_timeout = Duration::from_millis(150);
    let server = spawn_server_with(config).await;
    let first = TcpStream::connect(server.addr).await.unwrap();
    let second = TcpStream::connect(server.addr).await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while server.deps.auth_gate.unauth_count(server.addr.ip()) != 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let mut excess = TcpStream::connect(server.addr).await.unwrap();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), excess.read_u8()).await,
        Ok(Err(_))
    ));
    tokio::time::sleep(Duration::from_millis(180)).await;
    assert_eq!(server.deps.auth_gate.unauth_count(server.addr.ip()), 0);
    let client = TestClient::connect(server.addr).await;
    drop((first, second, client));
    server.shutdown_tx.send_replace(true);
}

#[tokio::test]
async fn slow_bind_lookup_cannot_outlive_authentication_deadline() {
    use longshipx_application::{AppError, ports::SessionTokenStore};
    struct Stalled;
    #[async_trait::async_trait]
    impl SessionTokenStore for Stalled {
        async fn create(
            &self,
            _: longshipx_domain::PlayerId,
            _: Duration,
        ) -> Result<String, AppError> {
            unreachable!()
        }
        async fn revoke_player(&self, _: longshipx_domain::PlayerId) -> Result<(), AppError> {
            unreachable!()
        }
        async fn resolve(&self, _: &str) -> Result<Option<longshipx_domain::PlayerId>, AppError> {
            std::future::pending().await
        }
    }
    let mut config = test_config("127.0.0.1:0".parse().unwrap());
    config.unauth_timeout = Duration::from_millis(150);
    let server = spawn_server_with_tokens(
        config,
        Arc::new(InMemoryTokenStore::new()),
        Arc::new(Stalled),
    )
    .await;
    let mut client = TestClient::connect(server.addr).await;
    client
        .send(&InboundMessage::Bind(pb::BindRequest {
            token: "slow".into(),
        }))
        .await;
    assert!(
        matches!(client.recv_expect("auth timeout").await, OutboundMessage::Error(e) if e.code == longshipx_protocol::ERR_TIMEOUT)
    );
    tokio::time::timeout(Duration::from_secs(1), async {
        while server.deps.auth_gate.unauth_count(server.addr.ip()) != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    server.shutdown_tx.send_replace(true);
}

#[tokio::test]
async fn both_queue_overloads_disconnect_only_the_affected_room_member() {
    use longshipx_gateway::tcp::handlers::handle_heartbeat;
    use longshipx_protocol::opcodes::OP_C2S_HEARTBEAT;
    for kcp in [false, true] {
        for room_queue in [true, false] {
            let mut router = build_router();
            router.route(
                OP_C2S_HEARTBEAT,
                move |ctx: longshipx_gateway::tcp::ConnContext, message| async move {
                    if ctx.authed_player().unwrap().nickname == "overload" {
                        // No yield: fill the real queue before its consumer can run on this runtime.
                        for _ in 0..65 {
                            if room_queue {
                                ctx.room_tx
                                    .try_send(longshipx_application::RoomEvent::Closed {
                                        room_id: ctx.current_room().unwrap(),
                                        reason: "test overload".into(),
                                    });
                            } else {
                                let _ = ctx
                                    .outbound
                                    .try_send_frame(longshipx_net_kit::Frame::new(1, vec![]));
                            }
                        }
                        Ok(None)
                    } else {
                        handle_heartbeat(ctx, message).await
                    }
                },
            );
            let tokens = Arc::new(InMemoryTokenStore::new());
            let server = spawn_server_transport(
                test_config("127.0.0.1:0".parse().unwrap()),
                tokens.clone(),
                tokens,
                router,
                kcp,
            )
            .await;
            let mut victim = TestClient::connect_transport(server.addr, kcp).await;
            let (bind, _) = make_player_with_token(&server, "overload").await;
            victim.send(&InboundMessage::Bind(bind)).await;
            victim.recv_expect("bind").await;
            victim
                .send(&InboundMessage::JoinRoom(pb::JoinRoomRequest {
                    room_id: None,
                }))
                .await;
            let OutboundMessage::RoomEvent(joined) = victim.recv_expect("join").await else {
                panic!("missing join");
            };
            let Some(pb::room_event_notification::Event::MemberJoined(joined)) = joined.event
            else {
                panic!("missing member join");
            };
            let mut healthy = TestClient::connect_transport(server.addr, kcp).await;
            let (bind, _) = make_player_with_token(&server, "healthy").await;
            healthy.send(&InboundMessage::Bind(bind)).await;
            healthy.recv_expect("bind").await;
            healthy
                .send(&InboundMessage::JoinRoom(pb::JoinRoomRequest {
                    room_id: Some(joined.room_id),
                }))
                .await;
            healthy.recv_expect("join").await;
            victim.recv_expect("peer join").await;
            let trigger = ClientCodec
                .encode(&InboundMessage::Heartbeat(pb::HeartbeatPing {
                    client_ts_ms: 0,
                }))
                .unwrap();
            // Forced disconnect may precede the transport ACK; do not wait for a flush.
            victim
                .writer
                .write_all(&longshipx_net_kit::encode_frame(&trigger, 65_536).unwrap())
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(2), async {
                while server.deps.connections.active_count() != 1 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            let left = healthy.recv_expect("departed overload member").await;
            assert!(matches!(
                left,
                OutboundMessage::RoomEvent(pb::RoomEventNotification {
                    event: Some(pb::room_event_notification::Event::MemberLeft(_)),
                    ..
                })
            ));
            assert_eq!(server.deps.auth_gate.unauth_count(server.addr.ip()), 0);
            healthy
                .send(&InboundMessage::Heartbeat(pb::HeartbeatPing {
                    client_ts_ms: 0,
                }))
                .await;
            assert!(matches!(
                healthy.recv_expect("healthy heartbeat").await,
                OutboundMessage::HeartbeatAck(_)
            ));
            let end = tokio::time::timeout(
                Duration::from_secs(4),
                read_frame(&mut victim.reader, 65_536),
            )
            .await
            .unwrap_or_else(|_| panic!("overload EOF timeout: kcp={kcp} room_queue={room_queue}"));
            assert!(
                matches!(end, Ok(None) | Err(_)),
                "overloaded TLS connection remained readable"
            );
            drop(victim);
            drop(healthy);
            tokio::time::timeout(Duration::from_secs(4), async {
                while server.deps.connections.active_count() != 0 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap_or_else(|_| {
                panic!("overload cleanup timeout: kcp={kcp} room_queue={room_queue}")
            });
            server.shutdown_tx.send_replace(true);
            server.handle.await.unwrap();
        }
    }
}

#[path = "e2e/room_membership.rs"]
mod room_membership;

#[path = "e2e/kcp.rs"]
mod kcp;

#[path = "e2e/kcp_network.rs"]
mod kcp_network;

#[path = "e2e/kcp_blackbox.rs"]
mod kcp_blackbox;

#[path = "e2e/kcp_stress.rs"]
mod kcp_stress;

fn kcp_settings() -> longshipx_net_kit::kcp::KcpSettings {
    longshipx_net_kit::kcp::KcpSettings {
        mtu: 1200,
        window: 64,
        interval: Duration::from_millis(10),
        idle_timeout: Duration::from_secs(2),
        max_sessions: 64,
        queue_capacity: 256,
    }
}
