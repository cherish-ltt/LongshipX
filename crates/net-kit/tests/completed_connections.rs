use longshipx_net_kit::{AcceptLoopConfig, Transport, accept_loop_with};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

#[path = "support/allocations.rs"]
mod allocations;

struct FakeTransport(tokio::sync::Mutex<tokio::sync::mpsc::Receiver<tokio::io::DuplexStream>>);

#[async_trait::async_trait]
impl Transport for FakeTransport {
    type Conn = tokio::io::DuplexStream;
    async fn accept(&self) -> std::io::Result<(Self::Conn, std::net::SocketAddr)> {
        let connection = self.0.lock().await.recv().await.unwrap();
        Ok((connection, "127.0.0.1:12345".parse().unwrap()))
    }
    fn local_addr(&self) -> std::io::Result<std::net::SocketAddr> {
        Ok("127.0.0.1:12345".parse().unwrap())
    }
}

#[tokio::test]
async fn completed_connections_must_not_accumulate_task_allocations() {
    let (tx, rx) = tokio::sync::mpsc::channel(1);
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let completed = Arc::new(AtomicUsize::new(0));
    let seen = completed.clone();
    let server = tokio::spawn(accept_loop_with(
        Arc::new(FakeTransport(tokio::sync::Mutex::new(rx))),
        AcceptLoopConfig {
            shutdown_timeout: std::time::Duration::from_secs(1),
            max_concurrent: Some(16),
            accept_concurrency: 1,
        },
        shutdown_rx,
        move |conn, _| {
            let seen = seen.clone();
            async move {
                drop(conn);
                seen.fetch_add(1, Ordering::SeqCst);
            }
        },
    ));
    let start = allocations::live_bytes();
    for round in 1..=10_000 {
        let (local, peer) = tokio::io::duplex(1);
        drop(peer);
        tx.send(local).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while completed.load(Ordering::SeqCst) != round {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        tokio::task::yield_now().await;
    }
    let retained = allocations::live_bytes().saturating_sub(start);
    shutdown_tx.send_replace(true);
    server.await.unwrap();
    tokio::task::yield_now().await;
    let after_shutdown = allocations::live_bytes().saturating_sub(start);
    eprintln!("completed=10000 retained_bytes={retained} after_shutdown_bytes={after_shutdown}");
    assert!(
        retained < 100_000,
        "completed connection tasks retain {retained} bytes until listener shutdown"
    );
}
