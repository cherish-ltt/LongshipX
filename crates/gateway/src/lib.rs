//! 接口层(PRD 4.1/8):把外部请求翻译成 application 用例调用。
//!
//! * `tcp`:TCP+TLS 长连接入口(绑定/心跳/房间/聊天 + 背压 + 限流);
//! * `http`:axum HTTP 入口(注册/登录/档案/健康检查)。
//!
//! 🔴 未鉴权连接必须限时限量;🔴 发送队列必须有界;🔴 不信任客户端上报结果。

pub mod error;
pub mod http;
pub mod tcp;
