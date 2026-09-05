//! accept 循环:统一处理新连接接入、并发上限与停机(PRD 8.1/9.3)。

use crate::transport::Transport;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Semaphore, watch};

/// 持续 accept 直到收到停机信号。
///
/// * `max_concurrent`:同时处理的连接上限,达到上限后新连接被直接关闭;
/// * `on_connection`:每条连接的处理器,在独立 task 中运行(单连接 panic 不影响进程,PRD 9.3)。
pub async fn accept_loop<T, F, Fut>(
    transport: Arc<T>,
    max_concurrent: Option<usize>,
    mut shutdown: watch::Receiver<bool>,
    on_connection: F,
) where
    T: Transport + 'static,
    F: Fn(T::Conn, std::net::SocketAddr) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let semaphore = max_concurrent.map(|n| Arc::new(Semaphore::new(n)));
    let on_connection = Arc::new(on_connection);
    loop {
        tokio::select! {
            _ = shutdown.changed() => {
                tracing::info!("监听器收到停机信号,停止 accept");
                break;
            }
            accepted = transport.accept() => match accepted {
                Ok((conn, peer)) => {
                    let permit = match &semaphore {
                        Some(sem) => match Semaphore::try_acquire_owned(sem.clone()) {
                            Ok(permit) => Some(permit),
                            Err(_) => {
                                tracing::debug!(%peer, "连接数已达上限,拒绝新连接");
                                continue;
                            }
                        },
                        None => None,
                    };
                    let handler = on_connection.clone();
                    tokio::spawn(async move {
                        let _permit = permit;
                        handler(conn, peer).await;
                    });
                }
                Err(err) => {
                    tracing::warn!(error = %err, "accept 失败,稍后重试");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support;
    use std::net::SocketAddr;
    use tokio::io::AsyncReadExt as _;

    #[tokio::test]
    async fn serves_connections_until_shutdown() {
        crate::tls::install_ring_provider();
        let acceptor = crate::tls::server_acceptor_from_pem_bytes(
            test_support::cert_pems().0,
            test_support::cert_pems().1,
        )
        .unwrap();
        let transport = Arc::new(
            crate::TcpTlsTransport::bind("127.0.0.1:0".parse().unwrap(), acceptor, 16, true, None)
                .unwrap(),
        );
        let addr: SocketAddr = transport.local_addr().unwrap();

        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let seen = Arc::new(tokio::sync::Mutex::new(Vec::<SocketAddr>::new()));
        let seen_for_handler = seen.clone();
        let handler = move |conn: crate::TlsTcpStream, peer: SocketAddr| {
            let seen = seen_for_handler.clone();
            async move {
                seen.lock().await.push(peer);
                drop(conn);
            }
        };

        let server = tokio::spawn(accept_loop(transport, Some(8), shutdown_rx, handler));
        let client = tokio::spawn(test_support::tls_client_connect(addr));
        client.await.unwrap().unwrap();

        // 等待服务端记录连接。
        for _ in 0..50 {
            if !seen.lock().await.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(seen.lock().await.len(), 1);

        shutdown_tx.send_replace(true);
        assert!(
            tokio::time::timeout(Duration::from_secs(1), server)
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn rejects_connections_over_concurrency_limit() {
        crate::tls::install_ring_provider();
        let acceptor = crate::tls::server_acceptor_from_pem_bytes(
            test_support::cert_pems().0,
            test_support::cert_pems().1,
        )
        .unwrap();
        let transport = Arc::new(
            crate::TcpTlsTransport::bind("127.0.0.1:0".parse().unwrap(), acceptor, 16, true, None)
                .unwrap(),
        );
        let addr: SocketAddr = transport.local_addr().unwrap();
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        // 处理器挂起 200ms,占住唯一许可。
        let handler = |_conn: crate::TlsTcpStream, _peer: SocketAddr| async {
            tokio::time::sleep(Duration::from_millis(200)).await;
        };
        let _server = tokio::spawn(accept_loop(transport, Some(1), shutdown_rx, handler));
        tokio::time::sleep(Duration::from_millis(50)).await;
        let first = tokio::spawn(test_support::tls_client_connect(addr));
        tokio::time::sleep(Duration::from_millis(50)).await;
        // 第二条连接:TLS 握手可能在丢弃前已完成,但套接字会很快被服务端关闭。
        let second = tokio::spawn(async move {
            let Ok(mut stream) = test_support::tls_client_connect(addr).await else {
                return true; // 连接阶段即被拒绝
            };
            let mut byte = [0u8; 1];
            match tokio::time::timeout(Duration::from_secs(2), stream.read(&mut byte)).await {
                // Ok(0)/Err = 服务端已关闭;Ok(1) = 意外收到数据。
                Ok(Ok(0)) | Ok(Err(_)) => true,
                _ => false,
            }
        });
        assert!(first.await.unwrap().is_ok(), "第一条连接应正常握手");
        let second_closed = tokio::time::timeout(Duration::from_secs(4), second)
            .await
            .ok()
            .and_then(|joined| joined.ok())
            .unwrap_or(false);
        assert!(second_closed, "第二条连接应被服务端关闭");
        shutdown_tx.send_replace(true);
    }
}
