//! 启动装配(PRD 4.2 composition root):加载配置 → 依赖注入 → 启动监听 → 优雅停机。

use crate::{observability, shutdown};
use ppt_tcp_application::auth::LoginDependencies;
use ppt_tcp_application::ports::SessionTokenStore;
use ppt_tcp_application::{
    GetPlayerProfile, LoginUseCase, RegisterUseCase, RoomService, RoomServiceConfig,
};
use ppt_tcp_gateway::http::HttpState;
use ppt_tcp_gateway::tcp::config::{RateLimitSettings, TcpGatewayConfig};
use ppt_tcp_gateway::tcp::router_setup::build_router;
use ppt_tcp_gateway::tcp::{AuthGate, ConnectionRegistry, GatewayDeps, TcpGateway};
use ppt_tcp_infrastructure::cache::{InMemoryTokenStore, RedisTokenStore};
use ppt_tcp_infrastructure::config::{Config, DatabaseConfig, TlsConfig};
use ppt_tcp_infrastructure::password::Argon2PasswordHasher;
use ppt_tcp_infrastructure::persistence::repositories::{
    SeaAccountRepository, SeaAuditLogger, SeaPlayerRepository,
};
use ppt_tcp_protocol::GameCodec;
use std::error::Error;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;

struct Services {
    register: Arc<RegisterUseCase>,
    login: Arc<LoginUseCase>,
    profile: Arc<GetPlayerProfile>,
    rooms: Arc<RoomService>,
    tokens: Arc<dyn SessionTokenStore>,
    players: Arc<dyn ppt_tcp_domain::PlayerRepository>,
}

pub async fn run() -> Result<(), Box<dyn Error>> {
    dotenvy::dotenv().ok();
    let config = load_config()?;
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let recorder = metrics_exporter_prometheus::PrometheusBuilder::new()
        .install_recorder()
        .map_err(|err| format!("指标记录器初始化失败: {err}"))?;
    let metrics_addr: SocketAddr = format!("0.0.0.0:{}", config.log.metrics_port).parse()?;
    tokio::spawn(observability::serve_metrics(metrics_addr, recorder));

    let db = connect_database(&config.database).await?;
    ppt_tcp_migration::run_migrations(&db).await?;
    tracing::info!("数据库迁移已就绪");

    let tokens = connect_token_store(&config).await?;
    let services = build_services(&config, db.clone(), tokens)?;
    let tcp = start_tcp_gateway(&config, &services, shutdown_rx.clone()).await?;
    start_http_server(&config, &services, shutdown_rx.clone()).await?;

    shutdown::wait_for_signal(shutdown_tx).await;
    graceful_teardown(&config, &services, &tcp, db).await;
    Ok(())
}

fn load_config() -> Result<Config, Box<dyn Error>> {
    let config = Config::from_env()?;
    config.validate()?;
    if config.is_database_url_placeholder() {
        tracing::warn!("DATABASE_URL 仍为示例占位,生产环境必须通过环境变量覆盖(PRD 18.3)");
    }
    observability::init_tracing(&config.log.level, &config.log.format);
    tracing::info!(summary = %config, "配置加载完成");
    Ok(config)
}

async fn connect_database(
    cfg: &DatabaseConfig,
) -> Result<sea_orm::DatabaseConnection, Box<dyn Error>> {
    let mut opts = sea_orm::ConnectOptions::new(cfg.url.clone());
    // 🔴 连接池必须设上限,防止流量突增打满数据库(PRD 7.4/16 R13)。
    opts.max_connections(cfg.max_connections)
        .min_connections(cfg.min_connections)
        .connect_timeout(Duration::from_secs(cfg.connect_timeout_secs))
        .idle_timeout(Duration::from_secs(cfg.idle_timeout_secs))
        .sqlx_logging(matches!(cfg.sqlx_log_level.as_str(), "debug" | "trace"));
    let db = sea_orm::Database::connect(opts).await?;
    tracing::info!(
        max_connections = cfg.max_connections,
        "PostgreSQL 连接池已建立"
    );
    Ok(db)
}

async fn connect_token_store(
    config: &Config,
) -> Result<Arc<dyn SessionTokenStore>, Box<dyn Error>> {
    let store = RedisTokenStore::connect(
        &config.redis.url,
        Duration::from_secs(config.redis.connect_timeout_secs),
    )
    .await?;
    tracing::info!("Redis token 存储已连接");
    Ok(Arc::new(store))
}

fn build_services(
    config: &Config,
    db: sea_orm::DatabaseConnection,
    tokens: Arc<dyn SessionTokenStore>,
) -> Result<Services, Box<dyn Error>> {
    let db = Arc::new(db);
    let accounts = Arc::new(SeaAccountRepository::new(db.clone()));
    let players = Arc::new(SeaPlayerRepository::new(db.clone()));
    let audit = Arc::new(SeaAuditLogger::new(db));
    let (memory_kb, iterations, parallelism) = config.app.password_params();
    let hasher = Arc::new(Argon2PasswordHasher::new(
        memory_kb,
        iterations,
        parallelism,
    )?);
    let publisher = Arc::new(ppt_tcp_infrastructure::events::InMemoryEventPublisher::new(
        256,
    ));
    let rooms = Arc::new(RoomService::new(
        RoomServiceConfig {
            command_capacity: config.network.channel_per_room,
            max_players: config.app.max_players_per_room,
        },
        publisher,
    ));
    let register = Arc::new(RegisterUseCase::new(
        accounts.clone(),
        players.clone(),
        hasher.clone(),
        audit.clone(),
    ));
    let login = Arc::new(LoginUseCase::new(
        LoginDependencies {
            accounts,
            players: players.clone(),
            tokens: tokens.clone(),
            hasher,
            audit,
        },
        config.app.session_token_ttl(),
    ));
    let profile = Arc::new(GetPlayerProfile::new(players.clone()));
    Ok(Services {
        register,
        login,
        profile,
        rooms,
        tokens,
        players,
    })
}

