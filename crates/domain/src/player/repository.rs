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

    /// 原子增加经验并返回提交后的玩家及本次升级结果;不存在返回 None。
    async fn gain_exp(
        &self,
        id: PlayerId,
        amount: u64,
    ) -> Result<Option<(Player, Option<u32>)>, RepoError>;

    /// 仅更新登录时间,不得覆盖成长数据;不存在返回 None。
    async fn record_login(
        &self,
        id: PlayerId,
        at: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<Player>, RepoError>;
}
