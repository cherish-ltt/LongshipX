//! HTTP 错误响应:AppError → 状态码 + JSON 体(不泄漏内部细节)。

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use ppt_tcp_application::error::AppError;
use serde::Serialize;

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    message: String,
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    error: &'a str,
    message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = ErrorBody {
            error: self.status.canonical_reason().unwrap_or("error"),
            message: self.message,
        };
        (self.status, Json(body)).into_response()
    }
}

impl From<AppError> for ApiError {
    fn from(err: AppError) -> Self {
        let (status, message) = match err {
            AppError::Validation(msg) => (StatusCode::BAD_REQUEST, msg),
            AppError::Unauthorized(msg) => (StatusCode::UNAUTHORIZED, msg),
            AppError::Forbidden(msg) => (StatusCode::FORBIDDEN, msg),
            AppError::NotFound(msg) => (StatusCode::NOT_FOUND, msg),
            AppError::Conflict(msg) => (StatusCode::CONFLICT, msg),
            AppError::Busy => (
                StatusCode::SERVICE_UNAVAILABLE,
                "服务繁忙,请稍后重试".into(),
            ),
            AppError::RateLimited => (StatusCode::TOO_MANY_REQUESTS, "请求过于频繁".into()),
            AppError::Storage(_) | AppError::Internal(_) => {
                // 🔴 内部细节只进日志,不回给客户端。
                tracing::error!(error = %err, "HTTP 处理内部错误");
                (StatusCode::INTERNAL_SERVER_ERROR, "服务器内部错误".into())
            },
        };
        Self { status, message }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn maps_app_errors_to_http_statuses() {
        let cases = [
            (AppError::Validation("bad".into()), StatusCode::BAD_REQUEST),
            (
                AppError::Unauthorized("no".into()),
                StatusCode::UNAUTHORIZED,
            ),
            (AppError::Forbidden("no".into()), StatusCode::FORBIDDEN),
            (AppError::NotFound("x".into()), StatusCode::NOT_FOUND),
            (AppError::Conflict("dup".into()), StatusCode::CONFLICT),
            (AppError::Busy, StatusCode::SERVICE_UNAVAILABLE),
            (AppError::RateLimited, StatusCode::TOO_MANY_REQUESTS),
            (
                AppError::Internal("boom".into()),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
            (
                AppError::Storage("boom".into()),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
        ];
        for (app_err, expected) in cases {
            let response = ApiError::from(app_err).into_response();
            assert_eq!(response.status(), expected);
        }
    }
}
