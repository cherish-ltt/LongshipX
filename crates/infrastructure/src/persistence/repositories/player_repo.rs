//! 玩家仓储的 PostgreSQL/SeaORM 实现。

use crate::persistence::converters::{player_to_active, player_to_domain};
use crate::persistence::entities::player::{ActiveModel, Column, Entity as Players};
use async_trait::async_trait;
use longshipx_domain::{AccountId, Player, PlayerId, PlayerRepository, RepoError};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter,
    QuerySelect, TransactionTrait,
};
use std::sync::Arc;

pub struct SeaPlayerRepository {
    db: Arc<DatabaseConnection>,
}

impl SeaPlayerRepository {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }
}

#[async_trait]
impl PlayerRepository for SeaPlayerRepository {
    async fn gain_exp(
        &self,
        id: PlayerId,
        amount: u64,
    ) -> Result<Option<(Player, Option<u32>)>, RepoError> {
        let txn = self.db.begin().await.map_err(storage_error)?;
        let Some(model) = Players::find_by_id(id.0)
            .lock_exclusive()
            .one(&txn)
            .await
            .map_err(storage_error)?
        else {
            return Ok(None);
        };
        let mut player = player_to_domain(&model)?;
        let level = player.gain_exp(amount);
        let active =
            ActiveModel {
                id: Set(id.0),
                level: Set(i32::try_from(player.level())
                    .map_err(|err| RepoError::Storage(err.to_string()))?),
                exp: Set(i64::try_from(player.exp())
                    .map_err(|err| RepoError::Storage(err.to_string()))?),
                ..Default::default()
            };
        active.update(&txn).await.map_err(storage_error)?;
        txn.commit().await.map_err(storage_error)?;
        Ok(Some((player, level)))
    }

    async fn record_login(
        &self,
        id: PlayerId,
        at: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<Player>, RepoError> {
        let active = ActiveModel {
            last_login_at: Set(Some(at.into())),
            ..Default::default()
        };
        Players::update_many()
            .set(active)
            .filter(Column::Id.eq(id.0))
            .exec_with_returning(self.db.as_ref())
            .await
            .map_err(storage_error)?
            .first()
            .map(player_to_domain)
            .transpose()
    }

    async fn find_by_id(&self, id: PlayerId) -> Result<Option<Player>, RepoError> {
        Players::find_by_id(id.0)
            .one(self.db.as_ref())
            .await
            .map_err(|err| RepoError::Storage(err.to_string()))?
            .map(|model| player_to_domain(&model))
            .transpose()
    }

    async fn find_by_account(&self, account_id: AccountId) -> Result<Option<Player>, RepoError> {
        Players::find()
            .filter(Column::AccountId.eq(account_id.0))
            .one(self.db.as_ref())
            .await
            .map_err(|err| RepoError::Storage(err.to_string()))?
            .map(|model| player_to_domain(&model))
            .transpose()
    }

    async fn save(&self, player: &Player) -> Result<(), RepoError> {
        let exists = Players::find_by_id(player.id().0)
            .one(self.db.as_ref())
            .await
            .map_err(|err| RepoError::Storage(err.to_string()))?
            .is_some();
        let active: ActiveModel = player_to_active(player);
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

fn storage_error(err: sea_orm::DbErr) -> RepoError {
    RepoError::Storage(err.to_string())
}
