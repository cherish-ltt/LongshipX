use super::*;

pub(super) async fn server(config: TcpGatewayConfig) -> TestServer {
    let tokens = Arc::new(InMemoryTokenStore::new());
    spawn_server_transport(config, tokens.clone(), tokens, build_router(), true).await
}

async fn bound(server: &TestServer, name: &str) -> (TestClient, pb::BindRequest) {
    let (bind, _) = make_player_with_token(server, name).await;
    let mut client = TestClient::connect_transport(server.addr, true).await;
    client.send(&InboundMessage::Bind(bind.clone())).await;
    assert!(
        matches!(client.recv_expect("bind").await, OutboundMessage::BindResult(reply) if reply.ok)
    );
    (client, bind)
}

pub(super) async fn closed(client: &mut TestClient) {
    tokio::time::timeout(Duration::from_secs(4), async {
        while let Ok(Some(frame)) = read_frame(&mut client.reader, 65_536).await {
            assert!(!matches!(
                ClientCodec.decode(&frame),
                Ok(OutboundMessage::Profile(_))
            ));
        }
    })
    .await
    .expect("connection must close");
}

pub(super) async fn stop(server: TestServer) {
    server.shutdown_tx.send_replace(true);
    tokio::time::timeout(Duration::from_secs(3), server.handle)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(server.deps.connections.active_count(), 0);
    assert_eq!(server.deps.auth_gate.unauth_count(server.addr.ip()), 0);
}

#[tokio::test]
async fn mixed_transport_room_profile_heartbeat_chat_leave_and_shutdown() {
    let server = spawn_server().await;
    let (cert, key) = test_certs();
    let acceptor = longshipx_net_kit::tls::server_acceptor_from_pem_bytes(cert, key).unwrap();
    let kcp = TcpGateway::bind_kcp(
        test_config("127.0.0.1:0".parse().unwrap()),
        kcp_settings(),
        acceptor,
        server.deps.clone(),
    )
    .await
    .unwrap();
    let addr = kcp.local_addr().unwrap();
    let shutdown = server.shutdown_tx.subscribe();
    let kcp_task = tokio::spawn(async move { kcp.run(shutdown).await });
    let (alice_bind, _) = make_player_with_token(&server, "tcp-player").await;
    let (bob_bind, _) = make_player_with_token(&server, "kcp-player").await;
    let mut alice = TestClient::connect(server.addr).await;
    let mut bob = TestClient::connect_transport(addr, true).await;
    for (client, bind) in [(&mut alice, alice_bind), (&mut bob, bob_bind)] {
        client.send(&InboundMessage::Bind(bind)).await;
        assert!(matches!(client.recv_expect("bind").await, OutboundMessage::BindResult(r) if r.ok));
        client
            .send(&InboundMessage::GetProfile(pb::GetProfileRequest {}))
            .await;
        assert!(matches!(client.recv_expect("profile").await, OutboundMessage::Profile(p) if p.ok));
        client
            .send(&InboundMessage::Heartbeat(pb::HeartbeatPing {
                client_ts_ms: 1,
            }))
            .await;
        assert!(matches!(
            client.recv_expect("heartbeat").await,
            OutboundMessage::HeartbeatAck(_)
        ));
    }
    alice
        .send(&InboundMessage::JoinRoom(pb::JoinRoomRequest {
            room_id: None,
        }))
        .await;
    let OutboundMessage::RoomEvent(event) = alice.recv_expect("room").await else {
        panic!("room");
    };
    let Some(pb::room_event_notification::Event::MemberJoined(join)) = event.event else {
        panic!("join");
    };
    bob.send(&InboundMessage::JoinRoom(pb::JoinRoomRequest {
        room_id: Some(join.room_id.clone()),
    }))
    .await;
    bob.recv_expect("join").await;
    alice.recv_expect("join").await;
    for from_kcp in [false, true] {
        let sender = if from_kcp { &mut bob } else { &mut alice };
        sender
            .send(&InboundMessage::RoomChat(pb::RoomChatRequest {
                text: "cross-transport".into(),
            }))
            .await;
        for client in [&mut alice, &mut bob] {
            assert!(
                matches!(client.recv_expect("chat").await, OutboundMessage::RoomEvent(e)
                if matches!(&e.event, Some(pb::room_event_notification::Event::Chat(c)) if c.text == "cross-transport"))
            );
        }
    }
    bob.send(&InboundMessage::LeaveRoom(pb::LeaveRoomRequest {
        room_id: Some(join.room_id),
    }))
    .await;
    assert!(matches!(
        alice.recv_expect("leave").await,
        OutboundMessage::RoomEvent(_)
    ));
    server.shutdown_tx.send_replace(true);
    tokio::time::timeout(Duration::from_secs(3), kcp_task)
        .await
        .unwrap()
        .unwrap();
    stop(server).await;
}

