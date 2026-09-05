//! 玩家仓储的 PostgreSQL/SeaORM 实现。

use crate::persistence::converters::{player_to_active, player_to_domain};
use crate::persistence::entities::player::{ActiveModel, Column, Entity as Players};
use async_trait::async_trait;
use longshipx_domain::{AccountId, Player, PlayerId, PlayerRepository, RepoError};
use sea_orm::{ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
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
