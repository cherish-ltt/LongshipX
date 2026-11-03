//! accept 循环:统一处理新连接接入、并发上限与停机(PRD 8.1/9.3)。
//!
//! 接入分两级:TCP accept(微秒级)与 TLS 握手(毫秒级)由 accept worker
//! 并行消化,内核 accept 队列在突发建连时也能被即时排空——否则 macOS
//! `kern.ipc.somaxconn=128` 之类的系统限制会让 SYN 被丢弃/重置(基准实测)。
//! worker 池是固定有界并发,`max_concurrent` 名额仍在 accept 时刻同步预占,
//! 语义与"同时处理的连接上限"保持一致。

use crate::transport::Transport;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Semaphore, watch};

/// accept 循环的可调参数。
#[derive(Debug, Clone, Copy)]
pub struct AcceptLoopConfig {
    pub shutdown_timeout: Duration,
    /// 同时在处理的连接上限(含握手未完成的连接,PRD 8.1)。
    pub max_concurrent: Option<usize>,
    /// TLS 握手(accept 管线)的并行度。1 = 旧版串行行为;生产建议 256。
    pub accept_concurrency: usize,
}

impl Default for AcceptLoopConfig {
    fn default() -> Self {
        Self {
            shutdown_timeout: Duration::from_secs(30),
            max_concurrent: None,
            accept_concurrency: 1,
        }
    }
}

/// 持续 accept 直到收到停机信号(兼容旧签名:串行 accept,无握手并行)。
///
/// * `max_concurrent`:同时处理的连接上限,达到上限后新连接被直接关闭;
/// * `on_connection`:每条连接的处理器,在独立 task 中运行(单连接 panic 不影响进程,PRD 9.3)。
pub async fn accept_loop<T, F, Fut>(
    transport: Arc<T>,
    max_concurrent: Option<usize>,
    shutdown: watch::Receiver<bool>,
    on_connection: F,
) where
    T: Transport + 'static,
    F: Fn(T::Conn, std::net::SocketAddr) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    accept_loop_with(
        transport,
        AcceptLoopConfig {
            max_concurrent,
            accept_concurrency: 1,
            ..Default::default()
        },
        shutdown,
        on_connection,
    )
    .await;
}

