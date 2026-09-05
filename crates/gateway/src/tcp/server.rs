//! TCP+TLS 网关:监听绑定、accept 循环与停机联动。

use crate::tcp::config::TcpGatewayConfig;
use crate::tcp::context::GatewayDeps;
use crate::tcp::handler;
use longshipx_net_kit::{TcpTlsTransport, TlsAcceptor, accept_loop};
use std::sync::Arc;
use tokio::sync::watch;

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
        let handler = move |stream: longshipx_net_kit::TlsTcpStream, peer: std::net::SocketAddr| {
            let deps = deps.clone();
            async move {
                handler::handle_connection(stream, peer, deps, config).await;
            }
        };
        accept_loop(
            self.transport.clone(),
            Some(self.config.max_connections),
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
