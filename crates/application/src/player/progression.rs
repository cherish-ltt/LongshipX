//! 命令用例:增加经验并结算升级,升级时发布领域事件(PRD 5.3/6.1)。

use crate::dto::PlayerProfile;
use crate::error::AppError;
use crate::ports::EventPublisher;
use chrono::Utc;
use longshipx_domain::{DomainEvent, PlayerId, PlayerRepository};
use std::sync::Arc;

pub struct GainExpUseCase {
    players: Arc<dyn PlayerRepository>,
    events: Arc<dyn EventPublisher>,
}

impl GainExpUseCase {
    pub fn new(players: Arc<dyn PlayerRepository>, events: Arc<dyn EventPublisher>) -> Self {
        Self { players, events }
    }

    pub async fn execute(
        &self,
        player_id: PlayerId,
        amount: u64,
    ) -> Result<PlayerProfile, AppError> {
        if amount == 0 {
            return Err(AppError::Validation("经验值必须大于 0".into()));
        }
        let mut player = self
            .players
            .find_by_id(player_id)
            .await?
            .ok_or_else(|| AppError::NotFound("玩家不存在".into()))?;
        if let Some(new_level) = player.gain_exp(amount) {
            self.events
                .publish(DomainEvent::PlayerLeveledUp {
                    player_id,
                    new_level,
                    at: Utc::now(),
                })
                .await?;
        }
        self.players.save(&player).await?;
        Ok(PlayerProfile::from(&player))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::AppError;
    use crate::fakes::FakePlayers;
    use chrono::Utc;
    use longshipx_domain::{AccountId, Nickname, Player};

    struct CollectingEvents(std::sync::Mutex<Vec<DomainEvent>>);

    #[async_trait::async_trait]
    impl EventPublisher for CollectingEvents {
        async fn publish(&self, event: DomainEvent) -> Result<(), AppError> {
            self.0.lock().unwrap().push(event);
            Ok(())
        }
    }

    async fn fixture() -> (Arc<FakePlayers>, PlayerId) {
        let players = Arc::new(FakePlayers::default());
        let player = Player::create(
            AccountId(uuid::Uuid::now_v7()),
            Nickname::try_new("阿快").unwrap(),
            Utc::now(),
        );
        let id = player.id();
        players.save(&player).await.unwrap();
        (players, id)
    }

    #[tokio::test]
    async fn adds_exp_without_level_up() {
        let (players, id) = fixture().await;
        let events = Arc::new(CollectingEvents(std::sync::Mutex::new(Vec::new())));
        let use_case = GainExpUseCase::new(players, events.clone());
        let profile = use_case.execute(id, 50).await.unwrap();
        assert_eq!(profile.level, 1);
        assert_eq!(profile.exp, 50);
        assert_eq!(events.0.lock().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn level_up_publishes_domain_event() {
        let (players, id) = fixture().await;
        let events = Arc::new(CollectingEvents(std::sync::Mutex::new(Vec::new())));
        let use_case = GainExpUseCase::new(players, events.clone());
        let profile = use_case.execute(id, 100).await.unwrap();
        assert_eq!(profile.level, 2);
        let published = events.0.lock().unwrap();
        assert!(matches!(
            published.last(),
            Some(DomainEvent::PlayerLeveledUp { new_level: 2, .. })
        ));
    }

    #[tokio::test]
    async fn zero_exp_is_validation_error() {
        let (players, id) = fixture().await;
        let events = Arc::new(CollectingEvents(std::sync::Mutex::new(Vec::new())));
        let use_case = GainExpUseCase::new(players, events);
        assert!(matches!(
            use_case.execute(id, 0).await,
            Err(AppError::Validation(_))
        ));
    }

    #[tokio::test]
    async fn unknown_player_is_not_found() {
        let (players, _) = fixture().await;
        let events = Arc::new(CollectingEvents(std::sync::Mutex::new(Vec::new())));
        let use_case = GainExpUseCase::new(players, events);
        assert!(matches!(
            use_case.execute(PlayerId(uuid::Uuid::now_v7()), 10).await,
            Err(AppError::NotFound(_))
        ));
    }
}
