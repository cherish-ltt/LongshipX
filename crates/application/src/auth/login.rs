//! 登录用例:验密 → 状态检查 → 单会话 token(旧 token 立即吊销)→ 更新登录时间。

use crate::dto::{LoginCommand, LoginResult};
use crate::error::AppError;
use crate::ports::{AuditLogger, PasswordHasher, SessionTokenStore};
use longshipx_domain::shared::value::Username;
use longshipx_domain::{AccountRepository, PlayerRepository};
use std::sync::Arc;
use std::time::Duration;

pub struct LoginUseCase {
    deps: LoginDependencies,
    token_ttl: Duration,
}

impl LoginUseCase {
    pub fn new(deps: LoginDependencies, token_ttl: Duration) -> Self {
        Self { token_ttl, deps }
    }

    pub async fn execute(&self, cmd: LoginCommand) -> Result<LoginResult, AppError> {
        let username = Username::try_new(&cmd.username)?;
        let account = self.load_account(username.as_str()).await?;
        self.ensure_login_allowed(&account)?;
        if !self
            .deps
            .hasher
            .verify(&cmd.password, account.password_hash().expose())?
        {
            return Err(AppError::Unauthorized("用户名或密码错误".into()));
        }

        let mut player = self.load_player(account.id()).await?;
        player.record_login(chrono::Utc::now());
        self.players_save(&player).await?;

        // 单会话策略:登录即吊销该玩家历史 token(支持"顶号下线",PRD 6.1)。
        if let Err(err) = self.deps.tokens.revoke_player(player.id()).await {
            tracing::warn!(error = %err, "吊销历史 token 失败,继续签发新 token");
        }
        let token = self.deps.tokens.create(player.id(), self.token_ttl).await?;
        self.audit_login(&player).await;

        Ok(LoginResult {
            token,
            player_id: player.id(),
            nickname: player.nickname().as_str().to_string(),
            expires_in_secs: self.token_ttl.as_secs(),
        })
    }

    async fn load_account(&self, username: &str) -> Result<longshipx_domain::Account, AppError> {
        // 统一返回"用户名或密码错误",不暴露用户是否存在(PRD 第 10 章)。
        self.deps
            .accounts
            .find_by_username(username)
            .await?
            .ok_or_else(|| AppError::Unauthorized("用户名或密码错误".into()))
    }

    fn ensure_login_allowed(&self, account: &longshipx_domain::Account) -> Result<(), AppError> {
        if account.status().allows_login(chrono::Utc::now()) {
            return Ok(());
        }
        Err(AppError::Forbidden("账号当前不可登录".into()))
    }

    async fn load_player(
        &self,
        account: longshipx_domain::AccountId,
    ) -> Result<longshipx_domain::Player, AppError> {
        self.deps
            .players
            .find_by_account(account)
            .await?
            .ok_or_else(|| AppError::Internal("账号缺少对应的玩家档案".into()))
    }

    async fn players_save(&self, player: &longshipx_domain::Player) -> Result<(), AppError> {
        Ok(self.deps.players.save(player).await?)
    }

    async fn audit_login(&self, player: &longshipx_domain::Player) {
        if let Err(err) = self
            .deps
            .audit
            .record(Some(player.id()), "login", "{}".into())
            .await
        {
            tracing::warn!(error = %err, "登录审计记录失败");
        }
    }
}

/// 登录用例依赖集合(避免构造函数参数过多)。
pub struct LoginDependencies {
    pub accounts: Arc<dyn AccountRepository>,
    pub players: Arc<dyn PlayerRepository>,
    pub tokens: Arc<dyn SessionTokenStore>,
    pub hasher: Arc<dyn PasswordHasher>,
    pub audit: Arc<dyn AuditLogger>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fakes::{FakeAccounts, FakeAudit, FakeHasher, FakePlayers, FakeTokens};
    use longshipx_domain::{AccountStatus, PlayerId};

    fn deps_with(account_status: AccountStatus) -> (LoginUseCase, Arc<FakeTokens>) {
        let accounts = Arc::new(FakeAccounts::with_status("alice", account_status));
        let players = Arc::new(FakePlayers::for_account("alice"));
        let tokens = Arc::new(FakeTokens::default());
        let hasher = Arc::new(FakeHasher);
        let audit = Arc::new(FakeAudit::default());
        let use_case = LoginUseCase::new(
            LoginDependencies {
                accounts,
                players,
                tokens: tokens.clone(),
                hasher,
                audit,
            },
            Duration::from_secs(60),
        );
        (use_case, tokens)
    }

    fn cmd() -> LoginCommand {
        LoginCommand {
            username: "Alice".into(),
            password: "super-secret".into(),
        }
    }

    #[tokio::test]
    async fn login_issues_token_and_records_login() {
        let (use_case, tokens) = deps_with(AccountStatus::Active);
        let result = use_case.execute(cmd()).await.unwrap();
        assert_eq!(result.expires_in_secs, 60);
        assert_eq!(tokens.issued().len(), 1);
        assert_eq!(result.nickname, "玩家_alice");
    }

    #[tokio::test]
    async fn wrong_password_is_generic_unauthorized() {
        let (use_case, tokens) = deps_with(AccountStatus::Active);
        let bad = LoginCommand {
            username: "alice".into(),
            password: "wrong-password".into(),
        };
        let err = use_case.execute(bad).await.unwrap_err();
        assert_eq!(err, AppError::Unauthorized("用户名或密码错误".into()));
        assert!(tokens.issued().is_empty());
    }

    #[tokio::test]
    async fn unknown_user_is_generic_unauthorized() {
        let (use_case, _) = deps_with(AccountStatus::Active);
        let ghost = LoginCommand {
            username: "nobody".into(),
            password: "super-secret".into(),
        };
        assert!(matches!(
            use_case.execute(ghost).await,
            Err(AppError::Unauthorized(_))
        ));
    }

    #[tokio::test]
    async fn banned_account_is_forbidden() {
        let (use_case, _) = deps_with(AccountStatus::Banned {
            reason: "作弊".into(),
            until: None,
        });
        let err = use_case.execute(cmd()).await.unwrap_err();
        assert!(matches!(err, AppError::Forbidden(_)));
    }

    #[tokio::test]
    async fn login_revokes_previous_tokens() {
        let (use_case, tokens) = deps_with(AccountStatus::Active);
        tokens.seed(PlayerId(uuid::Uuid::now_v7()));
        use_case.execute(cmd()).await.unwrap();
        assert!(!tokens.revoked_players().is_empty());
    }

    #[tokio::test]
    async fn invalid_username_shape_is_validation_error() {
        let (use_case, _) = deps_with(AccountStatus::Active);
        let bad = LoginCommand {
            username: "!!".into(),
            password: "super-secret".into(),
        };
        assert!(matches!(
            use_case.execute(bad).await,
            Err(AppError::Validation(_))
        ));
    }
}
