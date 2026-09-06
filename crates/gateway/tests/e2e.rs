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
}

fn test_config(addr: std::net::SocketAddr) -> TcpGatewayConfig {
    TcpGatewayConfig {
        bind_addr: addr,
        max_frame_size: 65_536,
        send_queue_capacity: 64,
        room_event_capacity: 64,
        unauth_timeout: Duration::from_secs(2),
        heartbeat_timeout: Duration::from_secs(5),
        rate: RATE_OFF,
        max_connections: 64,
        backlog: 16,
        nodelay: true,
        keepalive: Duration::from_secs(30),
    }
}

async fn spawn_server() -> TestServer {
    spawn_server_with(test_config("127.0.0.1:0".parse().unwrap())).await
}

async fn spawn_server_with(config: TcpGatewayConfig) -> TestServer {
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
    let deps = Arc::new(GatewayDeps {
        codec: Arc::new(GameCodec),
        router: build_router(),
        tokens: tokens.clone(),
        players: players.clone(),
        profile: Arc::new(longshipx_application::GetPlayerProfile::new(
            players.clone(),
        )),
        rooms,
        auth_gate: Arc::new(AuthGate::new(8)),
        connections: Arc::new(ConnectionRegistry::new()),
    });
    let gateway = TcpGateway::bind(config, acceptor, deps).unwrap();
    let addr = gateway.local_addr().unwrap();
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(async move { gateway.run(shutdown_rx).await });
    TestServer {
        addr,
        shutdown_tx,
        tokens,
        players,
        handle,
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
