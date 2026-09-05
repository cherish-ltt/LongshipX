//! TCP+TLS 长连接入口。

pub mod auth_gate;
pub mod config;
pub mod connections;
pub mod context;
pub mod convert;
pub mod handler;
pub mod handlers;
pub mod rate_limit;
pub mod router_setup;
pub mod server;

pub use auth_gate::AuthGate;
pub use config::{RateLimitSettings, TcpGatewayConfig};
pub use connections::ConnectionRegistry;
pub use context::{AuthState, AuthedPlayer, ConnContext, GatewayDeps};
pub use rate_limit::TokenBucket;
pub use server::TcpGateway;
