//! HTTP 入口(axum):注册/登录/档案/健康检查(PRD 8.3)。

pub mod auth_extractor;
pub mod error;
pub mod routes;
pub mod state;

pub use error::ApiError;
pub use state::HttpState;
