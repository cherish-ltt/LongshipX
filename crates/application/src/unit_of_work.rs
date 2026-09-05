//! 事务边界抽象(PRD 6.2):应用层只感知"原子操作单元",不感知数据库事务;
//! infrastructure 用 SeaORM 的 DatabaseTransaction 实现真正的提交/回滚。

use crate::error::AppError;
use async_trait::async_trait;
use std::future::Future;
use std::pin::Pin;

/// 应用层闭包返回的 boxed future。
pub type UnitFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, AppError>> + Send + 'a>>;

#[async_trait]
pub trait UnitOfWork: Send + Sync {
    /// 在一个原子操作单元内执行 `operation`:全部成功则提交,任一失败则整体放弃。
    async fn commit<T, F>(&self, operation: F) -> Result<T, AppError>
    where
        T: Send,
        F: FnOnce() -> UnitFuture<'static, T> + Send;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct PassthroughUnitOfWork;

    #[async_trait]
    impl UnitOfWork for PassthroughUnitOfWork {
        async fn commit<T, F>(&self, operation: F) -> Result<T, AppError>
        where
            T: Send,
            F: FnOnce() -> UnitFuture<'static, T> + Send,
        {
            operation().await
        }
    }

    #[tokio::test]
    async fn commit_runs_operation_and_propagates_error() {
        let uow = PassthroughUnitOfWork;
        let ok: i32 = uow.commit(|| Box::pin(async { Ok(41 + 1) })).await.unwrap();
        assert_eq!(ok, 42);
        let err = uow
            .commit::<(), _>(|| Box::pin(async { Err(AppError::Busy) }))
            .await
            .unwrap_err();
        assert_eq!(err, AppError::Busy);
    }
}
