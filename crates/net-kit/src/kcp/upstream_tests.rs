use super::{KcpSettings, KcpStream, driver, tests::settings};
use kcp_ovo::core::kcp::Kcp;
use kcp_ovo::helper::{KcpPacketHeader, current_millis};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::UdpSocket;

#[tokio::test]
async fn stream_config_uses_corrected_fast_timeout_backoff() {
    let config = KcpSettings {
        interval: Duration::from_millis(1),
        ..settings()
    };
    let mut core = Kcp::new(1, config.core_config()).unwrap();
    assert!(core.stream_mode && core.nodelay);
    assert!(!core.nocwnd);
    assert_eq!(core.fastresend, 2);
    core.set_output(|packet| async move { Ok(packet.len()) });
    core.send(b"unacknowledged TLS bytes").unwrap();
    core.update(0).await.unwrap();

    // Virtual time: no RTT sample or ACK can alter the timeout sequence.
    for (xmit, rto, deadline) in [(1, 200, 200), (2, 300, 500), (3, 450, 950)] {
        let segment = core.snd_buf.front().unwrap();
        assert_eq!(
            (segment.xmit, segment.rto, segment.resendts),
            (xmit, rto, deadline)
        );
        core.update(deadline - 1).await.unwrap();
        assert_eq!(core.snd_buf.front().unwrap().xmit, xmit);
        core.update(deadline).await.unwrap();
        assert_eq!(core.snd_buf.front().unwrap().xmit, xmit + 1);
    }
    assert_eq!(core.waitsnd(), 1);
}

#[test]
fn core_receive_preserves_owned_payload_and_reordered_stream() {
    let mut core = Kcp::new(1, settings().core_config()).unwrap();
    let mut packet = KcpPacketHeader::new(1, kcp_ovo::KcpCmd::Push, 0, 64, 0, 0, 0, 3)
        .to_bytes()
        .unwrap();
    packet.extend_from_slice(b"one");
    core.input(&packet).unwrap();
    let owned = core.rcv_queue.front().unwrap().kcp_packet.data.as_ptr();
    packet[28..].fill(0);
    let received: Vec<u8> = core.recv().unwrap();
    assert_eq!(received, b"one");
    assert_eq!(
        received.as_ptr(),
        owned,
        "single segment must move, not copy"
    );

    for (sn, payload) in [(2, b"tri"), (2, b"tri"), (1, b"two")] {
        let mut packet = KcpPacketHeader::new(1, kcp_ovo::KcpCmd::Push, 0, 64, 0, sn, 0, 3)
            .to_bytes()
            .unwrap();
        packet.extend_from_slice(payload);
        core.input(&packet).unwrap();
        packet.fill(0);
    }
    assert_eq!(core.recv().unwrap(), b"twotri");
    assert_eq!(core.nrcv_buf, 0);
    assert_eq!(core.nrcv_que, 0);
    assert!(matches!(core.recv(), Err(kcp_ovo::KcpError::QueueEmpty)));
}

#[tokio::test]
async fn duplicate_ack_does_not_trigger_false_fast_retransmission() {
    let mut core = Kcp::new(1, settings().core_config()).unwrap();
    core.set_output(|packet| async move { Ok(packet.len()) });
    core.peer_seen = true;
    core.rmt_wnd = 64;
    core.cwnd = 3;
    core.send(&vec![42; core.mss as usize * 3]).unwrap();
    core.update(0).await.unwrap();
    let ack = KcpPacketHeader::new(1, kcp_ovo::KcpCmd::Ack, 0, 64, 0, 2, 0, 0)
        .to_bytes()
        .unwrap();
    for _ in 0..3 {
        core.input(&ack).unwrap();
    }
    assert_eq!(
        core.waitsnd(),
        3,
        "selective ACK is not cumulative confirmation"
    );
    assert_eq!(core.snd_buf.front().unwrap().fastack, 1);
    core.update(10).await.unwrap();
    assert!(core.snd_buf.iter().all(|segment| segment.xmit == 1));
    let cumulative = KcpPacketHeader::new(1, kcp_ovo::KcpCmd::Wins, 0, 64, 0, 0, 3, 0)
        .to_bytes()
        .unwrap();
    core.input(&cumulative).unwrap();
    assert_eq!(core.waitsnd(), 0);
}

#[tokio::test]
async fn cancelled_flush_keeps_data_and_resumes_after_ack() {
    tokio::time::timeout(Duration::from_secs(3), async {
        let config = settings();
        let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let mut client = KcpStream::connect(socket.local_addr().unwrap(), config)
            .await
            .unwrap();
        client.write_all(b"TLS bytes").await.unwrap();
        let mut packet = [0; 1400];
        let (len, peer) = socket.recv_from(&mut packet).await.unwrap();
        let conv = driver::packet_conv(&packet[..len], config.mtu).unwrap();
        let mut core = driver::core(socket.clone(), peer, conv, config).unwrap();
        core.input(&packet[..len]).unwrap();
        let data = loop {
            match core.recv() {
                Ok(data) => break data,
                Err(kcp_ovo::KcpError::QueueEmpty) => {},
                Err(error) => panic!("unexpected receive failure: {error}"),
            }
            let (len, _) = socket.recv_from(&mut packet).await.unwrap();
            core.input(&packet[..len]).unwrap();
        };
        assert_eq!(data, b"TLS bytes");
        // Receipt alone cannot finish flush; deliberately withhold the peer's ACK output.
        assert!(
            tokio::time::timeout(Duration::from_millis(30), client.flush())
                .await
                .is_err()
        );
        core.update(current_millis()).await.unwrap();
        client.flush().await.unwrap();
        client.flush().await.unwrap();
        assert!(matches!(core.recv(), Err(kcp_ovo::KcpError::QueueEmpty)));
        client.shutdown().await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn udp_output_error_does_not_commit_a_sent_segment() {
    let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let mut core = driver::core(socket, "[::1]:9".parse().unwrap(), 1, settings()).unwrap();
    core.peer_seen = true;
    core.rmt_wnd = 64;
    core.send(b"TLS bytes").unwrap();
    // An IPv4 socket cannot send to IPv6; failure must leave the segment untransmitted.
    assert!(core.update(0).await.is_err());
    assert_eq!(core.waitsnd(), 1);
    assert_eq!(core.snd_buf.front().unwrap().xmit, 0);
}
