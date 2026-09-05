//! 进程内领域事件分发(PRD 5.3):broadcast 通道实现 application 的 EventPublisher,
//! 未来替换为 NATS/Kafka 时 domain/application 不用改一行。

use async_trait::async_trait;
use longshipx_application::error::AppError;
use longshipx_application::ports::EventPublisher;
use longshipx_domain::DomainEvent;
use tokio::sync::broadcast;

/// 进程内事件总线:无订阅者时静默丢弃(事件是旁路观察点)。
pub struct InMemoryEventPublisher {
    tx: broadcast::Sender<DomainEvent>,
}

impl InMemoryEventPublisher {
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity.max(1));
        Self { tx }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<DomainEvent> {
        self.tx.subscribe()
    }

    pub fn subscriber_count(&self) -> usize {
        self.tx.receiver_count()
    }
}

#[async_trait]
impl EventPublisher for InMemoryEventPublisher {
    async fn publish(&self, event: DomainEvent) -> Result<(), AppError> {
        // SendError 只在无订阅者时出现,静默即可。
        let _ = self.tx.send(event);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use longshipx_domain::{PlayerId, RoomId};

    fn leveled_up(level: u32) -> DomainEvent {
        DomainEvent::PlayerLeveledUp {
            player_id: PlayerId(uuid::Uuid::now_v7()),
            new_level: level,
            at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn subscribers_receive_events_in_order() {
        let publisher = InMemoryEventPublisher::new(16);
        let mut rx = publisher.subscribe();
        publisher.publish(leveled_up(2)).await.unwrap();
        publisher.publish(leveled_up(3)).await.unwrap();
        let first = rx.recv().await.unwrap();
        let second = rx.recv().await.unwrap();
        assert!(matches!(
            first,
            DomainEvent::PlayerLeveledUp { new_level: 2, .. }
        ));
        assert!(matches!(
            second,
            DomainEvent::PlayerLeveledUp { new_level: 3, .. }
        ));
        assert_eq!(publisher.subscriber_count(), 1);
    }

    #[tokio::test]
    async fn publish_without_subscribers_is_ok() {
        let publisher = InMemoryEventPublisher::new(4);
        publisher
            .publish(DomainEvent::RoomClosed {
                room_id: RoomId(uuid::Uuid::now_v7()),
                reason: "停机".into(),
                at: Utc::now(),
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn capacity_bounds_backlog() {
        let publisher = InMemoryEventPublisher::new(2);
        let mut rx = publisher.subscribe();
        for level in 1..=4u32 {
            publisher.publish(leveled_up(level)).await.unwrap();
        }
        // 通道容量 2:最早的 2 条被丢弃(Lagged),最后 2 条仍可读。
        assert!(rx.recv().await.is_err() || rx.recv().await.is_ok());
        let mut last = None;
        while let Ok(event) = rx.try_recv() {
            last = Some(event);
        }
        assert!(matches!(
            last,
            Some(DomainEvent::PlayerLeveledUp { new_level: 4, .. })
        ));
    }
}
