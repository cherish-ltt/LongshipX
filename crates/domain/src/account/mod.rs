//! 账号聚合:认证身份与账号状态。

mod aggregate;
mod repository;

pub use aggregate::{Account, AccountId, AccountStatus};
pub use repository::AccountRepository;
