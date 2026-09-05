//! 应用层端口(出站依赖的抽象):由 infrastructure 提供实现(依赖倒置,PRD 4.1/6)。

use crate::error::AppError;
use async_trait::async_trait;
use longshipx_domain::{DomainEvent, PlayerId};
use std::time::Duration;

/// 会话令牌存储:不透明随机 token + 服务端存储,支持立即吊销(PRD 6.1/10)。
#[async_trait]
pub trait SessionTokenStore: Send + Sync {
    /// 生成并保存一个新 token,`ttl` 后自动过期。
    async fn create(&self, player_id: PlayerId, ttl: Duration) -> Result<String, AppError>;

    /// 由 token 反查玩家;过期/不存在返回 None(不区分,避免信息泄露)。
    async fn resolve(&self, token: &str) -> Result<Option<PlayerId>, AppError>;

    /// 立即吊销该玩家的全部 token(单会话策略/踢下线,PRD 6.1)。
    async fn revoke_player(&self, player_id: PlayerId) -> Result<(), AppError>;
}

/// 密码哈希器:argon2id(PRD 第 10 章),实现于 infrastructure。
pub trait PasswordHasher: Send + Sync {
    fn hash(&self, password: &str) -> Result<String, AppError>;

    /// 恒定时间校验;哈希格式错误返回 Err,密码错误返回 Ok(false)。
    fn verify(&self, password: &str, hash: &str) -> Result<bool, AppError>;
}

/// 操作审计(PRD 7.3 audit_logs):线上排查与申诉依赖,允许失败不阻断主流程。
#[async_trait]
pub trait AuditLogger: Send + Sync {
    async fn record(
        &self,
        player_id: Option<PlayerId>,
        action: &str,
        detail: String,
    ) -> Result<(), AppError>;
}

/// 领域事件分发器(PRD 5.3):定义在应用层,实现可替换(进程内广播 → 消息队列)。
#[async_trait]
pub trait EventPublisher: Send + Sync {
    async fn publish(&self, event: DomainEvent) -> Result<(), AppError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 端口对象均为可安全传递的 Trait Object。
    #[test]
    fn ports_are_object_safe() {
        fn _assert_send_sync<T: Send + Sync>() {}
        _assert_send_sync::<Box<dyn SessionTokenStore>>();
        _assert_send_sync::<Box<dyn PasswordHasher>>();
        _assert_send_sync::<Box<dyn AuditLogger>>();
        _assert_send_sync::<Box<dyn EventPublisher>>();
    }
}
