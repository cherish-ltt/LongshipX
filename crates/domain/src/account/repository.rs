//! 账号仓储接口:仅定义,实现在 infrastructure(依赖倒置,PRD 5.2)。

use crate::account::{Account, AccountId};
use crate::shared::error::RepoError;

#[async_trait::async_trait]
pub trait AccountRepository: Send + Sync {
    async fn find_by_id(&self, id: AccountId) -> Result<Option<Account>, RepoError>;

    /// 入参为已归一化(小写)的用户名;数据库 CITEXT 保证大小写不敏感唯一。
    async fn find_by_username(&self, username: &str) -> Result<Option<Account>, RepoError>;

    /// 新账号插入、已有账号更新(以 id 是否存在区分)。
    async fn save(&self, account: &Account) -> Result<(), RepoError>;
}
