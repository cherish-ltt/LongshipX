//! 玩家聚合:游戏内角色档案与成长。

mod aggregate;
mod repository;

pub use aggregate::{MAX_LEVEL, Player, PlayerId};
pub use repository::PlayerRepository;
