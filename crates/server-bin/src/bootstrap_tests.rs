use super::*;
use longshipx_net_kit::transport::ConnectionIo;

fn certificates() -> (Arc<rustls::ServerConfig>, tokio_rustls::TlsConnector) {
    let key = rcgen::KeyPair::generate().unwrap();
    let cert = rcgen::CertificateParams::new(vec!["localhost".into()])
        .unwrap()
        .self_signed(&key)
        .unwrap();
    let server =
        longshipx_net_kit::tls::server_config_from_pem_bytes(&cert.pem(), &key.serialize_pem())
            .unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert.der().clone()).unwrap();
    let client = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .unwrap()
    .with_root_certificates(roots)
    .with_no_client_auth();
    (server, tokio_rustls::TlsConnector::from(Arc::new(client)))
}

#[tokio::test]
async fn configured_tcp_kcp_and_dual_listeners_handshake_and_stop() {
    tokio::time::timeout(Duration::from_secs(10), async {
        for (tcp, kcp) in [(true, false), (false, true), (true, true)] {
            let mut config = Config::from_lookup(|_| None).unwrap();
            config.network.tcp_enabled = tcp;
            config.network.kcp_enabled = kcp;
            config.network.tcp_bind_addr = "127.0.0.1:0".into();
            config.network.kcp_bind_addr = "127.0.0.1:0".into();
            config.validate().unwrap();
            // Repositories are constructed, but this transport smoke test never queries a database.
            let services = build_services(
                &config,
                sea_orm::DatabaseConnection::default(),
                in_memory_tokens(),
            )
            .unwrap();
            let (tls, client) = certificates();
            let (shutdown, rx) = watch::channel(false);
            let running = start_game_gateways(&config, &services, tls, rx)
                .await
                .unwrap();
            assert_eq!(running.len(), usize::from(tcp) + usize::from(kcp));
            for (index, (gateway, _)) in running.iter().enumerate() {
                let addr = gateway.local_addr().unwrap();
                let raw: Box<dyn ConnectionIo> = if tcp && index == 0 {
                    Box::new(tokio::net::TcpStream::connect(addr).await.unwrap())
                } else {
                    Box::new(
                        longshipx_net_kit::kcp::KcpStream::connect(
                            addr,
                            longshipx_net_kit::kcp::KcpSettings {
                                mtu: config.network.kcp_mtu,
                                window: config.network.kcp_window,
                                interval: Duration::from_millis(config.network.kcp_interval_ms),
                                idle_timeout: Duration::from_secs(
                                    config.network.kcp_idle_timeout_secs,
                                ),
                                max_sessions: config.network.kcp_max_sessions,
                                queue_capacity: config.network.kcp_queue_capacity,
                            },
                        )
                        .await
                        .unwrap(),
                    )
                };
                let _tls = client
                    .connect("localhost".try_into().unwrap(), raw)
                    .await
                    .unwrap();
            }
            shutdown.send_replace(true);
            for (_, task) in running {
                task.await.unwrap();
            }
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn udp_bind_failure_does_not_leave_partial_tcp_listener() {
    let reserved_tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let tcp_addr = reserved_tcp.local_addr().unwrap();
    let udp = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let mut config = Config::from_lookup(|_| None).unwrap();
    config.network.kcp_enabled = true;
    config.network.tcp_bind_addr = tcp_addr.to_string();
    config.network.kcp_bind_addr = udp.local_addr().unwrap().to_string();
    let services = build_services(
        &config,
        sea_orm::DatabaseConnection::default(),
        in_memory_tokens(),
    )
    .unwrap();
    let (tls, _) = certificates();
    let (_shutdown, rx) = watch::channel(false);
    drop(reserved_tcp);
    assert!(
        start_game_gateways(&config, &services, tls, rx)
            .await
            .is_err()
    );
    let _rebound = tokio::net::TcpListener::bind(tcp_addr).await.unwrap();
}
