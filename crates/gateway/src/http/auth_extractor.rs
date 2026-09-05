//! Bearer token 鉴权提取器:HTTP 中间件形态的鉴权(PRD 8.3/12)。

use crate::http::error::ApiError;
use crate::http::state::HttpState;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use ppt_tcp_domain::PlayerId;
use std::sync::Arc;

/// 从 `Authorization: Bearer <token>` 解析出的已鉴权玩家。
#[derive(Debug, Clone)]
pub struct AuthUser {
    pub player_id: PlayerId,
}

impl FromRequestParts<Arc<HttpState>> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<HttpState>,
    ) -> Result<Self, Self::Rejection> {
        let header = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| {
                ApiError::new(
                    axum::http::StatusCode::UNAUTHORIZED,
                    "缺少 Authorization 头",
                )
            })?;
        let token = header.strip_prefix("Bearer ").ok_or_else(|| {
            ApiError::new(
                axum::http::StatusCode::UNAUTHORIZED,
                "Authorization 必须为 Bearer token",
            )
        })?;
        match state.tokens.resolve(token.trim()).await {
            Ok(Some(player_id)) => Ok(Self { player_id }),
            Ok(None) => Err(ApiError::new(
                axum::http::StatusCode::UNAUTHORIZED,
                "token 无效或已过期",
            )),
            Err(err) => Err(ApiError::from(err)),
        }
    }
}