/// 持续 accept 直到收到停机信号;TLS 握手由 `accept_concurrency` 个 worker
/// 并行消化,`max_concurrent` 名额在 accept 时刻同步预占。
pub async fn accept_loop_with<T, F, Fut>(
    transport: Arc<T>,
    config: AcceptLoopConfig,
    shutdown: watch::Receiver<bool>,
    on_connection: F,
) where
    T: Transport + 'static,
    F: Fn(T::Conn, std::net::SocketAddr) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let concurrency = config.accept_concurrency.max(1);
    let semaphore = config.max_concurrent.map(|n| Arc::new(Semaphore::new(n)));
    let on_connection = Arc::new(on_connection);
    let handles = (0..concurrency)
        .map(|id| {
            let transport = transport.clone();
            let semaphore = semaphore.clone();
            let on_connection = on_connection.clone();
            let mut shutdown = shutdown.clone();
            tokio::spawn(async move {
                let mut handshakes = tokio::task::JoinSet::new();
                'accept_loop: while !*shutdown.borrow() {
                    // 回收旧任务不能取消正在进行的 accept/TLS 握手。
                    let accepted = transport.accept();
                    tokio::pin!(accepted);
                    let accepted = loop {
                        tokio::select! {
                            completed = handshakes.join_next(), if !handshakes.is_empty() => {
                                if let Some(Err(err)) = completed {
                                    tracing::warn!(worker = id, error = %err, "连接处理任务异常退出");
                                }
                            }
                            _ = shutdown.changed() => {
                                tracing::info!(worker = id, "监听器收到停机信号,停止 accept");
                                break 'accept_loop;
                            }
                            result = &mut accepted => break result,
                        }
                    };
                    match accepted {
                        Ok((conn, peer)) => {
                            // 连接名额在 accept 时刻同步预占:上限语义含握手中的连接。
                            let permit = match &semaphore {
                                Some(sem) => match Semaphore::try_acquire_owned(sem.clone()) {
                                    Ok(permit) => Some(permit),
                                    Err(_) => {
                                        tracing::debug!(%peer, "连接数已达上限,拒绝新连接");
                                        continue;
                                    },
                                },
                                None => None,
                            };
                            let handler = on_connection.clone();
                            handshakes.spawn(async move {
                                let _permit = permit;
                                handler(conn, peer).await;
                            });
                        },
                        Err(err) => {
                            tracing::warn!(worker = id, error = %err, "accept 失败,稍后重试");
                            tokio::time::sleep(Duration::from_millis(50)).await;
                        },
                    }
                }
                let drained = tokio::time::timeout(config.shutdown_timeout, async {
                    while handshakes.join_next().await.is_some() {}
                })
                .await;
                if drained.is_err() {
                    handshakes.abort_all();
                    while handshakes.join_next().await.is_some() {}
                }
            })
        })
        .collect::<Vec<_>>();
    for handle in handles {
        let _ = handle.await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support;
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::AsyncReadExt as _;

    async fn spawn_acceptor() -> (SocketAddr, crate::TcpTlsTransport) {
        crate::tls::install_ring_provider();
        let acceptor = crate::tls::server_acceptor_from_pem_bytes(
            test_support::cert_pems().0,
            test_support::cert_pems().1,
        )
        .unwrap();
        // backlog ≥ 并发建连突发量:过小会在内核 accept 队列溢出时
        // 直接触发 ECONNRESET(macOS 行为),与被测代码无关。
        let transport =
            crate::TcpTlsTransport::bind("127.0.0.1:0".parse().unwrap(), acceptor, 64, true, None)
                .unwrap();
        let addr: SocketAddr = transport.local_addr().unwrap();
        (addr, transport)
    }

    #[tokio::test]
    async fn serves_connections_until_shutdown() {
        let (addr, transport) = spawn_acceptor().await;

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

        let server = tokio::spawn(accept_loop(
            Arc::new(transport),
            Some(8),
            shutdown_rx,
            handler,
        ));
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
        let (addr, transport) = spawn_acceptor().await;
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        // 处理器挂起 200ms,占住唯一许可。
        let handler = |_conn: crate::TlsTcpStream, _peer: SocketAddr| async {
            tokio::time::sleep(Duration::from_millis(200)).await;
        };
        let _server = tokio::spawn(accept_loop(
            Arc::new(transport),
            Some(1),
            shutdown_rx,
            handler,
        ));
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

    /// 突发并发建连:多 worker 并行消化 TLS 握手,全部连接都应成功
    /// (串行 accept 下,同样的突发会拖慢排空、加剧 accept 队列压力)。
    #[tokio::test]
    async fn concurrent_handshake_pipeline_surges() {
        let (addr, transport) = spawn_acceptor().await;
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let seen = Arc::new(AtomicUsize::new(0));
        let seen_for_handler = seen.clone();
        let handler = move |_conn: crate::TlsTcpStream, _peer: SocketAddr| {
            let seen = seen_for_handler.clone();
            async move {
                seen.fetch_add(1, Ordering::SeqCst);
            }
        };
        let server = tokio::spawn(accept_loop_with(
            Arc::new(transport),
            AcceptLoopConfig {
                shutdown_timeout: Duration::from_secs(1),
                max_concurrent: Some(64),
                accept_concurrency: 8,
            },
            shutdown_rx,
            handler,
        ));
        // 突发 32 条并发建连:全部应当完成 TLS 握手并被计数。
        let clients: Vec<_> = (0..32)
            .map(|_| tokio::spawn(test_support::tls_client_connect(addr)))
            .collect();
        for client in clients {
            client.await.unwrap().unwrap();
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while seen.load(Ordering::SeqCst) < 32 {
            assert!(
                tokio::time::Instant::now() < deadline,
                "并发握手管线未在时限内消化全部连接"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        shutdown_tx.send_replace(true);
        assert!(
            tokio::time::timeout(Duration::from_secs(1), server)
                .await
                .is_ok()
        );
    }
}
