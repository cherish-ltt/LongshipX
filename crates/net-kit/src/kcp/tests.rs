use super::*;
use crate::Transport;
use rustls::pki_types::pem::PemObject;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn settings() -> KcpSettings {
    KcpSettings {
        mtu: 1200,
        window: 64,
        interval: Duration::from_millis(10),
        idle_timeout: Duration::from_secs(3),
        max_sessions: 16,
        queue_capacity: 128,
    }
}

fn connector() -> tokio_rustls::TlsConnector {
    let mut roots = rustls::RootCertStore::empty();
    for cert in rustls::pki_types::CertificateDer::pem_slice_iter(
        crate::test_support::cert_pems().0.as_bytes(),
    ) {
        roots.add(cert.unwrap()).unwrap();
    }
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .unwrap()
    .with_root_certificates(roots)
    .with_no_client_auth();
    tokio_rustls::TlsConnector::from(Arc::new(config))
}

#[tokio::test]
async fn tls_large_frame_roundtrip_and_acknowledged_flush() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let listener = KcpTransport::bind("127.0.0.1:0".parse().unwrap(), settings())
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let (cert, key) = crate::test_support::cert_pems();
        let acceptor = crate::tls::server_acceptor_from_pem_bytes(cert, key).unwrap();
        let server = tokio::spawn(async move {
            let (raw, _) = listener.accept().await.unwrap();
            let mut tls = acceptor.accept(raw).await.unwrap();
            let mut data = vec![0; 65_536];
            tls.read_exact(&mut data).await.unwrap();
            assert!(data.iter().all(|byte| *byte == 42));
            tls.write_all(&data).await.unwrap();
            tls.flush().await.unwrap();
        });
        let raw = KcpStream::connect(addr, settings()).await.unwrap();
        let mut tls = connector()
            .connect("localhost".try_into().unwrap(), raw)
            .await
            .unwrap();
        tls.write_all(&vec![42; 65_536]).await.unwrap();
        tls.flush().await.unwrap();
        let mut data = vec![0; 65_536];
        tls.read_exact(&mut data).await.unwrap();
        assert!(data.iter().all(|byte| *byte == 42));
        server.await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn flush_without_peer_fails_at_idle_deadline() {
    let peer = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let mut config = settings();
    config.idle_timeout = Duration::from_millis(100);
    let mut stream = KcpStream::connect(peer.local_addr().unwrap(), config)
        .await
        .unwrap();
    stream.write_all(b"TLS handshake bytes").await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(1), stream.flush())
            .await
            .unwrap()
            .is_err()
    );
    assert!(stream.flush().await.is_err());
}

#[test]
fn invalid_settings_are_rejected() {
    let valid = settings();
    for invalid in [
        KcpSettings { mtu: 0, ..valid },
        KcpSettings { mtu: 1401, ..valid },
        KcpSettings { window: 0, ..valid },
        KcpSettings {
            max_sessions: 0,
            ..valid
        },
        KcpSettings {
            queue_capacity: 0,
            ..valid
        },
        KcpSettings {
            interval: Duration::ZERO,
            ..valid
        },
        KcpSettings {
            idle_timeout: Duration::ZERO,
            ..valid
        },
    ] {
        assert!(invalid.validate().is_err());
    }
}

#[test]
fn packet_boundary_rejects_invalid_and_message_mode_datagrams() {
    use kcp_ovo::helper::KcpPacketHeader;
    let header = KcpPacketHeader::new(1, kcp_ovo::KcpCmd::Wins, 0, 64, 0, 0, 0, 0);
    let packet = header.to_bytes().unwrap();
    assert_eq!(driver::packet_conv(&packet, 1200), Some(1));
    for length in 0..packet.len() {
        assert_eq!(driver::packet_conv(&packet[..length], 1200), None);
    }
    let mut invalid = packet.to_vec();
    invalid[5] = 1;
    assert_eq!(driver::packet_conv(&invalid, 1200), None);
    invalid[5] = 0;
    invalid.extend_from_slice(&[0]);
    assert_eq!(driver::packet_conv(&invalid, 1200), None);
}

