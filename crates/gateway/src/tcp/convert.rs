//! 领域/应用事件 → 协议消息的适配(PRD 4.1:接口层负责翻译)。

use longshipx_application::RoomEvent;
use longshipx_domain::RoomId;
use longshipx_protocol::generated::RoomEventNotification;
use longshipx_protocol::generated::room_event_notification as pb_event;

fn room_id_str(id: RoomId) -> String {
    id.0.to_string()
}

/// RoomEvent → protobuf 通知。
pub fn room_event_to_notification(event: RoomEvent) -> RoomEventNotification {
    let payload = match event {
        RoomEvent::MemberJoined {
            room_id,
            player_id,
            nickname,
        } => pb_event::Event::MemberJoined(pb_event::MemberJoined {
            room_id: room_id_str(room_id),
            player_id: player_id.0.to_string(),
            nickname,
        }),
        RoomEvent::MemberLeft { room_id, player_id } => {
            pb_event::Event::MemberLeft(pb_event::MemberLeft {
                room_id: room_id_str(room_id),
                player_id: player_id.0.to_string(),
            })
        },
        RoomEvent::Chat {
            room_id,
            sender,
            nickname,
            text,
        } => pb_event::Event::Chat(pb_event::RoomChat {
            room_id: room_id_str(room_id),
            sender_id: sender.0.to_string(),
            sender_nickname: nickname,
            text,
        }),
        RoomEvent::Closed { room_id, reason } => {
            pb_event::Event::RoomClosed(pb_event::RoomClosed {
                room_id: room_id_str(room_id),
                reason,
            })
        },
    };
    RoomEventNotification {
        event: Some(payload),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use longshipx_domain::PlayerId;
    use longshipx_protocol::generated::room_event_notification::Event;

    #[test]
    fn every_room_event_maps_to_proto_oneof() {
        let room = RoomId(uuid::Uuid::now_v7());
        let player = PlayerId(uuid::Uuid::now_v7());

        let joined = room_event_to_notification(RoomEvent::MemberJoined {
            room_id: room,
            player_id: player,
            nickname: "玩家".into(),
        });
        assert!(matches!(
            joined.event,
            Some(Event::MemberJoined(inner)) if inner.room_id == room.0.to_string()
        ));

        let left = room_event_to_notification(RoomEvent::MemberLeft {
            room_id: room,
            player_id: player,
        });
        assert!(matches!(left.event, Some(Event::MemberLeft(_))));

        let chat = room_event_to_notification(RoomEvent::Chat {
            room_id: room,
            sender: player,
            nickname: "玩家".into(),
            text: "你好".into(),
        });
        assert!(matches!(chat.event, Some(Event::Chat(inner)) if inner.text == "你好"));

        let closed = room_event_to_notification(RoomEvent::Closed {
            room_id: room,
            reason: "停机".into(),
        });
        assert!(matches!(closed.event, Some(Event::RoomClosed(inner)) if inner.reason == "停机"));
    }
}
