//! HTTP 入口测试(PRD 8.3/14):注册/登录/档案/健康检查与 Bearer 鉴权中间件。

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt as _;
use longshipx_application::auth::LoginDependencies;
use longshipx_application::error::AppError;
use longshipx_application::ports::{AuditLogger, PasswordHasher, SessionTokenStore};
use longshipx_application::{GetPlayerProfile, LoginUseCase, RegisterUseCase};
use longshipx_gateway::http::routes::router;
use longshipx_gateway::http::state::HttpState;
use longshipx_infrastructure::cache::InMemoryTokenStore;
use longshipx_infrastructure::persistence::repositories::{
    InMemoryAccountRepository, InMemoryPlayerRepository,
};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt as _;

/// 可逆假哈希器:注册→登录全链路无需真实 argon2 开销。
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
        _player_id: Option<longshipx_domain::PlayerId>,
        _action: &str,
        _detail: String,
    ) -> Result<(), AppError> {
        Ok(())
    }
}

fn app() -> axum::Router {
    let accounts = Arc::new(InMemoryAccountRepository::new());
    let players = Arc::new(InMemoryPlayerRepository::new());
    let hasher = Arc::new(PlainHasher);
    let audit = Arc::new(NullAudit);
    let tokens: Arc<dyn SessionTokenStore> = Arc::new(InMemoryTokenStore::new());
    let state = Arc::new(HttpState {
        register: Arc::new(RegisterUseCase::new(
            accounts.clone(),
            players.clone(),
            hasher.clone(),
            audit.clone(),
        )),
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
        profile: Arc::new(GetPlayerProfile::new(players.clone())),
        tokens,
    });
    router(state)
}

async fn body_json(response: axum::response::Response) -> serde_json::Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn healthz_is_ok() {
    let response = app()
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn register_login_and_me_roundtrip() {
    let app = app();

    // 注册。
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/register")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"username": "erin", "password": "super-secret", "nickname": "艾琳"})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let registered = body_json(response).await;
    assert_eq!(registered["nickname"], "艾琳");
    let player_id = registered["player_id"].as_str().unwrap().to_string();

    // 登录拿 token。
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/login")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"username": "Erin", "password": "super-secret"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let logged_in = body_json(response).await;
    let token = logged_in["token"].as_str().unwrap().to_string();
    assert_eq!(logged_in["expires_in_secs"], 600);

    // Bearer token 访问 /me。
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/me")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let profile = body_json(response).await;
    assert_eq!(profile["player_id"].as_str().unwrap(), player_id);
    assert_eq!(profile["nickname"], "艾琳");
}

#[tokio::test]
async fn duplicate_username_conflicts() {
    let app = app();
    let payload = json!({"username": "frank", "password": "super-secret", "nickname": "弗兰克"});
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/register")
                .header("content-type", "application/json")
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/register")
                .header("content-type", "application/json")
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn weak_password_is_bad_request() {
    let response = app()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/register")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"username": "gina", "password": "short", "nickname": "g"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn login_rejects_wrong_password_without_leaking_user_existence() {
    let app = app();
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/register")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"username": "hank", "password": "super-secret", "nickname": "汉克"})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    // 错误密码与不存在的用户返回一致的 401。
    for body in [
        json!({"username": "hank", "password": "wrong-password"}),
        json!({"username": "ghost", "password": "super-secret"}),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/login")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let payload = body_json(response).await;
        assert_eq!(payload["message"], "用户名或密码错误");
    }
}

#[tokio::test]
async fn me_requires_bearer_token() {
    let app = app();
    // 无 Authorization 头。
    let response = app
        .clone()
        .oneshot(Request::builder().uri("/me").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    // 非法 token。
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/me")
                .header("authorization", "Bearer not-a-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    // 非 Bearer scheme。
    let response = app
        .oneshot(
            Request::builder()
                .uri("/me")
                .header("authorization", "Basic abc")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
