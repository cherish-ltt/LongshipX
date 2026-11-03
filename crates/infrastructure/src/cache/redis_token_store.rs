//! Redis token 存储(PRD 7.5):`token:{token} → player_id` + 玩家级反向索引,
//! 支持跨重启存活与立即吊销。

use async_trait::async_trait;
use longshipx_application::error::AppError;
use longshipx_application::ports::SessionTokenStore;
use longshipx_domain::PlayerId;
use redis::AsyncCommands;
use redis::aio::MultiplexedConnection;
use std::time::Duration;
use uuid::Uuid;

/// Redis 实现的会话令牌存储。
#[derive(Clone)]
pub struct RedisTokenStore {
    connection: MultiplexedConnection,
}

impl RedisTokenStore {
    /// 连接 Redis 并构建存储;`REDIS_CONNECT_TIMEOUT_SECS` 控制建连超时。
    pub async fn connect(url: &str, timeout: Duration) -> Result<Self, AppError> {
        let client = redis::Client::open(url.to_string())
            .map_err(|err| AppError::Internal(format!("Redis URL 非法: {err}")))?;
        let connection = tokio::time::timeout(timeout, client.get_multiplexed_async_connection())
            .await
            .map_err(|_| AppError::Internal("Redis 连接超时".into()))?
            .map_err(|err| AppError::Internal(format!("Redis 连接失败: {err}")))?;
        Ok(Self { connection })
    }

    pub fn token_key(token: &str) -> String {
        format!("token:{token}")
    }

    pub fn player_index_key(player_id: PlayerId) -> String {
        format!("ptokens:{}", player_id.0)
    }
}

#[async_trait]
impl SessionTokenStore for RedisTokenStore {
    async fn create(&self, player_id: PlayerId, ttl: Duration) -> Result<String, AppError> {
        let token = random_token();
        let ttl_ms = u64::try_from(ttl.as_millis())
            .ok()
            .filter(|ttl| *ttl <= i64::MAX as u64 / 2)
            .ok_or_else(|| AppError::Validation("token TTL 过大".into()))?
            .max(1);
        let mut conn = self.connection.clone();
        let _: () = redis::Script::new(
            r"
            local old = redis.call('SMEMBERS', KEYS[1])
            for _, token in ipairs(old) do
                redis.call('DEL', 'token:' .. token)
            end
            redis.call('DEL', KEYS[1])
            redis.call('SADD', KEYS[1], ARGV[3])
            redis.call('PEXPIRE', KEYS[1], ARGV[2])
            redis.call('SET', KEYS[2], ARGV[1], 'PX', ARGV[2])
            ",
        )
        .key(Self::player_index_key(player_id))
        .key(Self::token_key(&token))
        .arg(player_id.0.to_string())
        .arg(ttl_ms)
        .arg(&token)
        .invoke_async(&mut conn)
        .await
        .map_err(map_redis)?;
        Ok(token)
    }

    async fn resolve(&self, token: &str) -> Result<Option<PlayerId>, AppError> {
        let mut conn = self.connection.clone();
        let raw: Option<String> = conn.get(Self::token_key(token)).await.map_err(map_redis)?;
        let Some(raw) = raw else {
            return Ok(None);
        };
        let uuid = Uuid::parse_str(&raw)
            .map_err(|err| AppError::Internal(format!("token 存储数据损坏: {err}")))?;
        Ok(Some(PlayerId(uuid)))
    }

    async fn revoke_player(&self, player_id: PlayerId) -> Result<(), AppError> {
        let mut conn = self.connection.clone();
        let _: () = redis::Script::new(
            r"
            local tokens = redis.call('SMEMBERS', KEYS[1])
            for _, token in ipairs(tokens) do
                redis.call('DEL', 'token:' .. token)
            end
            redis.call('DEL', KEYS[1])
            ",
        )
        .key(Self::player_index_key(player_id))
        .invoke_async(&mut conn)
        .await
        .map_err(map_redis)?;
        Ok(())
    }
}

fn map_redis(err: redis::RedisError) -> AppError {
    AppError::Storage(format!("Redis 命令失败: {err}"))
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

#[cfg(test)]
mod tests {
    use super::*;
    use longshipx_domain::PlayerId;

    #[test]
    fn keys_are_prefixed_and_scoped() {
        assert_eq!(RedisTokenStore::token_key("abc"), "token:abc");
        let player = PlayerId(uuid::Uuid::now_v7());
        assert!(RedisTokenStore::player_index_key(player).starts_with("ptokens:"));
    }

    #[test]
    fn connect_rejects_malformed_url_without_blocking() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let result = runtime.block_on(async {
            RedisTokenStore::connect("not-a-redis-url", Duration::from_millis(100)).await
        });
        assert!(matches!(result, Err(AppError::Internal(_))));
    }

    #[test]
    fn connect_times_out_when_unreachable() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let result = runtime.block_on(async {
            // 192.0.2.0/24 为 TEST-NET-1,不会真的建立连接。
            RedisTokenStore::connect("redis://192.0.2.1:6379", Duration::from_millis(200)).await
        });
        assert!(result.is_err());
    }
}
