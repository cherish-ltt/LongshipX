use longshipx_application::{AppError, ports::RegistrationStore};
use longshipx_domain::{
    Account, AccountRepository, Nickname, PasswordHash, Player, PlayerRepository, Username,
};
use longshipx_infrastructure::persistence::repositories::{
    InMemoryAccountRepository, InMemoryPlayerRepository, InMemoryRegistrationStore,
    SeaAccountRepository, SeaPlayerRepository, SeaRegistrationStore,
};
use std::sync::Arc;

fn pair(username: &str, nickname: &str) -> (Account, Player) {
    let account = Account::register(
        Username::try_new(username).unwrap(),
        PasswordHash::new("test".into()),
        chrono::Utc::now(),
    );
    let player = Player::create(
        account.id(),
        Nickname::try_new(nickname).unwrap(),
        chrono::Utc::now(),
    );
    (account, player)
}

async fn concurrent_registration(
    store: &dyn RegistrationStore,
    accounts: &dyn AccountRepository,
    players: &dyn PlayerRepository,
) {
    let username = format!("u{}", &uuid::Uuid::now_v7().simple().to_string()[..20]);
    let (a, p) = pair(&username, "first");
    let (b, q) = pair(&username, "second");
    let (first, second) = tokio::join!(store.register(&a, &p), store.register(&b, &q));
    assert!(matches!(
        (&first, &second),
        (Ok(()), Err(AppError::Conflict(_))) | (Err(AppError::Conflict(_)), Ok(()))
    ));
    let winner = accounts.find_by_username(&username).await.unwrap().unwrap();
    assert!(
        players
            .find_by_account(winner.id())
            .await
            .unwrap()
            .is_some()
    );
    let loser = if winner.id() == a.id() { &b } else { &a };
    assert!(accounts.find_by_id(loser.id()).await.unwrap().is_none());
    assert!(players.find_by_account(loser.id()).await.unwrap().is_none());
}

#[tokio::test]
async fn memory_registration_is_atomic_and_unique() {
    let accounts = Arc::new(InMemoryAccountRepository::new());
    let players = Arc::new(InMemoryPlayerRepository::new());
    let store = InMemoryRegistrationStore::new(accounts.clone(), players.clone());
    concurrent_registration(&store, accounts.as_ref(), players.as_ref()).await;
}

#[tokio::test]
#[ignore = "requires dedicated LONGSHIPX_TEST_PG_URL"]
async fn postgres_registration_rolls_back_and_is_unique() {
    use sea_orm::{ConnectionTrait, Database, TransactionTrait};
    let url =
        std::env::var("LONGSHIPX_TEST_PG_URL").expect("dedicated PostgreSQL database required");
    let db = Arc::new(Database::connect(&url).await.unwrap());
    db.execute_unprepared("CREATE EXTENSION IF NOT EXISTS citext")
        .await
        .unwrap();
    longshipx_migration::run_migrations(db.as_ref())
        .await
        .unwrap();
    let accounts = SeaAccountRepository::new(db.clone());
    let players = SeaPlayerRepository::new(db.clone());
    let store = Arc::new(SeaRegistrationStore::new(db.clone()));
    concurrent_registration(store.as_ref(), &accounts, &players).await;

    db.execute_unprepared("ALTER TABLE players ADD CONSTRAINT registration_fault CHECK (nickname <> 'injected-failure') NOT VALID").await.unwrap();
    let (account, rejected) = pair(
        &format!("r{}", &uuid::Uuid::now_v7().simple().to_string()[..20]),
        "injected-failure",
    );
    let failure = store.register(&account, &rejected).await;
    db.execute_unprepared("ALTER TABLE players DROP CONSTRAINT registration_fault")
        .await
        .unwrap();
    assert!(matches!(failure, Err(AppError::Storage(_))));
    assert!(accounts.find_by_id(account.id()).await.unwrap().is_none());
    assert!(players.find_by_id(rejected.id()).await.unwrap().is_none());
    store.register(&account, &rejected).await.unwrap();
    assert!(
        players
            .find_by_account(account.id())
            .await
            .unwrap()
            .is_some()
    );

    // 在第二条写入被数据库锁阻塞时取消任务,检查第一条写入也被回滚。
    let blocker = db.begin().await.unwrap();
    blocker
        .execute_unprepared("LOCK TABLE players IN ACCESS EXCLUSIVE MODE")
        .await
        .unwrap();
    let (cancelled, player) = pair(
        &format!("c{}", &uuid::Uuid::now_v7().simple().to_string()[..20]),
        "cancelled",
    );
    let pending = tokio::spawn({
        let store = store.clone();
        let account = cancelled.clone();
        async move { store.register(&account, &player).await }
    });
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let result = db.query_one_raw(sea_orm::Statement::from_string(sea_orm::DbBackend::Postgres,
                "SELECT count(*)::bigint AS count FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE 'INSERT INTO \"players\"%'".to_string())).await.unwrap().unwrap();
            if result.try_get::<i64>("", "count").unwrap() > 0 { break; }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    pending.abort();
    assert!(pending.await.unwrap_err().is_cancelled());
    blocker.rollback().await.unwrap();
    assert!(accounts.find_by_id(cancelled.id()).await.unwrap().is_none());
    assert!(
        players
            .find_by_account(cancelled.id())
            .await
            .unwrap()
            .is_none()
    );
}
