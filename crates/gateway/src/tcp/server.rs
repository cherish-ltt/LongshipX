//! TCP+TLS 网关:监听绑定、accept 循环与停机联动。

use crate::tcp::config::TcpGatewayConfig;
use crate::tcp::context::GatewayDeps;
use crate::tcp::handler;
use longshipx_net_kit::Transport;
use longshipx_net_kit::{AcceptLoopConfig, TcpTlsTransport, TlsAcceptor, accept_loop_with};
use std::sync::Arc;
use tokio::sync::watch;

struct RawTcp(Arc<TcpTlsTransport>);

#[async_trait::async_trait]
impl Transport for RawTcp {
    type Conn = tokio::net::TcpStream;
    async fn accept(&self) -> std::io::Result<(Self::Conn, std::net::SocketAddr)> {
        self.0
            .accept_tcp()
            .await
            .map_err(|err| std::io::Error::other(err.to_string()))
    }
    fn local_addr(&self) -> std::io::Result<std::net::SocketAddr> {
        self.0.local_addr()
    }
}

pub struct TcpGateway {
    transport: Arc<TcpTlsTransport>,
    deps: Arc<GatewayDeps>,
    config: TcpGatewayConfig,
}

impl TcpGateway {
    pub fn bind(
        config: TcpGatewayConfig,
        acceptor: TlsAcceptor,
        deps: Arc<GatewayDeps>,
    ) -> Result<Self, crate::error::GatewayError> {
        let transport = TcpTlsTransport::bind(
            config.bind_addr,
            acceptor,
            config.backlog,
            config.nodelay,
            Some(config.keepalive),
        )?;
        Ok(Self {
            transport: Arc::new(transport),
            deps,
            config,
        })
    }

    pub fn local_addr(&self) -> Result<std::net::SocketAddr, crate::error::GatewayError> {
        self.transport
            .local_addr()
            .map_err(|err| crate::error::GatewayError::Net(err.into()))
    }

    /// 运行 accept 循环直到收到停机信号;退出后向在线连接广播维护通知(PRD 13.2)。
    pub async fn run(&self, shutdown: watch::Receiver<bool>) {
        let deps = self.deps.clone();
        let config = self.config;
        let transport = self.transport.clone();
        let handshakes = Arc::new(tokio::sync::Semaphore::new(
            config.accept_concurrency.max(1) as usize,
        ));
        let handler = move |stream: tokio::net::TcpStream, peer: std::net::SocketAddr| {
            let deps = deps.clone();
            let transport = transport.clone();
            let handshakes = handshakes.clone();
            async move {
                let Some(admission) =
                    handler::Admission::acquire(&deps, peer, config.unauth_timeout)
                else {
                    return;
                };
                let Ok(slot) = handshakes.try_acquire_owned() else {
                    tracing::debug!(%peer, "TLS 握手并发已达上限,拒绝");
                    return;
                };
                let result =
                    tokio::time::timeout_at(admission.deadline, transport.handshake(stream)).await;
                drop(slot);
                let stream = match result {
                    Ok(Ok(stream)) => stream,
                    Ok(Err(err)) => {
                        tracing::debug!(%peer, error = %err, "TLS 握手失败");
                        return;
                    },
                    Err(_) => {
                        tracing::debug!(%peer, "TLS 握手超时");
                        return;
                    },
                };
                handler::handle_admitted_connection(stream, peer, deps, config, admission).await;
            }
        };
        accept_loop_with(
            Arc::new(RawTcp(self.transport.clone())),
            AcceptLoopConfig {
                max_concurrent: Some(self.config.max_connections),
                accept_concurrency: self.config.accept_concurrency.max(1) as usize,
            },
            shutdown,
            handler,
        )
        .await;
        let notified = self.broadcast_shutdown("服务器即将维护,请保存进度并重连");
        tracing::info!(notified, "已向在线连接广播停机通知");
    }

    /// 优雅停机第一拍:向所有在线连接广播维护通知(PRD 13.2)。
    pub fn broadcast_shutdown(&self, message: &str) -> usize {
        self.deps.connections.broadcast(
            self.deps.codec.as_ref(),
            &longshipx_protocol::OutboundMessage::Shutdown(
                longshipx_protocol::generated::ServerShutdownNotice {
                    message: message.to_string(),
                },
            ),
        )
    }

    pub fn active_connections(&self) -> usize {
        self.deps.connections.active_count()
    }

    pub fn config(&self) -> TcpGatewayConfig {
        self.config
    }

    pub fn bind_addr(&self) -> std::net::SocketAddr {
        self.config.bind_addr
    }

    pub async fn wait_until_drained(&self, timeout: std::time::Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        while self.active_connections() > 0 {
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        true
    }
}
