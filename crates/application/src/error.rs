//! 应用层统一错误:由网关映射为协议错误或 HTTP 状态码。

use longshipx_domain::{DomainError, RepoError};

/// 用例执行错误。⚠️ 不携带密码/token 等敏感内容,可直接下发到客户端。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AppError {
    #[error("输入不合法: {0}")]
    Validation(String),
    #[error("未鉴权: {0}")]
    Unauthorized(String),
    #[error("禁止操作: {0}")]
    Forbidden(String),
    #[error("资源不存在: {0}")]
    NotFound(String),
    #[error("状态冲突: {0}")]
    Conflict(String),
    #[error("服务繁忙,请稍后重试")]
    Busy,
    #[error("请求过于频繁")]
    RateLimited,
    #[error("存储故障: {0}")]
    Storage(String),
    #[error("内部错误: {0}")]
    Internal(String),
}

impl From<DomainError> for AppError {
    fn from(err: DomainError) -> Self {
        match err {
            DomainError::InvalidValue { field, reason } => {
                Self::Validation(format!("{field}: {reason}"))
            },
            DomainError::RoomFull(_) => Self::Conflict("房间已满".into()),
            DomainError::RoomClosed(_) => Self::Conflict("房间已关闭".into()),
            DomainError::NotMember { .. } => Self::Validation("不在该房间中".into()),
            DomainError::IllegalTransition { .. } => Self::Internal("非法状态迁移".into()),
        }
    }
}

impl From<RepoError> for AppError {
    fn from(err: RepoError) -> Self {
        match err {
            RepoError::Conflict(reason) => Self::Conflict(reason),
            RepoError::Storage(reason) => Self::Storage(reason),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_domain_and_repo_errors() {
        assert!(matches!(
            AppError::from(DomainError::RoomFull(longshipx_domain::RoomId(
                uuid::Uuid::now_v7()
            ))),
            AppError::Conflict(_)
        ));
        assert!(matches!(
            AppError::from(RepoError::Storage("down".into())),
            AppError::Storage(_)
        ));
        assert_eq!(AppError::Busy.to_string(), "服务繁忙,请稍后重试");
    }
}
