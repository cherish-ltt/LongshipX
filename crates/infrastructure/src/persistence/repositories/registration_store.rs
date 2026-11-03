use crate::persistence::converters::{account_to_active, player_to_active};
use async_trait::async_trait;
use longshipx_application::AppError;
use longshipx_application::ports::RegistrationStore;
use longshipx_domain::{Account, Player};
use sea_orm::{ActiveModelTrait, DatabaseConnection, DbErr, SqlErr, TransactionTrait};
use std::sync::Arc;

pub struct SeaRegistrationStore {
    db: Arc<DatabaseConnection>,
}

impl SeaRegistrationStore {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }
}

#[async_trait]
impl RegistrationStore for SeaRegistrationStore {
    async fn register(&self, account: &Account, player: &Player) -> Result<(), AppError> {
        let txn = self.db.begin().await.map_err(map_error)?;
        // 未提交事务在错误或任务取消时回滚,不执行有竞态的补偿删除。
        account_to_active(account)
            .insert(&txn)
            .await
            .map_err(map_error)?;
        player_to_active(player)
            .insert(&txn)
            .await
            .map_err(map_error)?;
        txn.commit().await.map_err(map_error)
    }
}

fn map_error(err: DbErr) -> AppError {
    match err.sql_err() {
        Some(SqlErr::UniqueConstraintViolation(_)) => AppError::Conflict("注册数据已存在".into()),
        _ => AppError::Storage(err.to_string()),
    }
}
