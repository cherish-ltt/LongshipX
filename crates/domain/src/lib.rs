//! 领域层:依赖方向的最内层(PRD 第 4/5 章)。
//!
//! 🔴 本 crate 禁止依赖 tokio、sea_orm、redis、prost 等任何框架/技术栈;
//! 只允许 uuid、chrono、thiserror、serde(标记)、async-trait(仓储接口,PRD 5.2)。
//! 基础设施层的映射类型必须通过显式 `From`/`TryFrom` 与本层互转。

pub mod account;
pub mod events;
pub mod player;
pub mod room;
pub mod session;
pub mod shared;

pub use account::{Account, AccountId, AccountRepository, AccountStatus};
pub use events::DomainEvent;
pub use player::{Player, PlayerId, PlayerRepository};
pub use room::{Room, RoomId, RoomRepository, RoomState};
pub use session::{Session, SessionId};
pub use shared::error::{DomainError, RepoError};
pub use shared::value::{Nickname, PasswordHash, PlainPassword, Username};
