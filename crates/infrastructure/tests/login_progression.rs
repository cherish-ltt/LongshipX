use async_trait::async_trait;
use chrono::Utc;
use longshipx_application::auth::LoginDependencies;
use longshipx_application::ports::{
    AuditLogger, EventPublisher, PasswordHasher, SessionTokenStore,
};
use longshipx_application::{AppError, GainExpUseCase, LoginCommand, LoginUseCase};
use longshipx_domain::{
    Account, AccountId, AccountRepository, DomainEvent, Nickname, PasswordHash, Player, PlayerId,
    PlayerRepository, RepoError, Username,
};
use longshipx_infrastructure::cache::InMemoryTokenStore;
use longshipx_infrastructure::persistence::repositories::{
    InMemoryAccountRepository, InMemoryPlayerRepository,
};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Barrier;

struct Hasher;
#[async_trait::async_trait]
impl PasswordHasher for Hasher {
    async fn hash(&self, password: &str) -> Result<String, AppError> {
        Ok(format!("test:{password}"))
    }
    async fn verify(&self, password: &str, hash: &str) -> Result<bool, AppError> {
        Ok(hash == format!("test:{password}"))
    }
}
struct Audit;
#[async_trait]
impl AuditLogger for Audit {
    async fn record(&self, _: Option<PlayerId>, _: &str, _: String) -> Result<(), AppError> {
        Ok(())
    }
}

#[derive(Default)]
struct Events(Mutex<Vec<DomainEvent>>);
#[async_trait]
impl EventPublisher for Events {
    async fn publish(&self, event: DomainEvent) -> Result<(), AppError> {
        self.0.lock().unwrap().push(event);
        Ok(())
    }
}
fn login_command() -> LoginCommand {
    LoginCommand {
        username: "audit_alice".into(),
        password: "audit-password".into(),
    }
}
async fn account_and_player() -> (
    Arc<InMemoryAccountRepository>,
    Arc<InMemoryPlayerRepository>,
    Player,
) {
    let accounts = Arc::new(InMemoryAccountRepository::new());
    let players = Arc::new(InMemoryPlayerRepository::new());
    let account = Account::register(
        Username::try_new("audit_alice").unwrap(),
        PasswordHash::new("test:audit-password".into()),
        Utc::now(),
    );
    let player = Player::create(
        account.id(),
        Nickname::try_new("Alice").unwrap(),
        Utc::now(),
    );
    accounts.save(&account).await.unwrap();
    players.save(&player).await.unwrap();
    (accounts, players, player)
}
fn login(
    accounts: Arc<InMemoryAccountRepository>,
    players: Arc<dyn PlayerRepository>,
    tokens: Arc<dyn SessionTokenStore>,
) -> LoginUseCase {
    LoginUseCase::new(
        LoginDependencies {
            accounts,
            players,
            tokens,
            hasher: Arc::new(Hasher),
            audit: Arc::new(Audit),
        },
        Duration::from_secs(60),
    )
}

struct PausedLoginPlayers {
    inner: Arc<InMemoryPlayerRepository>,
    loaded: Arc<Barrier>,
    resume: Arc<tokio::sync::Notify>,
}
#[async_trait]
impl PlayerRepository for PausedLoginPlayers {
    async fn find_by_id(&self, id: PlayerId) -> Result<Option<Player>, RepoError> {
        self.inner.find_by_id(id).await
    }
    async fn find_by_account(&self, id: AccountId) -> Result<Option<Player>, RepoError> {
        let player = self.inner.find_by_account(id).await?;
        self.loaded.wait().await;
        self.resume.notified().await;
        Ok(player)
    }
    async fn save(&self, player: &Player) -> Result<(), RepoError> {
        self.inner.save(player).await
    }
    async fn gain_exp(
        &self,
        id: PlayerId,
        amount: u64,
    ) -> Result<Option<(Player, Option<u32>)>, RepoError> {
        self.inner.gain_exp(id, amount).await
    }
    async fn record_login(
        &self,
        id: PlayerId,
        at: chrono::DateTime<Utc>,
    ) -> Result<Option<Player>, RepoError> {
        self.inner.record_login(id, at).await
    }
}
#[tokio::test]
async fn login_must_not_overwrite_experience_awarded_after_its_read() {
    let (accounts, players, player) = account_and_player().await;
    let loaded = Arc::new(Barrier::new(2));
    let resume = Arc::new(tokio::sync::Notify::new());
    let paused = Arc::new(PausedLoginPlayers {
        inner: players.clone(),
        loaded: loaded.clone(),
        resume: resume.clone(),
    });
    let use_case = login(accounts, paused, Arc::new(InMemoryTokenStore::new()));
    let login_task = tokio::spawn(async move { use_case.execute(login_command()).await });
    loaded.wait().await;
    let awarded = GainExpUseCase::new(players.clone(), Arc::new(Events::default()))
        .execute(player.id(), 50)
        .await
        .unwrap();
    resume.notify_one();
    assert!(login_task.await.unwrap().is_ok());
    let saved = players.find_by_id(player.id()).await.unwrap().unwrap();
    eprintln!(
        "awarded_exp={}; after_successful_login_exp={}",
        awarded.exp,
        saved.exp()
    );
    assert_eq!(
        saved.exp(),
        50,
        "login full-row save overwrites the newer experience"
    );
}
