//! HTTP 入口基准:axum 路由 + Bearer 鉴权 + 用例链路(tower oneshot,无真实网络/TLS)。
//!
//! 框架链路统一用可逆假哈希器隔离 argon2 成本;另设一条生产哈希器登录基准,
//! 量化 argon2id 在完整登录链路中的占比。
//!
//! 运行:`cargo bench --bench http -p longshipx-gateway`

use std::hint::black_box;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::Request;
use criterion::{Criterion, criterion_group, criterion_main};
use longshipx_application::RegisterCommand;
use longshipx_application::auth::LoginDependencies;
use longshipx_application::error::AppError;
use longshipx_application::ports::{AuditLogger, PasswordHasher, SessionTokenStore};
use longshipx_application::{GetPlayerProfile, LoginUseCase, RegisterUseCase};
use longshipx_domain::PlayerId;
use longshipx_gateway::http::routes::router;
use longshipx_gateway::http::state::HttpState;
use longshipx_infrastructure::cache::InMemoryTokenStore;
use longshipx_infrastructure::password::Argon2PasswordHasher;
use longshipx_infrastructure::persistence::repositories::{
    InMemoryAccountRepository, InMemoryPlayerRepository,
};
use tokio::runtime::{Builder, Runtime};
use tower::ServiceExt as _;

/// 可逆假哈希器:注册→登录全链路无需真实 argon2 开销(与 http.rs 测试装置一致)。
struct PlainHasher;

impl PasswordHasher for PlainHasher {
    fn hash(&self, password: &str) -> Result<String, AppError> {
        Ok(format!("plain$${password}"))
    }

    fn verify(&self, password: &str, hash: &str) -> Result<bool, AppError> {
        Ok(hash == format!("plain$${password}"))
    }
}

struct NullAudit;

#[async_trait::async_trait]
impl AuditLogger for NullAudit {
    async fn record(
        &self,
        _player_id: Option<PlayerId>,
        _action: &str,
        _detail: String,
    ) -> Result<(), AppError> {
        Ok(())
    }
}

/// 基准装置:装配好的路由器、令牌存储与多线程运行时。
struct HttpBench {
    rt: Runtime,
    app: Router,
    bearer: String,
    login_body: String,
}

/// 内存基础设施装配 HTTP 应用;返回令牌存储与注册用例供预置数据。
fn build_app(
    hasher: Arc<dyn PasswordHasher>,
) -> (Router, Arc<InMemoryTokenStore>, Arc<RegisterUseCase>) {
    let accounts = Arc::new(InMemoryAccountRepository::new());
    let players = Arc::new(InMemoryPlayerRepository::new());
    let audit = Arc::new(NullAudit);
    let store = Arc::new(InMemoryTokenStore::new());
    let tokens: Arc<dyn SessionTokenStore> = store.clone();
    let register = Arc::new(RegisterUseCase::new(
        accounts.clone(),
        players.clone(),
        hasher.clone(),
        audit.clone(),
    ));
    let state = Arc::new(HttpState {
        register: register.clone(),
        login: Arc::new(LoginUseCase::new(
            LoginDependencies {
                accounts,
                players: players.clone(),
                tokens: tokens.clone(),
                hasher,
                audit,
            },
            Duration::from_secs(600),
        )),
        profile: Arc::new(GetPlayerProfile::new(players)),
        tokens,
    });
    (router(state), store, register)
}

fn setup(hasher: Arc<dyn PasswordHasher>) -> HttpBench {
    let (app, store, register) = build_app(hasher);
    let rt = Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("构建运行时");
    // 预置账号并签发 Bearer token(/me 基准直接持有令牌,避免依赖登录基准)。
    let registered = rt
        .block_on(register.execute(RegisterCommand {
            username: "bencher".to_string(),
            password: "bench-secret".to_string(),
            nickname: "基准员".to_string(),
        }))
        .expect("预置注册");
    let token = rt
        .block_on(store.create(registered.player_id, Duration::from_secs(3600)))
        .expect("签发令牌");
    HttpBench {
        rt,
        app,
        bearer: format!("Bearer {token}"),
        login_body: r#"{"username":"bencher","password":"bench-secret"}"#.to_string(),
    }
}

fn bench_http(c: &mut Criterion) {
    let mut group = c.benchmark_group("gateway/http");
    let env = setup(Arc::new(PlainHasher));

    group.bench_function("healthz", |b| {
        b.iter(|| {
            env.rt.block_on(async {
                let request = Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .unwrap();
                black_box(env.app.clone().oneshot(request).await.unwrap())
            })
        });
    });

    group.bench_function("me_bearer", |b| {
        b.iter(|| {
            env.rt.block_on(async {
                let request = Request::builder()
                    .uri("/me")
                    .header("authorization", env.bearer.as_str())
                    .body(Body::empty())
                    .unwrap();
                black_box(env.app.clone().oneshot(request).await.unwrap())
            })
        });
    });

    group.bench_function("login", |b| {
        b.iter(|| {
            env.rt.block_on(async {
                let request = Request::builder()
                    .method("POST")
                    .uri("/login")
                    .header("content-type", "application/json")
                    .body(Body::from(env.login_body.clone()))
                    .unwrap();
                black_box(env.app.clone().oneshot(request).await.unwrap())
            })
        });
    });

    // 生产哈希器:同一登录链路在 argon2id(8MiB/2 轮)下的真实成本。
    let argon_env = setup(Arc::new(Argon2PasswordHasher::new(8192, 2, 1).unwrap()));
    group.bench_function("login_argon2_8MiB", |b| {
        b.iter(|| {
            argon_env.rt.block_on(async {
                let request = Request::builder()
                    .method("POST")
                    .uri("/login")
                    .header("content-type", "application/json")
                    .body(Body::from(argon_env.login_body.clone()))
                    .unwrap();
                black_box(argon_env.app.clone().oneshot(request).await.unwrap())
            })
        });
    });

    group.finish();
}

criterion_group!(benches, bench_http);
criterion_main!(benches);
