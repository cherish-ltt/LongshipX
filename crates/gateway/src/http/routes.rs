//! HTTP 路由:POST /register、POST /login、GET /me、GET /healthz。

use crate::http::auth_extractor::AuthUser;
use crate::http::error::ApiError;
use crate::http::state::HttpState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use longshipx_application::dto::{LoginCommand, RegisterCommand};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

pub fn router(state: Arc<HttpState>) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/register", post(register))
        .route("/login", post(login))
        .route("/me", get(me))
        .with_state(state)
}

async fn healthz() -> &'static str {
    "ok"
}

#[derive(Debug, Deserialize)]
pub struct RegisterRequest {
    pub username: String,
    pub password: String,
    pub nickname: String,
}

#[derive(Debug, Serialize)]
pub struct RegisterResponse {
    pub account_id: Uuid,
    pub player_id: Uuid,
    pub nickname: String,
}

async fn register(
    State(state): State<Arc<HttpState>>,
    Json(request): Json<RegisterRequest>,
) -> Result<(StatusCode, Json<RegisterResponse>), ApiError> {
    let result = state
        .register
        .execute(RegisterCommand {
            username: request.username,
            password: request.password,
            nickname: request.nickname,
        })
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(RegisterResponse {
            account_id: result.account_id.0,
            player_id: result.player_id.0,
            nickname: result.nickname,
        }),
    ))
}

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Serialize)]
pub struct LoginResponse {
    pub token: String,
    pub player_id: Uuid,
    pub nickname: String,
    pub expires_in_secs: u64,
}

async fn login(
    State(state): State<Arc<HttpState>>,
    Json(request): Json<LoginRequest>,
) -> Result<Json<LoginResponse>, ApiError> {
    let result = state
        .login
        .execute(LoginCommand {
            username: request.username,
            password: request.password,
        })
        .await?;
    Ok(Json(LoginResponse {
        token: result.token,
        player_id: result.player_id.0,
        nickname: result.nickname,
        expires_in_secs: result.expires_in_secs,
    }))
}

#[derive(Debug, Serialize)]
pub struct PlayerResponse {
    pub player_id: Uuid,
    pub nickname: String,
    pub level: u32,
    pub exp: u64,
    pub last_login_at: Option<DateTime<Utc>>,
}

async fn me(
    State(state): State<Arc<HttpState>>,
    AuthUser { player_id }: AuthUser,
) -> Result<Json<PlayerResponse>, ApiError> {
    let profile = state.profile.execute(player_id).await?;
    Ok(Json(PlayerResponse {
        player_id: profile.player_id.0,
        nickname: profile.nickname,
        level: profile.level,
        exp: profile.exp,
        last_login_at: profile.last_login_at,
    }))
}
