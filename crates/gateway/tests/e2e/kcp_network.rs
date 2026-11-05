use super::*;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::net::UdpSocket;

pub(super) struct Proxy {
    pub(super) addr: SocketAddr,
    task: tokio::task::JoinHandle<()>,
    packets: Arc<parking_lot::Mutex<Vec<Vec<u8>>>>,
    corrupt: Arc<AtomicBool>,
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn proxy(server: SocketAddr, corrupt: bool) -> Proxy {
    udp_proxy(server, corrupt, true, 10).await
}

pub(super) async fn udp_proxy(
    server: SocketAddr,
    corrupt: bool,
    impair: bool,
    delay_ms: u64,
) -> Proxy {
    let front = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let back = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    back.connect(server).await.unwrap();
    let addr = front.local_addr().unwrap();
    let packets = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let captured = packets.clone();
    let corrupt = Arc::new(AtomicBool::new(corrupt));
    let corrupt_packets = corrupt.clone();
    let task = tokio::spawn(async move {
        let mut client = None;
        let mut a = [0; 1500];
        let mut b = [0; 1500];
        let mut counter = [0usize; 2];
        let mut delayed = Vec::<(tokio::time::Instant, bool, Vec<u8>)>::new();
        let mut tick = tokio::time::interval(Duration::from_millis(2));
        loop {
            let (to_server, bytes) = tokio::select! {
                result = front.recv_from(&mut a) => {
                    let (len, peer) = result.unwrap();
                    client = Some(peer);
                    (true, a[..len].to_vec())
                },
                result = back.recv(&mut b) => (false, b[..result.unwrap()].to_vec()),
                _ = tick.tick() => {
                    let now = tokio::time::Instant::now();
                    let mut i = 0;
                    while i < delayed.len() {
                        if delayed[i].0 <= now {
                            let (_, to_server, bytes) = delayed.swap_remove(i);
                            if to_server { back.send(&bytes).await.unwrap(); }
                            else if let Some(peer) = client { front.send_to(&bytes, peer).await.unwrap(); }
                        } else { i += 1; }
                    }
                    continue;
                }
            };
            if to_server {
                let mut capture = captured.lock();
                if capture.len() < 4096 {
                    capture.push(bytes.clone());
                }
            }
            let direction = usize::from(to_server);
            counter[direction] += 1;
            let n = counter[direction];
            let mut bytes = bytes;
            if corrupt_packets.load(Ordering::Relaxed) && to_server && bytes.len() > 28 {
                let last = bytes.len() - 1;
                bytes[last] ^= 0x80;
            }
            if impair && n % 11 == 0 {
                continue;
            }
            let delay = if impair && n % 7 == 0 {
                delay_ms + 25
            } else {
                delay_ms
            };
            assert!(delayed.len() < 2048, "proxy queue must remain bounded");
            delayed.push((
                tokio::time::Instant::now() + Duration::from_millis(delay),
                to_server,
                bytes.clone(),
            ));
            if impair && n % 13 == 0 {
                delayed.push((
                    tokio::time::Instant::now() + Duration::from_millis(delay + 2),
                    to_server,
                    bytes,
                ));
            }
        }
    });
    Proxy {
        addr,
        task,
        packets,
        corrupt,
    }
}

#[tokio::test]
async fn captured_tls_records_cannot_authenticate_a_new_session() {
    let server = super::kcp::server(test_config("127.0.0.1:0".parse().unwrap())).await;
    let proxy = proxy(server.addr, false).await;
    let (bind, _) = make_player_with_token(&server, "replay").await;
    let mut client = TestClient::connect_transport(proxy.addr, true).await;
    client.send(&InboundMessage::Bind(bind)).await;
    assert!(
        matches!(client.recv_expect("original bind").await, OutboundMessage::BindResult(r) if r.ok)
    );
    let packets = proxy.packets.lock().clone();
    drop(client);
    drop(proxy);
    tokio::time::timeout(Duration::from_secs(6), async {
        while server.deps.connections.active_count() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    for mut packet in packets {
        packet[..4].copy_from_slice(&0x12345678u32.to_be_bytes());
        socket.send_to(&packet, server.addr).await.unwrap();
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        server.deps.connections.active_count(),
        0,
        "replayed handshake must not create a TLS-authenticated connection"
    );
    super::kcp::stop(server).await;
}

#[tokio::test]
async fn lossy_reordered_duplicate_udp_preserves_tls_and_business_messages() {
    let server = super::kcp::server(test_config("127.0.0.1:0".parse().unwrap())).await;
    let proxy = proxy(server.addr, false).await;
    let (bind, _) = make_player_with_token(&server, "weak-network").await;
    let token = bind.token.clone();
    let mut client = TestClient::connect_transport(proxy.addr, true).await;
    client.send(&InboundMessage::Bind(bind)).await;
    assert!(
        matches!(client.recv_expect("bind over weak network").await, OutboundMessage::BindResult(r) if r.ok)
    );
    for n in 0..20 {
        client
            .send(&InboundMessage::Heartbeat(pb::HeartbeatPing {
                client_ts_ms: n,
            }))
            .await;
        assert!(matches!(
            client.recv_expect("ordered heartbeat").await,
            OutboundMessage::HeartbeatAck(_)
        ));
    }
    assert!(!proxy.packets.lock().iter().any(|packet| {
        packet
            .windows(token.len())
            .any(|part| part == token.as_bytes())
    }));
    super::kcp::stop(server).await;
}

#[tokio::test]
async fn tampered_tls_payload_never_authenticates() {
    let mut settings = test_config("127.0.0.1:0".parse().unwrap());
    settings.unauth_timeout = Duration::from_millis(600);
    let server = super::kcp::server(settings).await;
    let proxy = proxy(server.addr, true).await;
    let raw = longshipx_net_kit::kcp::KcpStream::connect(proxy.addr, kcp_settings())
        .await
        .unwrap();
    let mut roots = rustls::RootCertStore::empty();
    for cert in rustls::pki_types::CertificateDer::pem_slice_iter(test_certs().0.as_bytes()) {
        roots.add(cert.unwrap()).unwrap();
    }
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .unwrap()
    .with_root_certificates(roots)
    .with_no_client_auth();
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        tokio_rustls::TlsConnector::from(Arc::new(config))
            .connect("localhost".try_into().unwrap(), raw),
    )
    .await;
    assert!(matches!(result, Ok(Err(_))));
    assert_eq!(server.deps.connections.active_count(), 0);
    super::kcp::stop(server).await;
}

#[tokio::test]
async fn encrypted_application_record_tampering_closes_bound_session() {
    let server = super::kcp::server(test_config("127.0.0.1:0".parse().unwrap())).await;
    let proxy = udp_proxy(server.addr, false, false, 10).await;
    let (bind, _) = make_player_with_token(&server, "integrity").await;
    let mut client = TestClient::connect_transport(proxy.addr, true).await;
    client.send(&InboundMessage::Bind(bind)).await;
    assert!(
        matches!(client.recv_expect("bind before tampering").await, OutboundMessage::BindResult(r) if r.ok)
    );
    proxy.corrupt.store(true, Ordering::Relaxed);
    let frame = ClientCodec
        .encode(&InboundMessage::GetProfile(pb::GetProfileRequest {}))
        .unwrap();
    client
        .writer
        .write_all(&longshipx_net_kit::encode_frame(&frame, 65_536).unwrap())
        .await
        .unwrap();
    super::kcp::closed(&mut client).await;
    tokio::time::timeout(Duration::from_secs(1), async {
        while server.deps.connections.active_count() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    super::kcp::stop(server).await;
}
