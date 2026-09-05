//! 账号聚合根。

use crate::shared::value::{PasswordHash, Username};
use chrono::{DateTime, Utc};

/// 账号标识(UUIDv7,时间有序)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct AccountId(pub uuid::Uuid);

/// 账号状态机:Active → Suspended/Banned(Banned 可带截止时间)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountStatus {
    Active,
    Suspended,
    Banned {
        reason: String,
        until: Option<DateTime<Utc>>,
    },
}

impl AccountStatus {
    /// 是否允许登录;封禁到期后自动恢复登录能力,`until: None` 表示永久封禁。
    pub fn allows_login(&self, now: DateTime<Utc>) -> bool {
        match self {
            Self::Active => true,
            Self::Suspended => false,
            Self::Banned { until, .. } => until.is_some_and(|until| now >= until),
        }
    }

    /// 数据库存储用的状态码(见 migration:0 Active/1 Suspended/2 Banned)。
    pub fn code(&self) -> i16 {
        match self {
            Self::Active => 0,
            Self::Suspended => 1,
            Self::Banned { .. } => 2,
        }
    }
}

/// 账号聚合根:登录凭证与账号级状态,不承载玩法数据。
#[derive(Debug, Clone)]
pub struct Account {
    id: AccountId,
    username: Username,
    password_hash: PasswordHash,
    status: AccountStatus,
    created_at: DateTime<Utc>,
}

impl Account {
    /// 新注册账号:由用例在密码哈希完成后调用。
    pub fn register(username: Username, password_hash: PasswordHash, now: DateTime<Utc>) -> Self {
        Self {
            id: AccountId(uuid::Uuid::now_v7()),
            username,
            password_hash,
            status: AccountStatus::Active,
            created_at: now,
        }
    }

    /// 由仓储从持久化数据重建聚合(不做业务校验)。
    #[allow(clippy::too_many_arguments)]
    pub fn reconstitute(
        id: AccountId,
        username: Username,
        password_hash: PasswordHash,
        status: AccountStatus,
        created_at: DateTime<Utc>,
    ) -> Self {
        Self {
            id,
            username,
            password_hash,
            status,
            created_at,
        }
    }

    pub fn id(&self) -> AccountId {
        self.id
    }

    pub fn username(&self) -> &Username {
        &self.username
    }

    pub fn password_hash(&self) -> &PasswordHash {
        &self.password_hash
    }

    pub fn status(&self) -> &AccountStatus {
        &self.status
    }

    pub fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }

    pub fn suspend(&mut self) {
        self.status = AccountStatus::Suspended;
    }

    pub fn ban(&mut self, reason: String, until: Option<DateTime<Utc>>) {
        self.status = AccountStatus::Banned { reason, until };
    }

    pub fn activate(&mut self) {
        self.status = AccountStatus::Active;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::value::Username;

    fn fixture() -> Account {
        let name = Username::try_new("alice").unwrap();
        Account::register(name, PasswordHash::new("hash".into()), Utc::now())
    }

    #[test]
    fn register_creates_active_account_with_v7_id() {
        let account = fixture();
        assert_eq!(account.status(), &AccountStatus::Active);
        assert!(account.id().0.get_version_num() == 7);
    }

    #[test]
    fn banned_until_expiry_restores_login() {
        let now = Utc::now();
        let expired = now - chrono::Duration::hours(1);
        let future = now + chrono::Duration::hours(1);

        assert!(AccountStatus::Active.allows_login(now));
        assert!(!AccountStatus::Suspended.allows_login(now));
        assert!(
            !AccountStatus::Banned {
                reason: "作弊".into(),
                until: Some(future)
            }
            .allows_login(now)
        );
        assert!(
            AccountStatus::Banned {
                reason: "作弊".into(),
                until: Some(expired)
            }
            .allows_login(now)
        );
        assert!(
            !AccountStatus::Banned {
                reason: "永久".into(),
                until: None
            }
            .allows_login(now)
        );
    }

    #[test]
    fn status_codes_and_lifecycle() {
        let mut account = fixture();
        assert_eq!(account.status().code(), 0);
        account.ban("外挂".into(), None);
        assert_eq!(account.status().code(), 2);
        account.activate();
        assert_eq!(account.status().code(), 0);
        account.suspend();
        assert_eq!(account.status().code(), 1);
    }
}
