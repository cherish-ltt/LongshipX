//! 注册用例:创建账号 + 玩家档案(1:1),密码只以 argon2id 哈希形态落库。

use crate::dto::{RegisterCommand, RegisterResult};
use crate::error::AppError;
use crate::ports::{AuditLogger, PasswordHasher};
use longshipx_domain::shared::value::{Nickname, PasswordHash, PlainPassword, Username};
use longshipx_domain::{Account, AccountRepository, Player, PlayerRepository, RepoError};
use std::sync::Arc;

pub struct RegisterUseCase {
    accounts: Arc<dyn AccountRepository>,
    players: Arc<dyn PlayerRepository>,
    hasher: Arc<dyn PasswordHasher>,
    audit: Arc<dyn AuditLogger>,
}

impl RegisterUseCase {
    pub fn new(
        accounts: Arc<dyn AccountRepository>,
        players: Arc<dyn PlayerRepository>,
        hasher: Arc<dyn PasswordHasher>,
        audit: Arc<dyn AuditLogger>,
    ) -> Self {
        Self {
            accounts,
            players,
            hasher,
            audit,
        }
    }

    pub async fn execute(&self, cmd: RegisterCommand) -> Result<RegisterResult, AppError> {
        let username = Username::try_new(&cmd.username)?;
        let nickname = Nickname::try_new(&cmd.nickname)?;
        let password = PlainPassword::try_new(&cmd.password)?;

        if self
            .accounts
            .find_by_username(username.as_str())
            .await?
            .is_some()
        {
            return Err(AppError::Conflict("用户名已被占用".into()));
        }

        let hash = PasswordHash::new(self.hasher.hash(password.expose())?);
        let account = Account::register(username, hash, chrono::Utc::now());
        let player = Player::create(account.id(), nickname, chrono::Utc::now());

        self.accounts.save(&account).await?;
        self.players.save(&player).await?;
        self.audit_success(&player, account.username()).await;

        Ok(RegisterResult {
            account_id: account.id(),
            player_id: player.id(),
            nickname: player.nickname().as_str().to_string(),
        })
    }

    /// 审计失败不阻断注册主流程,仅记录告警(PRD 7.3:审计服务于排查)。
    async fn audit_success(&self, player: &Player, username: &Username) {
        let detail = format!("{{\"username\":\"{}\"}}", username.as_str());
        if let Err(err) = self
            .audit
            .record(Some(player.id()), "register", detail)
            .await
        {
            tracing::warn!(error = %err, "注册审计记录失败");
        }
    }

    /// 仓储冲突统一转换为应用层 Conflict(用户名唯一约束兜底)。
    #[allow(dead_code)]
    fn map_conflict(err: RepoError) -> AppError {
        match err {
            RepoError::Conflict(_) => AppError::Conflict("用户名已被占用".into()),
            other => other.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::error::AppError;
    use crate::fakes::{FailingHasher, FakeAccounts, FakeAudit, FakeHasher, FakePlayers};
    use crate::{RegisterCommand, RegisterUseCase};
    use std::sync::Arc;

    fn use_case() -> (RegisterUseCase, Arc<FakeAccounts>, Arc<FakeAudit>) {
        let accounts = Arc::new(FakeAccounts::default());
        let players = Arc::new(FakePlayers::default());
        let hasher = Arc::new(FakeHasher);
        let audit = Arc::new(FakeAudit::default());
        (
            RegisterUseCase::new(accounts.clone(), players, hasher, audit.clone()),
            accounts,
            audit,
        )
    }

    fn cmd() -> RegisterCommand {
        RegisterCommand {
            username: "alice_01".into(),
            password: "super-secret".into(),
            nickname: "爱丽丝".into(),
        }
    }

    #[tokio::test]
    async fn registers_account_and_player() {
        let (use_case, accounts, audit) = use_case();
        let result = use_case.execute(cmd()).await.unwrap();
        assert_eq!(result.nickname, "爱丽丝");
        assert_eq!(accounts.len(), 1);
        assert_eq!(audit.count.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn audit_records_normalized_username_and_player() {
        let (use_case, _, audit) = use_case();
        let mut command = cmd();
        command.username = "  ALICE_01  ".into();
        let result = use_case.execute(command).await.unwrap();
        let records = audit.records.lock().unwrap();
        assert_eq!(records[0].0, Some(result.player_id));
        assert_eq!(records[0].1, "register:{\"username\":\"alice_01\"}");
    }

    struct FailingAudit;

    #[async_trait::async_trait]
    impl crate::ports::AuditLogger for FailingAudit {
        async fn record(
            &self,
            _: Option<longshipx_domain::PlayerId>,
            _: &str,
            _: String,
        ) -> Result<(), AppError> {
            Err(AppError::Storage("audit unavailable".into()))
        }
    }

    #[tokio::test]
    async fn audit_failure_does_not_fail_registration() {
        let use_case = RegisterUseCase::new(
            Arc::new(FakeAccounts::default()),
            Arc::new(FakePlayers::default()),
            Arc::new(FakeHasher),
            Arc::new(FailingAudit),
        );
        assert!(use_case.execute(cmd()).await.is_ok());
    }

    #[tokio::test]
    async fn rejects_duplicate_username() {
        let (use_case, _, _) = use_case();
        use_case.execute(cmd()).await.unwrap();
        let err = use_case.execute(cmd()).await.unwrap_err();
        assert_eq!(err, AppError::Conflict("用户名已被占用".into()));
    }

    #[tokio::test]
    async fn validates_input_before_any_persistence() {
        let (use_case, accounts, _) = use_case();
        let bad = RegisterCommand {
            username: "a".into(),
            password: "super-secret".into(),
            nickname: "x".into(),
        };
        assert!(matches!(
            use_case.execute(bad).await,
            Err(AppError::Validation(_))
        ));
        let weak = RegisterCommand {
            username: "alice_01".into(),
            password: "short".into(),
            nickname: "x".into(),
        };
        assert!(matches!(
            use_case.execute(weak).await,
            Err(AppError::Validation(_))
        ));
        assert_eq!(accounts.len(), 0, "校验失败不应触发任何落库");
    }

    #[tokio::test]
    async fn hasher_failure_aborts_registration() {
        let accounts = Arc::new(FakeAccounts::default());
        let players = Arc::new(FakePlayers::default());
        let hasher = Arc::new(FailingHasher);
        let audit = Arc::new(FakeAudit::default());
        let use_case = RegisterUseCase::new(accounts, players, hasher, audit);
        let err = use_case.execute(cmd()).await.unwrap_err();
        assert!(matches!(err, AppError::Internal(_)));
    }
}