#[tokio::test]
async fn invalid_expired_and_revoked_tokens_fail_closed() {
    let server = server(test_config("127.0.0.1:0".parse().unwrap())).await;
    let mut invalid = TestClient::connect_transport(server.addr, true).await;
    invalid
        .send(&InboundMessage::Bind(pb::BindRequest {
            token: "invalid".into(),
        }))
        .await;
    assert!(
        matches!(invalid.recv_expect("invalid bind").await, OutboundMessage::BindResult(r) if !r.ok)
    );
    drop(invalid);
    let (mut client, bind) = bound(&server, "revoked").await;
    let player = server.tokens.resolve(&bind.token).await.unwrap().unwrap();
    server.tokens.revoke_player(player).await.unwrap();
    client
        .send(&InboundMessage::GetProfile(pb::GetProfileRequest {}))
        .await;
    closed(&mut client).await;
    let player = server.players.seed("expired");
    let token = server
        .tokens
        .create(player.id(), Duration::ZERO)
        .await
        .unwrap();
    let mut expired = TestClient::connect_transport(server.addr, true).await;
    expired
        .send(&InboundMessage::Bind(pb::BindRequest { token }))
        .await;
    assert!(
        matches!(expired.recv_expect("expired bind").await, OutboundMessage::BindResult(r) if !r.ok)
    );
    stop(server).await;
}

#[tokio::test]
async fn authentication_first_frame_limits_and_rate_limit() {
    for case in 0..3 {
        let mut config = test_config("127.0.0.1:0".parse().unwrap());
        if case == 2 {
            config.rate = RateLimitSettings {
                enabled: true,
                per_second: 1,
                burst: 1,
            };
        }
        let server = server(config).await;
        let mut client = TestClient::connect_transport(server.addr, true).await;
        if case == 0 {
            client
                .send(&InboundMessage::Heartbeat(pb::HeartbeatPing {
                    client_ts_ms: 1,
                }))
                .await;
        } else if case == 1 {
            client
                .writer
                .write_all(&70_000u32.to_be_bytes())
                .await
                .unwrap();
            client.writer.flush().await.unwrap();
        } else {
            let (bind, _) = make_player_with_token(&server, "limited").await;
            client.send(&InboundMessage::Bind(bind)).await;
            client.recv_expect("bind").await;
            client
                .send(&InboundMessage::Heartbeat(pb::HeartbeatPing {
                    client_ts_ms: 1,
                }))
                .await;
        }
        closed(&mut client).await;
        stop(server).await;
    }
}

#[tokio::test]
async fn no_plaintext_tls_certificate_validation_and_port_conflict() {
    let server = server(test_config("127.0.0.1:0".parse().unwrap())).await;
    assert!(
        longshipx_net_kit::kcp::KcpTransport::bind(server.addr, kcp_settings())
            .await
            .is_err()
    );
    let mut raw = longshipx_net_kit::kcp::KcpStream::connect(server.addr, kcp_settings())
        .await
        .unwrap();
    let frame = ClientCodec
        .encode(&InboundMessage::Bind(pb::BindRequest {
            token: "must-not-authenticate".into(),
        }))
        .unwrap();
    raw.write_all(&longshipx_net_kit::encode_frame(&frame, 65_536).unwrap())
        .await
        .unwrap();
    let mut bytes = [0; 64];
    let result = tokio::time::timeout(Duration::from_secs(3), raw.read(&mut bytes)).await;
    assert!(matches!(result, Ok(Ok(0)) | Ok(Err(_))));
    let roots = rustls::RootCertStore::empty();
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .unwrap()
    .with_root_certificates(roots)
    .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
    let raw = longshipx_net_kit::kcp::KcpStream::connect(server.addr, kcp_settings())
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(
            Duration::from_secs(3),
            connector.connect("localhost".try_into().unwrap(), raw)
        )
        .await
        .unwrap()
        .is_err()
    );
    stop(server).await;
}

