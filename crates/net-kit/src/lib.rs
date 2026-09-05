//! net-kit:通用网络框架,零业务依赖(PRD 4.2)。
//!
//! * `transport`/`listener`:连接接入抽象与 accept 循环(为 KCP/QUIC 预留);
//! * `codec`:长度前缀帧与 `Codec` trait(协议实现方负责帧 ⇄ 消息);
//! * `connection`/`backpressure`:读写分离与有界发送队列(PRD 8.1/8.5 🔴)。

pub mod backpressure;
pub mod codec;
pub mod connection;
pub mod error;
pub mod listener;
pub mod tls;
pub mod transport;

#[cfg(test)]
pub(crate) mod test_support;

pub use backpressure::OutboundSender;
pub use codec::{Frame, encode_frame, read_frame, write_frame};
pub use connection::{ConnectionConfig, FrameReader, split_connection};
pub use error::NetError;
pub use listener::accept_loop;
pub use transport::{TcpTlsTransport, Transport};

/// 当前传输的连接类型:TCP + TLS 1.3。
pub type TlsTcpStream = tokio_rustls::server::TlsStream<tokio::net::TcpStream>;
/// TLS 服务端接收器。
pub type TlsAcceptor = tokio_rustls::TlsAcceptor;
