//! HTTP 入口(axum):注册/登录/档案/健康检查(PRD 8.3)。

pub mod auth_extractor;
pub mod error;
pub mod routes;
pub mod server;
pub mod state;

pub use error::ApiError;
pub use server::{hsts_middleware, redirect_router, serve_https, serve_redirect};
pub use state::HttpState;
