//! 应用层:用例编排(PRD 第 6 章)。
//!
//! 只依赖 domain 定义的抽象;对外部技术的依赖通过端口 trait(`ports`)声明,
//! 由 infrastructure 注入实现。Room 的 Actor 模式实现也位于本层(应用层编排)。

pub mod auth;
pub mod dto;
pub mod error;
pub mod player;
pub mod ports;
pub mod room;
pub mod unit_of_work;

pub use auth::{LoginDependencies, LoginUseCase, RegisterUseCase};
pub use dto::{
    LoginCommand, LoginResult, PlayerProfile, RegisterCommand, RegisterResult, RoomSummary,
};
pub use error::AppError;
pub use player::{GainExpUseCase, GetPlayerProfile};
pub use ports::{AuditLogger, EventPublisher, PasswordHasher, SessionTokenStore};
pub use room::{RoomCommand, RoomEvent, RoomHandle, RoomService, RoomServiceConfig};
pub use unit_of_work::UnitOfWork;

#[cfg(test)]
pub(crate) mod fakes;