async fn start_tcp_gateway(
    config: &Config,
    services: &Services,
    shutdown_rx: watch::Receiver<bool>,
) -> Result<Arc<TcpGateway>, Box<dyn Error>> {
    let acceptor = build_tls_acceptor(&config.tls)?;
    let bind_addr: SocketAddr = config
        .network
        .tcp_bind_addr
        .parse()
        .map_err(|err| format!("SERVER_TCP_BIND_ADDR 非法: {err}"))?;
    let tcp_config = TcpGatewayConfig {
        bind_addr,
        max_frame_size: config.network.max_frame_size,
        send_queue_capacity: config.network.channel_per_conn,
        room_event_capacity: ppt_tcp_application::room::ROOM_EVENT_CAPACITY,
        unauth_timeout: Duration::from_secs(config.network.unauth_timeout_secs),
        heartbeat_timeout: Duration::from_secs(config.network.heartbeat_timeout_secs),
        rate: RateLimitSettings {
            enabled: config.rate_limit.enabled,
            per_second: config.rate_limit.per_conn,
            burst: config.rate_limit.burst,
        },
        max_connections: config.network.max_connections,
        backlog: config.network.backlog,
        nodelay: config.network.tcp_nodelay,
        keepalive: Duration::from_secs(config.network.tcp_keepalive_secs),
    };
    let deps = Arc::new(GatewayDeps {
        codec: Arc::new(GameCodec),
        router: build_router(),
        tokens: services.tokens.clone(),
        players: services.players.clone(),
        profile: services.profile.clone(),
        rooms: services.rooms.clone(),
        auth_gate: Arc::new(AuthGate::new(config.network.unauth_max_per_ip)),
        connections: Arc::new(ConnectionRegistry::new()),
    });
    let gateway = Arc::new(TcpGateway::bind(tcp_config, acceptor, deps)?);
    let addr = gateway.local_addr()?;
    tracing::info!(%addr, "TCP+TLS 网关开始监听");
    let gateway_for_run = gateway.clone();
    tokio::spawn(async move { gateway_for_run.run(shutdown_rx).await });
    Ok(gateway)
}

fn build_tls_acceptor(tls: &TlsConfig) -> Result<ppt_tcp_net_kit::TlsAcceptor, Box<dyn Error>> {
    let acceptor = ppt_tcp_net_kit::tls::server_acceptor_from_files(
        Path::new(&tls.cert_path),
        Path::new(&tls.key_path),
    )?;
    tracing::info!(cert = %tls.cert_path, "TLS 1.3 服务端证书加载完成");
    Ok(acceptor)
}

async fn start_http_server(
    config: &Config,
    services: &Services,
    shutdown_rx: watch::Receiver<bool>,
) -> Result<(), Box<dyn Error>> {
    let bind_addr: SocketAddr = config
        .network
        .http_bind_addr
        .parse()
        .map_err(|err| format!("SERVER_HTTP_BIND_ADDR 非法: {err}"))?;
    let state = Arc::new(HttpState {
        register: services.register.clone(),
        login: services.login.clone(),
        profile: services.profile.clone(),
        tokens: services.tokens.clone(),
    });
    let app = ppt_tcp_gateway::http::routes::router(state);
    let listener = tokio::net::TcpListener::bind(bind_addr).await?;
    let addr = listener.local_addr()?;
    tracing::info!(%addr, "HTTP 网关开始监听");
    let mut shutdown_for_axum = shutdown_rx.clone();
    tokio::spawn(async move {
        let serve = axum::serve(listener, app).with_graceful_shutdown(async move {
            let _ = shutdown_for_axum.wait_for(|done| *done).await;
        });
        if let Err(err) = serve.await {
            tracing::error!(error = %err, "HTTP 服务异常退出");
        }
    });
    Ok(())
}

/// 优雅停机序列(PRD 13.2):停 accept(网关自动广播维护通知)→ 关房间 → 排空 → 关连接池。
async fn graceful_teardown(
    config: &Config,
    services: &Services,
    tcp: &TcpGateway,
    db: sea_orm::DatabaseConnection,
) {
    if let Err(err) = services.rooms.close_all("服务器即将停机").await {
        tracing::warn!(error = %err, "关闭房间时出现错误");
    }

    let drain_timeout = Duration::from_secs(config.app.shutdown_timeout_secs);
    if tcp.wait_until_drained(drain_timeout).await {
        tracing::info!("所有连接已安全断开");
    } else {
        tracing::warn!(
            timeout_secs = drain_timeout.as_secs(),
            "排空等待超时,强制关闭剩余连接"
        );
    }

    if let Err(err) = db.close().await {
        tracing::warn!(error = %err, "关闭数据库连接池失败");
    }
    tracing::info!("优雅停机完成");
}

/// 开发环境辅助:内存 token 存储(仅测试/演示,重启即失效)。
#[allow(dead_code)]
fn in_memory_tokens() -> Arc<dyn SessionTokenStore> {
    Arc::new(InMemoryTokenStore::new())
}
