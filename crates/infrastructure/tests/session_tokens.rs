use longshipx_application::ports::SessionTokenStore;
use longshipx_domain::PlayerId;
use longshipx_infrastructure::cache::{InMemoryTokenStore, RedisTokenStore};
use std::time::Duration;

async fn replacement_contract(store: &dyn SessionTokenStore) {
    let player = PlayerId(uuid::Uuid::now_v7());
    let other = PlayerId(uuid::Uuid::now_v7());
    let other_token = store.create(other, Duration::from_secs(60)).await.unwrap();
    let old = store.create(player, Duration::from_secs(60)).await.unwrap();
    let (first, second) = tokio::join!(
        store.create(player, Duration::from_secs(60)),
        store.create(player, Duration::from_secs(60))
    );
    let first = first.unwrap();
    let second = second.unwrap();
    let valid = usize::from(store.resolve(&first).await.unwrap().is_some())
        + usize::from(store.resolve(&second).await.unwrap().is_some());
    assert_eq!(
        valid, 1,
        "concurrent replacements must leave one live token"
    );
    assert_eq!(store.resolve(&old).await.unwrap(), None);
    assert_eq!(store.resolve(&other_token).await.unwrap(), Some(other));
    store.revoke_player(player).await.unwrap();
    assert_eq!(store.resolve(&first).await.unwrap(), None);
    assert_eq!(store.resolve(&second).await.unwrap(), None);
    assert_eq!(store.resolve(&other_token).await.unwrap(), Some(other));
    store.revoke_player(other).await.unwrap();
}

#[tokio::test]
async fn memory_replacement_is_atomic_and_player_scoped() {
    replacement_contract(&InMemoryTokenStore::new()).await;
}

#[tokio::test]
#[ignore = "requires dedicated LONGSHIPX_TEST_REDIS_URL"]
async fn redis_replacement_is_atomic_and_player_scoped() {
    let url = std::env::var("LONGSHIPX_TEST_REDIS_URL").expect("dedicated Redis URL required");
    let store = RedisTokenStore::connect(&url, Duration::from_secs(3))
        .await
        .unwrap();
    replacement_contract(&store).await;
}

#[tokio::test]
#[ignore = "requires dedicated LONGSHIPX_TEST_REDIS_URL"]
async fn redis_replacement_failure_does_not_issue_an_extra_token() {
    use redis::AsyncCommands;
    let url = std::env::var("LONGSHIPX_TEST_REDIS_URL").unwrap();
    let store = RedisTokenStore::connect(&url, Duration::from_secs(3))
        .await
        .unwrap();
    let mut conn = redis::Client::open(url)
        .unwrap()
        .get_multiplexed_async_connection()
        .await
        .unwrap();
    let player = PlayerId(uuid::Uuid::now_v7());
    let old = store.create(player, Duration::from_secs(60)).await.unwrap();
    let index = RedisTokenStore::player_index_key(player);
    let _: () = conn.set(&index, "wrong-type").await.unwrap();
    assert!(store.create(player, Duration::from_secs(60)).await.is_err());
    assert_eq!(store.resolve(&old).await.unwrap(), Some(player));
    let _: () = conn.del(&index).await.unwrap();
    let _: () = conn.sadd(&index, &old).await.unwrap();
    let new = store.create(player, Duration::from_secs(60)).await.unwrap();
    assert_eq!(store.resolve(&old).await.unwrap(), None);
    assert_eq!(store.resolve(&new).await.unwrap(), Some(player));
    assert!(store.create(player, Duration::MAX).await.is_err());
    assert_eq!(store.resolve(&new).await.unwrap(), Some(player));
    store.revoke_player(player).await.unwrap();
}
