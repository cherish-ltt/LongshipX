use longshipx_application::{AppError, GainExpUseCase, ports::EventPublisher};
use longshipx_domain::{
    Account, AccountRepository, DomainEvent, Nickname, PasswordHash, Player, PlayerRepository,
    Username,
};
use longshipx_infrastructure::persistence::repositories::{
    InMemoryPlayerRepository, SeaAccountRepository, SeaPlayerRepository,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Default)]
struct Events(AtomicUsize);
#[async_trait::async_trait]
impl EventPublisher for Events {
    async fn publish(&self, _: DomainEvent) -> Result<(), AppError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

async fn update_contract(players: Arc<dyn PlayerRepository>, player: Player) {
    let id = player.id();
    players.save(&player).await.unwrap();
    let events = Arc::new(Events::default());
    let gain = Arc::new(GainExpUseCase::new(players.clone(), events.clone()));
    let mut tasks = tokio::task::JoinSet::new();
    let gate = Arc::new(tokio::sync::Barrier::new(21));
    for _ in 0..20 {
        let (gain, gate) = (gain.clone(), gate.clone());
        tasks.spawn(async move {
            gate.wait().await;
            gain.execute(id, 5).await.unwrap();
        });
    }
    gate.wait().await;
    let login_at = chrono::Utc::now();
    players.record_login(id, login_at).await.unwrap().unwrap();
    while let Some(result) = tasks.join_next().await {
        result.unwrap();
    }
    let saved = players.find_by_id(id).await.unwrap().unwrap();
    assert_eq!((saved.level(), saved.exp()), (2, 0));
    assert_eq!(
        saved.last_login_at().unwrap().timestamp_micros(),
        login_at.timestamp_micros()
    );
    assert_eq!(events.0.load(Ordering::SeqCst), 1);
    let missing = longshipx_domain::PlayerId(uuid::Uuid::now_v7());
    assert!(
        players
            .record_login(missing, login_at)
            .await
            .unwrap()
            .is_none()
    );
    assert!(players.gain_exp(missing, 5).await.unwrap().is_none());
}

fn pair() -> (Account, Player) {
    let username = format!("u{}", &uuid::Uuid::now_v7().simple().to_string()[..20]);
    let account = Account::register(
        Username::try_new(&username).unwrap(),
        PasswordHash::new("test".into()),
        chrono::Utc::now(),
    );
    let player = Player::create(
        account.id(),
        Nickname::try_new("test").unwrap(),
        chrono::Utc::now(),
    );
    (account, player)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn memory_updates_preserve_experience_and_login() {
    update_contract(Arc::new(InMemoryPlayerRepository::new()), pair().1).await;
}

#[tokio::test]
#[ignore = "requires dedicated LONGSHIPX_TEST_PG_URL"]
async fn postgres_updates_preserve_experience_and_failed_writes_publish_nothing() {
    use sea_orm::{ConnectionTrait, Database};
    let db = Arc::new(
        Database::connect(std::env::var("LONGSHIPX_TEST_PG_URL").unwrap())
            .await
            .unwrap(),
    );
    db.execute_unprepared("CREATE EXTENSION IF NOT EXISTS citext")
        .await
        .unwrap();
    longshipx_migration::run_migrations(db.as_ref())
        .await
        .unwrap();
    let accounts = SeaAccountRepository::new(db.clone());
    let players = Arc::new(SeaPlayerRepository::new(db.clone()));
    let (account, player) = pair();
    accounts.save(&account).await.unwrap();
    update_contract(players.clone(), player).await;

    let (account, player) = pair();
    accounts.save(&account).await.unwrap();
    players.save(&player).await.unwrap();
    db.execute_unprepared(&format!("ALTER TABLE players ADD CONSTRAINT progression_fault CHECK (id <> '{}' OR level=1) NOT VALID", player.id())).await.unwrap();
    let events = Arc::new(Events::default());
    let gain = GainExpUseCase::new(players.clone(), events.clone());
    let failure = gain.execute(player.id(), 100).await;
    db.execute_unprepared("ALTER TABLE players DROP CONSTRAINT progression_fault")
        .await
        .unwrap();
    assert!(matches!(failure, Err(AppError::Storage(_))));
    assert_eq!(events.0.load(Ordering::SeqCst), 0);
    let saved = players.find_by_id(player.id()).await.unwrap().unwrap();
    assert_eq!((saved.level(), saved.exp()), (1, 0));
    assert_eq!(gain.execute(player.id(), 100).await.unwrap().level, 2);
}
