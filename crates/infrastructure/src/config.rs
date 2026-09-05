//! 环境变量配置(PRD 18):代码内只有默认值,覆盖一律走环境变量/.env;
//! 🔴 敏感信息(DATABASE_URL 等)生产必须由环境注入,默认值仅是占位。
//!
//! 解析通过 `from_lookup` 注入读取器实现,便于测试与未来接入远程配置中心。

use std::fmt;
use std::time::Duration;

/// 全量配置:按功能分组,全部字段可被环境变量覆盖。
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub network: NetworkConfig,
    pub rate_limit: RateLimitConfig,
    pub tls: TlsConfig,
    pub database: DatabaseConfig,
    pub redis: RedisConfig,
    pub log: LogConfig,
    pub app: AppConfig,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NetworkConfig {
    pub tcp_bind_addr: String,
    pub http_bind_addr: String,
    pub wss_bind_addr: String,
    pub max_connections: usize,
    pub backlog: u32,
    pub read_buffer_size: usize,
    pub write_buffer_size: usize,
    pub channel_per_conn: usize,
    pub channel_per_room: usize,
    pub max_frame_size: usize,
    pub heartbeat_interval_secs: u64,
    pub heartbeat_timeout_secs: u64,
    pub unauth_timeout_secs: u64,
    pub unauth_max_per_ip: usize,
    pub tcp_nodelay: bool,
    pub tcp_keepalive_secs: u64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RateLimitConfig {
    pub per_conn: u64,
    pub burst: u64,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TlsConfig {
    pub cert_path: String,
    pub key_path: String,
    pub ca_path: Option<String>,
    /// 固定 TLS 1.3;其他取值在解析期直接报错(PRD 第 10 章 🔴 禁止降级)。
    pub min_version: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DatabaseConfig {
    pub url: String,
    pub max_connections: u32,
    pub min_connections: u32,
    pub connect_timeout_secs: u64,
    pub idle_timeout_secs: u64,
    pub sqlx_log_level: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RedisConfig {
    pub url: String,
    pub pool_max_size: usize,
    pub default_ttl_secs: u64,
    pub connect_timeout_secs: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LogConfig {
    pub level: String,
    pub format: String,
    pub otel_enabled: bool,
    pub metrics_port: u16,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AppConfig {
    pub session_token_ttl_secs: u64,
    pub password_iterations: u32,
    pub password_memory_kb: u32,
    pub password_parallelism: u32,
    pub max_players_per_room: u32,
    pub shutdown_timeout_secs: u64,
}

impl AppConfig {
    /// argon2id 参数:(memory_kb, iterations, parallelism)。
    pub fn password_params(&self) -> (u32, u32, u32) {
        (
            self.password_memory_kb,
            self.password_iterations,
            self.password_parallelism,
        )
    }

    pub fn session_token_ttl(&self) -> Duration {
        Duration::from_secs(self.session_token_ttl_secs)
    }
}

/// 配置错误:数值/布尔解析失败或非法取值。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("环境变量 {name} 解析失败: {reason}")]
    Parse { name: &'static str, reason: String },
    #[error("环境变量 {name} 取值非法: {reason}")]
    Invalid { name: &'static str, reason: String },
}

pub type ConfigResult<T> = Result<T, ConfigError>;

/// 数据库 URL 的示例占位(生产必须覆盖,见 PRD 18.3 🔴)。
pub const DATABASE_URL_PLACEHOLDER: &str = "postgres://user:pass@localhost:5432/game";

/// 环境变量读取器:返回 None 表示未设置(用默认值)。
pub type Lookup<'a> = dyn FnMut(&'static str) -> Option<String> + 'a;

impl Config {
    /// 从进程环境读取(edition 2024 下读取 env::var 为安全操作,写入方由部署负责)。
    pub fn from_env() -> ConfigResult<Self> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// 从任意键值源构建(测试与远程配置中心适配点)。
    pub fn from_lookup<F>(mut lookup: F) -> ConfigResult<Self>
    where
        F: FnMut(&'static str) -> Option<String>,
    {
        Ok(Self {
            network: network_from_lookup(&mut lookup)?,
            rate_limit: rate_limit_from_lookup(&mut lookup)?,
            tls: tls_from_lookup(&mut lookup)?,
            database: database_from_lookup(&mut lookup)?,
            redis: redis_from_lookup(&mut lookup)?,
            log: log_from_lookup(&mut lookup)?,
            app: app_from_lookup(&mut lookup)?,
        })
    }

    /// 数据库 URL 是否仍为示例占位:生产启动前应告警或拒绝。
    pub fn is_database_url_placeholder(&self) -> bool {
        self.database.url == DATABASE_URL_PLACEHOLDER
    }

    /// 启动前一致性校验。
    pub fn validate(&self) -> ConfigResult<()> {
        if self.network.heartbeat_timeout_secs < 2 * self.network.heartbeat_interval_secs {
            return Err(ConfigError::Invalid {
                name: "SERVER_HEARTBEAT_TIMEOUT_SECS",
                reason: "应至少为心跳间隔的 2 倍".into(),
            });
        }
        if self.network.channel_per_conn == 0 {
            return Err(ConfigError::Invalid {
                name: "SERVER_CHANNEL_PER_CONN_SIZE",
                reason: "发送队列必须有界且大于 0(PRD 8.5)".into(),
            });
        }
        Ok(())
    }
}

fn network_from_lookup(lookup: &mut Lookup<'_>) -> ConfigResult<NetworkConfig> {
    Ok(NetworkConfig {
        tcp_bind_addr: env_str(lookup, "SERVER_TCP_BIND_ADDR", "0.0.0.0:8080"),
        http_bind_addr: env_str(lookup, "SERVER_HTTP_BIND_ADDR", "0.0.0.0:8081"),
        wss_bind_addr: env_str(lookup, "SERVER_WSS_BIND_ADDR", "0.0.0.0:8082"),
        max_connections: env_usize(lookup, "SERVER_MAX_CONNECTIONS", 10_000)?,
        backlog: env_u32(lookup, "SERVER_CONNECTION_BACKLOG", 1024)?,
        read_buffer_size: env_usize(lookup, "SERVER_TCP_READ_BUFFER_SIZE", 16_384)?,
        write_buffer_size: env_usize(lookup, "SERVER_TCP_WRITE_BUFFER_SIZE", 16_384)?,
        channel_per_conn: env_usize(lookup, "SERVER_CHANNEL_PER_CONN_SIZE", 256)?,
        channel_per_room: env_usize(lookup, "SERVER_CHANNEL_PER_ROOM_SIZE", 512)?,
        max_frame_size: env_usize(lookup, "SERVER_MAX_FRAME_SIZE", 65_536)?,
        heartbeat_interval_secs: env_u64(lookup, "SERVER_HEARTBEAT_INTERVAL_SECS", 20)?,
        heartbeat_timeout_secs: env_u64(lookup, "SERVER_HEARTBEAT_TIMEOUT_SECS", 60)?,
        unauth_timeout_secs: env_u64(lookup, "SERVER_UNAUTH_TIMEOUT_SECS", 10)?,
        unauth_max_per_ip: env_usize(lookup, "SERVER_UNAUTH_MAX_PER_IP", 5)?,
        tcp_nodelay: env_bool(lookup, "SERVER_TCP_NODELAY", true)?,
        tcp_keepalive_secs: env_u64(lookup, "SERVER_TCP_KEEPALIVE_SECS", 30)?,
    })
}

fn rate_limit_from_lookup(lookup: &mut Lookup<'_>) -> ConfigResult<RateLimitConfig> {
    Ok(RateLimitConfig {
        per_conn: env_u64(lookup, "SERVER_RATE_LIMIT_PER_CONN", 100)?,
        burst: env_u64(lookup, "SERVER_RATE_LIMIT_BURST", 10)?,
        enabled: env_bool(lookup, "SERVER_RATE_LIMIT_ENABLED", true)?,
    })
}

fn tls_from_lookup(lookup: &mut Lookup<'_>) -> ConfigResult<TlsConfig> {
    let min_version = env_str(lookup, "TLS_MIN_VERSION", "TLSv1.3");
    if min_version != "TLSv1.3" {
        return Err(ConfigError::Invalid {
            name: "TLS_MIN_VERSION",
            reason: "仅允许 TLSv1.3,禁止降级".into(),
        });
    }
    Ok(TlsConfig {
        cert_path: env_str(lookup, "TLS_CERT_PATH", "./certs/server.crt"),
        key_path: env_str(lookup, "TLS_KEY_PATH", "./certs/server.key"),
        ca_path: env_opt_str(lookup, "TLS_CA_PATH"),
        min_version,
    })
}

fn database_from_lookup(lookup: &mut Lookup<'_>) -> ConfigResult<DatabaseConfig> {
    Ok(DatabaseConfig {
        url: env_str(lookup, "DATABASE_URL", DATABASE_URL_PLACEHOLDER),
        max_connections: env_u32(lookup, "DATABASE_MAX_CONNECTIONS", 30)?,
        min_connections: env_u32(lookup, "DATABASE_MIN_CONNECTIONS", 5)?,
        connect_timeout_secs: env_u64(lookup, "DATABASE_CONNECT_TIMEOUT_SECS", 5)?,
        idle_timeout_secs: env_u64(lookup, "DATABASE_IDLE_TIMEOUT_SECS", 300)?,
        sqlx_log_level: env_str(lookup, "DATABASE_SQLX_LOG_LEVEL", "warn"),
    })
}

fn redis_from_lookup(lookup: &mut Lookup<'_>) -> ConfigResult<RedisConfig> {
    Ok(RedisConfig {
        url: env_str(lookup, "REDIS_URL", "redis://127.0.0.1:6379"),
        pool_max_size: env_usize(lookup, "REDIS_POOL_MAX_SIZE", 10)?,
        default_ttl_secs: env_u64(lookup, "REDIS_DEFAULT_TTL_SECS", 604_800)?,
        connect_timeout_secs: env_u64(lookup, "REDIS_CONNECT_TIMEOUT_SECS", 2)?,
    })
}

fn log_from_lookup(lookup: &mut Lookup<'_>) -> ConfigResult<LogConfig> {
    Ok(LogConfig {
        level: env_str(lookup, "LOG_LEVEL", "info"),
        format: env_str(lookup, "LOG_FORMAT", "json"),
        otel_enabled: env_bool(lookup, "OTEL_ENABLED", false)?,
        metrics_port: env_u16(lookup, "METRICS_PORT", 9090)?,
    })
}

fn app_from_lookup(lookup: &mut Lookup<'_>) -> ConfigResult<AppConfig> {
    Ok(AppConfig {
        session_token_ttl_secs: env_u64(lookup, "APP_SESSION_TOKEN_TTL_SECS", 604_800)?,
        password_iterations: env_u32(lookup, "APP_PASSWORD_ITERATIONS", 3)?,
        password_memory_kb: env_u32(lookup, "APP_PASSWORD_MEMORY_KB", 19_456)?,
        password_parallelism: env_u32(lookup, "APP_PASSWORD_PARALLELISM", 4)?,
        max_players_per_room: env_u32(lookup, "APP_MAX_PLAYERS_PER_ROOM", 10)?,
        shutdown_timeout_secs: env_u64(lookup, "APP_SHUTDOWN_TIMEOUT_SECS", 30)?,
    })
}

fn env_str(lookup: &mut Lookup<'_>, name: &'static str, default: &str) -> String {
    lookup(name).unwrap_or_else(|| default.to_string())
}

fn env_opt_str(lookup: &mut Lookup<'_>, name: &'static str) -> Option<String> {
    lookup(name).filter(|value| !value.trim().is_empty())
}

fn env_parse<T: std::str::FromStr>(
    lookup: &mut Lookup<'_>,
    name: &'static str,
    default: T,
    type_name: &'static str,
) -> ConfigResult<T> {
    match lookup(name) {
        None => Ok(default),
        Some(raw) => raw.trim().parse::<T>().map_err(|_| ConfigError::Parse {
            name,
            reason: format!("无法按 {type_name} 解析: {raw:?}"),
        }),
    }
}

fn env_bool(lookup: &mut Lookup<'_>, name: &'static str, default: bool) -> ConfigResult<bool> {
    match lookup(name) {
        None => Ok(default),
        Some(raw) => match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Ok(true),
            "0" | "false" | "no" | "off" => Ok(false),
            other => Err(ConfigError::Parse {
                name,
                reason: format!("无法按 bool 解析: {other:?}"),
            }),
        },
    }
}

fn env_u16(lookup: &mut Lookup<'_>, name: &'static str, default: u16) -> ConfigResult<u16> {
    env_parse(lookup, name, default, "u16")
}

fn env_u32(lookup: &mut Lookup<'_>, name: &'static str, default: u32) -> ConfigResult<u32> {
    env_parse(lookup, name, default, "u32")
}

fn env_u64(lookup: &mut Lookup<'_>, name: &'static str, default: u64) -> ConfigResult<u64> {
    env_parse(lookup, name, default, "u64")
}

fn env_usize(lookup: &mut Lookup<'_>, name: &'static str, default: usize) -> ConfigResult<usize> {
    env_parse(lookup, name, default, "usize")
}

impl fmt::Display for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // 🔴 不打印任何敏感值:仅输出关键非敏感项。
        write!(
            f,
            "Config {{ tcp: {}, http: {}, max_frame: {}, max_conns: {}, db_max_conns: {}, metrics_port: {} }}",
            self.network.tcp_bind_addr,
            self.network.http_bind_addr,
            self.network.max_frame_size,
            self.network.max_connections,
            self.database.max_connections,
            self.log.metrics_port
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    type Map = HashMap<&'static str, String>;

    fn lookup_from(map: Map) -> impl FnMut(&'static str) -> Option<String> {
        move |name| map.get(name).cloned()
    }

    fn config_with(entries: &[(&'static str, &str)]) -> ConfigResult<Config> {
        let map: Map = entries
            .iter()
            .map(|(name, value)| (*name, value.to_string()))
            .collect();
        Config::from_lookup(lookup_from(map))
    }

    #[test]
    fn defaults_match_prd_table() {
        let config = Config::from_lookup(|_| None).unwrap();
        assert_eq!(config.network.tcp_bind_addr, "0.0.0.0:8080");
        assert_eq!(config.network.http_bind_addr, "0.0.0.0:8081");
        assert_eq!(config.network.wss_bind_addr, "0.0.0.0:8082");
        assert_eq!(config.network.max_connections, 10_000);
        assert_eq!(config.network.backlog, 1024);
        assert_eq!(config.network.read_buffer_size, 16_384);
        assert_eq!(config.network.write_buffer_size, 16_384);
        assert_eq!(config.network.channel_per_conn, 256);
        assert_eq!(config.network.channel_per_room, 512);
        assert_eq!(config.network.max_frame_size, 65_536);
        assert_eq!(config.network.heartbeat_interval_secs, 20);
        assert_eq!(config.network.heartbeat_timeout_secs, 60);
        assert_eq!(config.network.unauth_timeout_secs, 10);
        assert_eq!(config.network.unauth_max_per_ip, 5);
        assert!(config.network.tcp_nodelay);
        assert_eq!(config.network.tcp_keepalive_secs, 30);
        assert!(config.rate_limit.enabled);
        assert_eq!(config.rate_limit.per_conn, 100);
        assert_eq!(config.rate_limit.burst, 10);
        assert_eq!(config.tls.cert_path, "./certs/server.crt");
        assert_eq!(config.tls.key_path, "./certs/server.key");
        assert_eq!(config.tls.ca_path, None);
        assert_eq!(config.tls.min_version, "TLSv1.3");
        assert_eq!(config.database.max_connections, 30);
        assert_eq!(config.database.min_connections, 5);
        assert_eq!(config.database.connect_timeout_secs, 5);
        assert_eq!(config.database.idle_timeout_secs, 300);
        assert_eq!(config.database.sqlx_log_level, "warn");
        assert_eq!(config.redis.url, "redis://127.0.0.1:6379");
        assert_eq!(config.redis.pool_max_size, 10);
        assert_eq!(config.redis.default_ttl_secs, 604_800);
        assert_eq!(config.redis.connect_timeout_secs, 2);
        assert_eq!(config.log.level, "info");
        assert_eq!(config.log.format, "json");
        assert!(!config.log.otel_enabled);
        assert_eq!(config.log.metrics_port, 9090);
        assert_eq!(config.app.session_token_ttl_secs, 604_800);
        assert_eq!(config.app.password_params(), (19_456, 3, 4));
        assert_eq!(config.app.session_token_ttl(), Duration::from_secs(604_800));
        assert_eq!(config.app.max_players_per_room, 10);
        assert_eq!(config.app.shutdown_timeout_secs, 30);
        assert!(config.is_database_url_placeholder());
        assert!(config.validate().is_ok());
    }

    #[test]
    fn overrides_apply_and_blank_optional_becomes_none() {
        let config = config_with(&[
            ("SERVER_TCP_BIND_ADDR", "127.0.0.1:9000"),
            ("SERVER_MAX_FRAME_SIZE", "4096"),
            ("SERVER_MAX_CONNECTIONS", "500"),
            ("SERVER_TCP_NODELAY", "false"),
            ("SERVER_RATE_LIMIT_ENABLED", "off"),
            ("APP_MAX_PLAYERS_PER_ROOM", "4"),
            ("TLS_CA_PATH", "   "),
            ("DATABASE_URL", "postgres://real:pw@db:5432/game"),
        ])
        .unwrap();
        assert_eq!(config.network.tcp_bind_addr, "127.0.0.1:9000");
        assert_eq!(config.network.max_frame_size, 4096);
        assert_eq!(config.network.max_connections, 500);
        assert!(!config.network.tcp_nodelay);
        assert!(!config.rate_limit.enabled);
        assert_eq!(config.app.max_players_per_room, 4);
        assert_eq!(config.tls.ca_path, None, "空白值视为未设置");
        assert!(!config.is_database_url_placeholder());
    }

    #[test]
    fn parse_failures_are_reported_without_panic() {
        let err = config_with(&[("SERVER_MAX_CONNECTIONS", "many")]).unwrap_err();
        assert!(err.to_string().contains("SERVER_MAX_CONNECTIONS"));
        assert!(matches!(
            err,
            ConfigError::Parse {
                name: "SERVER_MAX_CONNECTIONS",
                ..
            }
        ));

        let err = config_with(&[("METRICS_PORT", "70000")]).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Parse {
                name: "METRICS_PORT",
                ..
            }
        ));

        let err = config_with(&[("SERVER_TCP_NODELAY", "maybe")]).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Parse {
                name: "SERVER_TCP_NODELAY",
                ..
            }
        ));

        let err = config_with(&[("APP_PASSWORD_MEMORY_KB", "-1")]).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Parse {
                name: "APP_PASSWORD_MEMORY_KB",
                ..
            }
        ));
    }

    #[test]
    fn tls_downgrade_is_rejected() {
        let err = config_with(&[("TLS_MIN_VERSION", "TLSv1.2")]).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Invalid {
                name: "TLS_MIN_VERSION",
                ..
            }
        ));
    }

    #[test]
    fn validate_rejects_bad_heartbeat_and_unbounded_queue() {
        let config = config_with(&[
            ("SERVER_HEARTBEAT_TIMEOUT_SECS", "25"),
            ("SERVER_HEARTBEAT_INTERVAL_SECS", "20"),
        ])
        .unwrap();
        assert!(matches!(
            config.validate(),
            Err(ConfigError::Invalid {
                name: "SERVER_HEARTBEAT_TIMEOUT_SECS",
                ..
            })
        ));

        let config = config_with(&[
            ("SERVER_HEARTBEAT_TIMEOUT_SECS", "60"),
            ("SERVER_CHANNEL_PER_CONN_SIZE", "0"),
        ])
        .unwrap();
        assert!(matches!(
            config.validate(),
            Err(ConfigError::Invalid {
                name: "SERVER_CHANNEL_PER_CONN_SIZE",
                ..
            })
        ));
    }

    #[test]
    fn display_hides_sensitive_values() {
        let config = Config::from_lookup(|_| None).unwrap();
        let text = config.to_string();
        assert!(!text.contains("postgres://"));
        assert!(text.contains("tcp: 0.0.0.0:8080"));
    }

    #[test]
    fn boolean_values_are_flexible() {
        for value in ["1", "true", "YES", "on"] {
            let config = config_with(&[("SERVER_TCP_NODELAY", value)]).unwrap();
            assert!(config.network.tcp_nodelay, "{value} 应解析为 true");
        }
        for value in ["0", "false", "No", "off"] {
            let config = config_with(&[("SERVER_TCP_NODELAY", value)]).unwrap();
            assert!(!config.network.tcp_nodelay, "{value} 应解析为 false");
        }
    }
}