#[tokio::test]
async fn session_limit_idle_reclaim_and_listener_shutdown() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut config = settings();
        config.max_sessions = 1;
        config.idle_timeout = Duration::from_millis(200);
        let listener = KcpTransport::bind("127.0.0.1:0".parse().unwrap(), config)
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let first = KcpStream::connect(addr, config).await.unwrap();
        let (server, _) = listener.accept().await.unwrap();
        let second = KcpStream::connect(addr, config).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), listener.accept())
                .await
                .is_err()
        );
        drop(first);
        drop(server);
        drop(second);
        tokio::time::sleep(Duration::from_millis(250)).await;
        let _third = KcpStream::connect(addr, config).await.unwrap();
        let (mut third_server, _) = listener.accept().await.unwrap();
        listener.close().await;
        assert!(listener.accept().await.is_err());
        assert_eq!(third_server.read(&mut [0; 1]).await.unwrap(), 0);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn slow_reader_applies_bounded_backpressure() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut config = settings();
        config.window = 2;
        let listener = KcpTransport::bind("127.0.0.1:0".parse().unwrap(), config)
            .await
            .unwrap();
        let mut client = KcpStream::connect(listener.local_addr().unwrap(), config)
            .await
            .unwrap();
        let (mut server, _) = listener.accept().await.unwrap();
        let data = vec![7; 128 * 1024];
        assert!(
            tokio::time::timeout(Duration::from_millis(100), client.write_all(&data))
                .await
                .is_err()
        );
        let mut received = [0; 1200];
        assert!(server.read(&mut received).await.unwrap() > 0);
        assert!(received.iter().all(|byte| *byte == 7));
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn duplicate_packet_flood_is_bounded_and_disconnects() {
    use kcp_ovo::helper::KcpPacketHeader;
    let mut config = settings();
    config.queue_capacity = 2;
    let socket = Arc::new(tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let peer = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let (tx, rx) = tokio::sync::mpsc::channel(4);
    let packet = KcpPacketHeader::new(1, kcp_ovo::KcpCmd::Push, 0, 64, 0, 0, 0, 0)
        .to_bytes()
        .unwrap();
    for _ in 0..4 {
        tx.try_send(packet.to_vec()).unwrap();
    }
    let mut stream = KcpStream::spawn(
        socket,
        (peer.local_addr().unwrap(), 1),
        config,
        rx,
        Arc::new(tokio::sync::Notify::new()),
    )
    .unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), stream.read(&mut [0; 1]))
            .await
            .unwrap()
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn close_wakes_pending_accept_and_releases_udp_port() {
    let listener = Arc::new(
        KcpTransport::bind("127.0.0.1:0".parse().unwrap(), settings())
            .await
            .unwrap(),
    );
    let addr = listener.local_addr().unwrap();
    let accepting = listener.clone();
    let pending = tokio::spawn(async move { accepting.accept().await });
    tokio::task::yield_now().await;
    tokio::time::timeout(Duration::from_secs(1), listener.close())
        .await
        .unwrap();
    assert!(pending.await.unwrap().is_err());
    let _socket = tokio::net::UdpSocket::bind(addr).await.unwrap();
}

#[tokio::test]
async fn late_packets_cannot_resurrect_a_closed_session() {
    let mut config = settings();
    config.idle_timeout = Duration::from_millis(200);
    let listener = KcpTransport::bind("127.0.0.1:0".parse().unwrap(), config)
        .await
        .unwrap();
    let mut client = KcpStream::connect(listener.local_addr().unwrap(), config)
        .await
        .unwrap();
    let (server, _) = listener.accept().await.unwrap();
    drop(server);
    client.write_all(b"late TLS bytes").await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(1), client.flush())
            .await
            .unwrap()
            .is_err()
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(50), listener.accept())
            .await
            .is_err()
    );
    listener.close().await;
}

#[tokio::test]
async fn ipv6_bidirectional_stream_and_port_release() {
    tokio::time::timeout(Duration::from_secs(3), async {
        let listener = KcpTransport::bind("[::1]:0".parse().unwrap(), settings())
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let mut client = KcpStream::connect(addr, settings()).await.unwrap();
        let (mut server, _) = listener.accept().await.unwrap();
        client.write_all(b"ping").await.unwrap();
        client.flush().await.unwrap();
        let mut data = [0; 4];
        server.read_exact(&mut data).await.unwrap();
        assert_eq!(&data, b"ping");
        server.write_all(b"pong").await.unwrap();
        server.flush().await.unwrap();
        client.read_exact(&mut data).await.unwrap();
        assert_eq!(&data, b"pong");
        listener.close().await;
        assert_eq!(server.read(&mut data).await.unwrap(), 0);
        let _rebound = tokio::net::UdpSocket::bind(addr).await.unwrap();
    })
    .await
    .unwrap();
}
