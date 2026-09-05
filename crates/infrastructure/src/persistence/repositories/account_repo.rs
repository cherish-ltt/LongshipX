//! 账号仓储的 PostgreSQL/SeaORM 实现。

use crate::persistence::converters::{account_to_active, account_to_domain};
use crate::persistence::entities::account::{ActiveModel, Column, Entity as Accounts};
use async_trait::async_trait;
use longshipx_domain::{Account, AccountId, AccountRepository, RepoError};
use sea_orm::{ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
use std::sync::Arc;

pub struct SeaAccountRepository {
    db: Arc<DatabaseConnection>,
}

impl SeaAccountRepository {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }
}

#[async_trait]
impl AccountRepository for SeaAccountRepository {
    async fn find_by_id(&self, id: AccountId) -> Result<Option<Account>, RepoError> {
        Accounts::find_by_id(id.0)
            .one(self.db.as_ref())
            .await
            .map_err(|err| RepoError::Storage(err.to_string()))?
            .map(|model| account_to_domain(&model))
            .transpose()
    }

    async fn find_by_username(&self, username: &str) -> Result<Option<Account>, RepoError> {
        Accounts::find()
            .filter(Column::Username.eq(username))
            .one(self.db.as_ref())
            .await
            .map_err(|err| RepoError::Storage(err.to_string()))?
            .map(|model| account_to_domain(&model))
            .transpose()
    }

    async fn save(&self, account: &Account) -> Result<(), RepoError> {
        let exists = Accounts::find_by_id(account.id().0)
            .one(self.db.as_ref())
            .await
            .map_err(|err| RepoError::Storage(err.to_string()))?
            .is_some();
        let active: ActiveModel = account_to_active(account);
        let result = if exists {
            active.update(self.db.as_ref()).await
        } else {
            active.insert(self.db.as_ref()).await
        };
        result
            .map(|_| ())
            .map_err(|err| RepoError::Storage(err.to_string()))
    }
}
