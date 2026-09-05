//! 领域事件定义(PRD 5.3):事件定义在 domain,分发实现交给 infrastructure,
//! 未来替换为 NATS/Kafka 广播时 domain/application 无需改动。

use crate::player::PlayerId;
use crate::room::RoomId;
use chrono::{DateTime, Utc};

/// 聚合根关键状态变化产生的领域事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DomainEvent {
    PlayerLeveledUp {
        player_id: PlayerId,
        new_level: u32,
        at: DateTime<Utc>,
    },
    PlayerJoinedRoom {
        room_id: RoomId,
        player_id: PlayerId,
        at: DateTime<Utc>,
    },
    PlayerLeftRoom {
        room_id: RoomId,
        player_id: PlayerId,
        reason: String,
        at: DateTime<Utc>,
    },
    RoomClosed {
        room_id: RoomId,
        reason: String,
        at: DateTime<Utc>,
    },
}

impl DomainEvent {
    pub fn occurred_at(&self) -> DateTime<Utc> {
        match self {
            Self::PlayerLeveledUp { at, .. }
            | Self::PlayerJoinedRoom { at, .. }
            | Self::PlayerLeftRoom { at, .. }
            | Self::RoomClosed { at, .. } => *at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_expose_occurrence_time() {
        let at = Utc::now();
        let player = PlayerId(uuid::Uuid::now_v7());
        let room = RoomId(uuid::Uuid::now_v7());
        let event = DomainEvent::PlayerLeveledUp {
            player_id: player,
            new_level: 2,
            at,
        };
        assert_eq!(event.occurred_at(), at);
        let closed = DomainEvent::RoomClosed {
            room_id: room,
            reason: "停机".into(),
            at,
        };
        assert_eq!(closed.occurred_at(), at);
    }
}
