//! 测试替身(仅 #[cfg(test)] 编译):内存版端口与仓储,供各用例单测复用。

use crate::error::AppError;
use crate::ports::{AuditLogger, PasswordHasher, SessionTokenStore};
use async_trait::async_trait;
use chrono::Utc;
use longshipx_domain::{
    Account, AccountId, AccountRepository, AccountStatus, Nickname, PasswordHash, Player, PlayerId,
    PlayerRepository, RepoError, Username,
};
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

pub const VALID_PASSWORD: &str = "super-secret";

/// 从用户名确定性派生 AccountId(测试专用,保证跨替身一致)。
pub fn deterministic_account_id(username: &str) -> AccountId {
    fn fnv(bytes: &[u8], seed: u64) -> u64 {
        let mut hash = 0xcbf2_9ce4_8422_2325_u64 ^ seed;
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x1000_0000_01b3);
        }
        hash
    }
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&fnv(username.as_bytes(), 0x015f).to_be_bytes());
    bytes[8..].copy_from_slice(&fnv(username.as_bytes(), 0x9e37).to_be_bytes());
    AccountId(uuid::Uuid::from_bytes(bytes))
}

/// 恒定哈希器:hash 输出可逆格式,verify 按格式比对。
pub struct FakeHasher;

impl PasswordHasher for FakeHasher {
    fn hash(&self, password: &str) -> Result<String, AppError> {
        Ok(format!("fake$$${password}"))
    }

    fn verify(&self, password: &str, hash: &str) -> Result<bool, AppError> {
        Ok(hash == format!("fake$$${password}"))
    }
}

/// 始终失败的哈希器:验证用例的故障路径。
pub struct FailingHasher;

impl PasswordHasher for FailingHasher {
    fn hash(&self, _password: &str) -> Result<String, AppError> {
        Err(AppError::Internal("hasher unavailable".into()))
    }

    fn verify(&self, _password: &str, _hash: &str) -> Result<bool, AppError> {
        Err(AppError::Internal("hasher unavailable".into()))
    }
}

/// 内存账号仓储。
#[derive(Default)]
pub struct FakeAccounts {
    inner: Mutex<HashMap<String, Account>>,
}

impl FakeAccounts {
    pub fn with_status(username: &str, status: AccountStatus) -> Self {
        let name = Username::try_new(username).unwrap();
        let mut account = Account::reconstitute(
            deterministic_account_id(username),
            name,
            PasswordHash::new(format!("fake$$${VALID_PASSWORD}")),
            AccountStatus::Active,
            Utc::now(),
        );
        match status {
            AccountStatus::Active => {},
            AccountStatus::Suspended => account.suspend(),
            AccountStatus::Banned { reason, until } => account.ban(reason, until),
        }
        Self {
            inner: Mutex::new(HashMap::from([(username.to_string(), account)])),
        }
    }

    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().len()
    }
}

#[async_trait]
impl AccountRepository for FakeAccounts {
    async fn find_by_id(&self, id: AccountId) -> Result<Option<Account>, RepoError> {
        Ok(self
            .inner
            .lock()
            .unwrap()
            .values()
            .find(|account| account.id() == id)
            .cloned())
    }

    async fn find_by_username(&self, username: &str) -> Result<Option<Account>, RepoError> {
        Ok(self.inner.lock().unwrap().get(username).cloned())
    }

    async fn save(&self, account: &Account) -> Result<(), RepoError> {
        self.inner
            .lock()
            .unwrap()
            .insert(account.username().as_str().to_string(), account.clone());
        Ok(())
    }
}

/// 内存玩家仓储(与 FakeAccounts 通过确定性 AccountId 关联)。
#[derive(Default)]
pub struct FakePlayers {
    inner: Mutex<HashMap<uuid::Uuid, Player>>,
}

impl FakePlayers {
    pub fn for_account(username: &str) -> Self {
        let nickname = Nickname::try_new(&format!("玩家_{username}")).unwrap();
        let player = Player::create(deterministic_account_id(username), nickname, Utc::now());
        Self {
            inner: Mutex::new(HashMap::from([(player.id().0, player)])),
        }
    }
}

#[async_trait]
impl PlayerRepository for FakePlayers {
    async fn find_by_id(&self, id: PlayerId) -> Result<Option<Player>, RepoError> {
        Ok(self.inner.lock().unwrap().get(&id.0).cloned())
    }

    async fn find_by_account(&self, account_id: AccountId) -> Result<Option<Player>, RepoError> {
        Ok(self
            .inner
            .lock()
            .unwrap()
            .values()
            .find(|player| player.account_id() == account_id)
            .cloned())
    }

    async fn save(&self, player: &Player) -> Result<(), RepoError> {
        self.inner
            .lock()
            .unwrap()
            .insert(player.id().0, player.clone());
        Ok(())
    }
}

#[derive(Default)]
struct FakeTokenState {
    by_token: HashMap<String, (PlayerId, Instant, Duration)>,
    by_player: HashMap<PlayerId, Vec<String>>,
    revoked: Vec<PlayerId>,
}

/// 内存 token 存储:支持 TTL 过期判定与玩家级吊销。
#[derive(Default)]
pub struct FakeTokens {
    inner: Mutex<FakeTokenState>,
}

impl FakeTokens {
    pub fn seed(&self, player_id: PlayerId) {
        let mut state = self.inner.lock().unwrap();
        let token = format!("seeded-{}", player_id.0);
        state.by_token.insert(
            token.clone(),
            (player_id, Instant::now(), Duration::from_secs(60)),
        );
        state.by_player.entry(player_id).or_default().push(token);
    }

    pub fn issued(&self) -> Vec<String> {
        self.inner
            .lock()
            .unwrap()
            .by_token
            .keys()
            .cloned()
            .collect()
    }

    pub fn revoked_players(&self) -> Vec<PlayerId> {
        self.inner.lock().unwrap().revoked.clone()
    }
}

#[async_trait]
impl SessionTokenStore for FakeTokens {
    async fn create(&self, player_id: PlayerId, ttl: Duration) -> Result<String, AppError> {
        let token = format!("token-{}", uuid::Uuid::now_v7());
        let mut state = self.inner.lock().unwrap();
        state
            .by_token
            .insert(token.clone(), (player_id, Instant::now(), ttl));
        state
            .by_player
            .entry(player_id)
            .or_default()
            .push(token.clone());
        Ok(token)
    }

    async fn resolve(&self, token: &str) -> Result<Option<PlayerId>, AppError> {
        let state = self.inner.lock().unwrap();
        Ok(state
            .by_token
            .get(token)
            .and_then(|(player, issued, ttl)| (issued.elapsed() <= *ttl).then_some(*player)))
    }

    async fn revoke_player(&self, player_id: PlayerId) -> Result<(), AppError> {
        let mut state = self.inner.lock().unwrap();
        if let Some(tokens) = state.by_player.remove(&player_id) {
            for token in tokens {
                state.by_token.remove(&token);
            }
        }
        state.revoked.push(player_id);
        Ok(())
    }
}

/// 审计记录收集器。
#[derive(Default)]
pub struct FakeAudit {
    pub count: AtomicUsize,
    pub records: Mutex<Vec<(Option<PlayerId>, String)>>,
}

#[async_trait]
impl AuditLogger for FakeAudit {
    async fn record(
        &self,
        player_id: Option<PlayerId>,
        action: &str,
        detail: String,
    ) -> Result<(), AppError> {
        self.count.fetch_add(1, Ordering::Relaxed);
        self.records
            .lock()
            .unwrap()
            .push((player_id, format!("{action}:{detail}")));
        Ok(())
    }
}
