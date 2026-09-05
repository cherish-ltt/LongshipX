//! 玩家仓储接口:仅定义,实现在 infrastructure。

use crate::account::AccountId;
use crate::player::{Player, PlayerId};
use crate::shared::error::RepoError;

#[async_trait::async_trait]
pub trait PlayerRepository: Send + Sync {
    async fn find_by_id(&self, id: PlayerId) -> Result<Option<Player>, RepoError>;

    async fn find_by_account(&self, account_id: AccountId) -> Result<Option<Player>, RepoError>;

    /// 新玩家插入、已有玩家更新。
    async fn save(&self, player: &Player) -> Result<(), RepoError>;
}
