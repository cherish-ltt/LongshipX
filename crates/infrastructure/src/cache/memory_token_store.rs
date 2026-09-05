//! 内存版 token 存储:开发/测试与"Redis 不可用降级"场景使用。
//! ⚠️ 重启即失效,不满足跨重启存活的强需求时务必使用 Redis 实现。

use async_trait::async_trait;
use longshipx_application::error::AppError;
use longshipx_application::ports::SessionTokenStore;
use longshipx_domain::PlayerId;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

struct Entry {
    player_id: PlayerId,
    expires_at: Instant,
}

#[derive(Default)]
struct State {
    by_token: HashMap<String, Entry>,
    by_player: HashMap<PlayerId, Vec<String>>,
}

/// 内存 token 存储(Mutex 仅保护索引结构,操作为 O(1)/O(n))。
#[derive(Default)]
pub struct InMemoryTokenStore {
    state: Mutex<State>,
}

impl InMemoryTokenStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn live_tokens(&self) -> usize {
        let now = Instant::now();
        let mut state = self.state.lock().expect("token store 锁中毒");
        state.by_token.retain(|_, entry| entry.expires_at > now);
        state.by_token.len()
    }
}

fn random_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("读取系统熵源失败");
    let mut hex = String::with_capacity(64);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

#[async_trait]
impl SessionTokenStore for InMemoryTokenStore {
    async fn create(&self, player_id: PlayerId, ttl: Duration) -> Result<String, AppError> {
        let token = random_token();
        let mut state = self.state.lock().expect("token store 锁中毒");
        state
            .by_player
            .entry(player_id)
            .or_default()
            .push(token.clone());
        state.by_token.insert(
            token.clone(),
            Entry {
                player_id,
                expires_at: Instant::now() + ttl,
            },
        );
        Ok(token)
    }

    async fn resolve(&self, token: &str) -> Result<Option<PlayerId>, AppError> {
        let state = self.state.lock().expect("token store 锁中毒");
        Ok(state
            .by_token
            .get(token)
            .filter(|entry| entry.expires_at > Instant::now())
            .map(|entry| entry.player_id))
    }

    async fn revoke_player(&self, player_id: PlayerId) -> Result<(), AppError> {
        let mut state = self.state.lock().expect("token store 锁中毒");
        if let Some(tokens) = state.by_player.remove(&player_id) {
            for token in tokens {
                state.by_token.remove(&token);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn create_resolve_revoke_roundtrip() {
        let store = InMemoryTokenStore::new();
        let player = PlayerId(uuid::Uuid::now_v7());
        let token = store.create(player, Duration::from_secs(60)).await.unwrap();
        assert_eq!(store.resolve(&token).await.unwrap(), Some(player));
        assert_eq!(store.live_tokens(), 1);

        store.revoke_player(player).await.unwrap();
        assert_eq!(store.resolve(&token).await.unwrap(), None);
        assert_eq!(store.live_tokens(), 0);
    }

    #[tokio::test]
    async fn expired_token_resolves_to_none() {
        let store = InMemoryTokenStore::new();
        let player = PlayerId(uuid::Uuid::now_v7());
        let token = store.create(player, Duration::from_secs(0)).await.unwrap();
        tokio::time::sleep(Duration::from_millis(5)).await;
        assert_eq!(store.resolve(&token).await.unwrap(), None);
        assert_eq!(store.live_tokens(), 0);
    }

    #[tokio::test]
    async fn unknown_token_is_none() {
        let store = InMemoryTokenStore::new();
        assert_eq!(store.resolve("missing").await.unwrap(), None);
    }

    #[tokio::test]
    async fn tokens_are_unique_per_create() {
        let store = InMemoryTokenStore::new();
        let player = PlayerId(uuid::Uuid::now_v7());
        let first = store.create(player, Duration::from_secs(60)).await.unwrap();
        let second = store.create(player, Duration::from_secs(60)).await.unwrap();
        assert_ne!(first, second);
        assert_eq!(first.len(), 64);
    }
}
