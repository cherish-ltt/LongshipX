//! Transport 抽象(PRD 8.1):当前仅 TCP+TLS 实现,为 KCP/QUIC 预留接口。

use crate::TlsTcpStream;
use crate::error::NetError;
use async_trait::async_trait;
use socket2::{Domain, Protocol, Socket, TcpKeepalive, Type};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::net::TcpListener;

/// 传输抽象:accept 返回一条可读写的连接与对端地址。
#[async_trait]
pub trait Transport: Send + Sync {
    type Conn: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + 'static;

    async fn accept(&self) -> std::io::Result<(Self::Conn, SocketAddr)>;

    fn local_addr(&self) -> std::io::Result<SocketAddr>;
}

/// TCP + TLS 1.3 传输。
pub struct TcpTlsTransport {
    listener: TcpListener,
    acceptor: crate::TlsAcceptor,
    nodelay: bool,
    keepalive: Option<Duration>,
}

impl TcpTlsTransport {
    /// 绑定监听,`backlog` 由配置注入(PRD 18.1 SERVER_CONNECTION_BACKLOG)。
    pub fn bind(
        addr: SocketAddr,
        acceptor: crate::TlsAcceptor,
        backlog: u32,
        nodelay: bool,
        keepalive: Option<Duration>,
    ) -> Result<Self, NetError> {
        let socket = Socket::new(Domain::for_address(addr), Type::STREAM, Some(Protocol::TCP))?;
        socket.set_reuse_address(true)?;
        socket.set_nonblocking(true)?;
        socket.bind(&socket2::SockAddr::from(addr))?;
        socket.listen(i32::try_from(backlog).unwrap_or(i32::MAX))?;
        let listener = TcpListener::from_std(socket.into())?;
        Ok(Self {
            listener,
            acceptor,
            nodelay,
            keepalive,
        })
    }

    /// 接受一条 TCP 连接并完成 TLS 1.3 握手。
    pub async fn accept_tls(&self) -> Result<(TlsTcpStream, SocketAddr), NetError> {
        let (tcp, peer) = self.listener.accept().await?;
        self.configure(&tcp)?;
        let stream = self
            .acceptor
            .accept(tcp)
            .await
            .map_err(|err| NetError::Tls(err.to_string()))?;
        Ok((stream, peer))
    }

    fn configure(&self, tcp: &tokio::net::TcpStream) -> Result<(), NetError> {
        tcp.set_nodelay(self.nodelay)?;
        if let Some(interval) = self.keepalive {
            let keepalive = TcpKeepalive::new().with_time(interval);
            socket2::SockRef::from(tcp).set_tcp_keepalive(&keepalive)?;
        }
        Ok(())
    }

    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.listener.local_addr()
    }
}

#[async_trait]
impl Transport for TcpTlsTransport {
    type Conn = TlsTcpStream;

    async fn accept(&self) -> std::io::Result<(Self::Conn, SocketAddr)> {
        self.accept_tls()
            .await
            .map_err(|err| std::io::Error::other(err.to_string()))
    }

    fn local_addr(&self) -> std::io::Result<SocketAddr> {
        Self::local_addr(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support;

    #[tokio::test]
    async fn bind_reports_local_addr() {
        crate::tls::install_ring_provider();
        let (cert, key) = test_support::cert_pems();
        let acceptor = crate::tls::server_acceptor_from_pem_bytes(cert, key).unwrap();
        let transport = TcpTlsTransport::bind(
            "127.0.0.1:0".parse().unwrap(),
            acceptor,
            16,
            true,
            Some(Duration::from_secs(30)),
        )
        .unwrap();
        let addr = transport.local_addr().unwrap();
        assert!(addr.port() > 0);
        assert!(addr.ip().is_loopback());
    }

    #[tokio::test]
    async fn accept_completes_tls_handshake() {
        crate::tls::install_ring_provider();
        let (cert, key) = test_support::cert_pems();
        let acceptor = crate::tls::server_acceptor_from_pem_bytes(cert, key).unwrap();
        let transport = std::sync::Arc::new(
            TcpTlsTransport::bind("127.0.0.1:0".parse().unwrap(), acceptor, 16, true, None)
                .unwrap(),
        );
        let server_addr = transport.local_addr().unwrap();

        let client_task = tokio::spawn(test_support::tls_client_connect(server_addr));
        let (_stream, peer) = transport.accept_tls().await.unwrap();
        assert!(peer.ip().is_loopback());
        // 客户端握手也应当成功。
        client_task.await.unwrap().unwrap();
    }
}