#[tokio::test]
async fn repeated_connections_and_auth_deadlines_release_resources() {
    let mut config = test_config("127.0.0.1:0".parse().unwrap());
    config.unauth_timeout = Duration::from_millis(300);
    config.max_connections = 2;
    let server = server(config).await;
    for _ in 0..6 {
        let mut client = TestClient::connect_transport(server.addr, true).await;
        closed(&mut client).await;
        assert_eq!(server.deps.auth_gate.unauth_count(server.addr.ip()), 0);
    }
    let (mut client, _) = bound(&server, "recovered").await;
    client
        .send(&InboundMessage::Heartbeat(pb::HeartbeatPing {
            client_ts_ms: 1,
        }))
        .await;
    assert!(matches!(
        client.recv_expect("heartbeat").await,
        OutboundMessage::HeartbeatAck(_)
    ));
    stop(server).await;
}

#[tokio::test]
async fn tcp_and_kcp_share_global_and_single_ip_admission_limits() {
    tokio::time::timeout(Duration::from_secs(15), async {
        for global_limit in [true, false] {
            let occupied = if global_limit { 1 } else { 8 };
            let mut config = test_config("127.0.0.1:0".parse().unwrap());
            config.max_connections = if global_limit { 1 } else { 32 };
            config.accept_concurrency = 16;
            config.unauth_timeout = Duration::from_secs(5);
            let server = spawn_server_with(config).await;
            let (cert, key) = test_certs();
            let acceptor = longshipx_net_kit::tls::server_acceptor_from_pem_bytes(cert, key).unwrap();
            let gateway = TcpGateway::bind_kcp(config, kcp_settings(), acceptor, server.deps.clone()).await.unwrap();
            let addr = gateway.local_addr().unwrap();
            let shutdown = server.shutdown_tx.subscribe();
            let worker = tokio::spawn(async move { gateway.run(shutdown).await });
            let mut sockets = Vec::new();
            for _ in 0..occupied { sockets.push(TcpStream::connect(server.addr).await.unwrap()); }
            tokio::time::timeout(Duration::from_secs(1), async {
                while server.deps.auth_gate.unauth_count(addr.ip()) != occupied {
                    tokio::task::yield_now().await;
                }
            }).await.unwrap();
            let mut rejected = longshipx_net_kit::kcp::KcpStream::connect(addr, kcp_settings()).await.unwrap();
            rejected.write_all(b"rejected before TLS").await.unwrap();
            assert!(rejected.flush().await.is_err());
            assert_eq!(server.deps.auth_gate.unauth_count(addr.ip()), occupied);
            assert_eq!(server.deps.connections.active_count(), 0);
            drop(sockets);
            tokio::time::timeout(Duration::from_secs(1), async {
                while server.deps.auth_gate.unauth_count(addr.ip()) != 0 {
                    tokio::task::yield_now().await;
                }
            }).await.unwrap();
            let mut client = TestClient::connect_transport(addr, true).await;
            let (bind, _) = make_player_with_token(&server, "recovered-shared-limit").await;
            client.send(&InboundMessage::Bind(bind)).await;
            assert!(matches!(client.recv_expect("recovered bind").await, OutboundMessage::BindResult(r) if r.ok));
            server.shutdown_tx.send_replace(true);
            worker.await.unwrap();
            stop(server).await;
        }
    }).await.unwrap();
}
