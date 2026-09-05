//! 内存版账号/玩家仓储:端到端测试与无数据库开发环境使用。

use async_trait::async_trait;
use chrono::Utc;
use longshipx_domain::shared::value::Nickname;
use longshipx_domain::{
    Account, AccountId, AccountRepository, Player, PlayerId, PlayerRepository, RepoError,
};
use std::collections::HashMap;
use std::sync::Mutex;

/// 内存账号仓储。
#[derive(Default)]
pub struct InMemoryAccountRepository {
    accounts: Mutex<HashMap<uuid::Uuid, Account>>,
}

impl InMemoryAccountRepository {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl AccountRepository for InMemoryAccountRepository {
    async fn find_by_id(&self, id: AccountId) -> Result<Option<Account>, RepoError> {
        Ok(self
            .accounts
            .lock()
            .expect("account repo 锁中毒")
            .get(&id.0)
            .cloned())
    }

    async fn find_by_username(&self, username: &str) -> Result<Option<Account>, RepoError> {
        Ok(self
            .accounts
            .lock()
            .expect("account repo 锁中毒")
            .values()
            .find(|account| account.username().as_str() == username)
            .cloned())
    }

    async fn save(&self, account: &Account) -> Result<(), RepoError> {
        self.accounts
            .lock()
            .expect("account repo 锁中毒")
            .insert(account.id().0, account.clone());
        Ok(())
    }
}

/// 内存玩家仓储。
#[derive(Default)]
pub struct InMemoryPlayerRepository {
    players: Mutex<HashMap<uuid::Uuid, Player>>,
}

impl InMemoryPlayerRepository {
    pub fn new() -> Self {
        Self::default()
    }

    /// 造一个测试玩家并注册进仓储。
    pub fn seed(&self, nickname: &str) -> Player {
        let player = Player::create(
            longshipx_domain::AccountId(uuid::Uuid::now_v7()),
            Nickname::try_new(nickname).expect("测试昵称应合法"),
            Utc::now(),
        );
        self.players
            .lock()
            .expect("player repo 锁中毒")
            .insert(player.id().0, player.clone());
        player
    }
}

#[async_trait]
impl PlayerRepository for InMemoryPlayerRepository {
    async fn find_by_id(&self, id: PlayerId) -> Result<Option<Player>, RepoError> {
        Ok(self
            .players
            .lock()
            .expect("player repo 锁中毒")
            .get(&id.0)
            .cloned())
    }

    async fn find_by_account(&self, account_id: AccountId) -> Result<Option<Player>, RepoError> {
        Ok(self
            .players
            .lock()
            .expect("player repo 锁中毒")
            .values()
            .find(|player| player.account_id() == account_id)
            .cloned())
    }

    async fn save(&self, player: &Player) -> Result<(), RepoError> {
        self.players
            .lock()
            .expect("player repo 锁中毒")
            .insert(player.id().0, player.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use longshipx_domain::shared::value::{PasswordHash, Username};

    #[tokio::test]
    async fn account_and_player_roundtrip() {
        let accounts = InMemoryAccountRepository::new();
        let account = Account::register(
            Username::try_new("dave").unwrap(),
            PasswordHash::new("hash".into()),
            Utc::now(),
        );
        accounts.save(&account).await.unwrap();
        assert!(accounts.find_by_username("dave").await.unwrap().is_some());
        assert!(accounts.find_by_username("nobody").await.unwrap().is_none());
        assert!(accounts.find_by_id(account.id()).await.unwrap().is_some());

        let players = InMemoryPlayerRepository::new();
        let player = players.seed("勇者戴夫");
        assert!(players.find_by_id(player.id()).await.unwrap().is_some());
        assert!(
            players
                .find_by_account(player.account_id())
                .await
                .unwrap()
                .is_some()
        );
    }
}
