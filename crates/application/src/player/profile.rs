//! 查询用例:玩家档案(只读,PRD 6.1 Query)。

use crate::dto::PlayerProfile;
use crate::error::AppError;
use longshipx_domain::{PlayerId, PlayerRepository};
use std::sync::Arc;

pub struct GetPlayerProfile {
    players: Arc<dyn PlayerRepository>,
}

impl GetPlayerProfile {
    pub fn new(players: Arc<dyn PlayerRepository>) -> Self {
        Self { players }
    }

    pub async fn execute(&self, player_id: PlayerId) -> Result<PlayerProfile, AppError> {
        let player = self
            .players
            .find_by_id(player_id)
            .await?
            .ok_or_else(|| AppError::NotFound("玩家不存在".into()))?;
        Ok(PlayerProfile::from(&player))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::AppError;
    use crate::fakes::FakePlayers;
    use chrono::Utc;
    use longshipx_domain::{Nickname, Player};

    #[tokio::test]
    async fn returns_profile_for_existing_player() {
        let players = FakePlayers::default();
        let player = Player::create(
            longshipx_domain::AccountId(uuid::Uuid::now_v7()),
            Nickname::try_new("阿宽").unwrap(),
            Utc::now(),
        );
        let id = player.id();
        players.save(&player).await.unwrap();

        let query = GetPlayerProfile::new(Arc::new(players));
        let profile = query.execute(id).await.unwrap();
        assert_eq!(profile.nickname, "阿宽");
        assert_eq!(profile.level, 1);
    }

    #[tokio::test]
    async fn missing_player_is_not_found() {
        let query = GetPlayerProfile::new(Arc::new(FakePlayers::default()));
        let err = query
            .execute(PlayerId(uuid::Uuid::now_v7()))
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::NotFound(_)));
    }
}
