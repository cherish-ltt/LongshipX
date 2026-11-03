use super::*;

#[tokio::test]
async fn join_requires_leaving_current_room_without_losing_membership() {
    let server = spawn_server().await;
    let (bind, _) = make_player_with_token(&server, "room-owner").await;
    let mut client = TestClient::connect(server.addr).await;
    client.send(&InboundMessage::Bind(bind)).await;
    client.recv_expect("bound").await;
    client
        .send(&InboundMessage::JoinRoom(pb::JoinRoomRequest {
            room_id: None,
        }))
        .await;
    let OutboundMessage::RoomEvent(event) = client.recv_expect("created").await else {
        panic!("expected room event");
    };
    let Some(pb::room_event_notification::Event::MemberJoined(joined)) = event.event else {
        panic!("expected join receipt");
    };
    let first = joined.room_id;
    for target in [None, Some(uuid::Uuid::now_v7().to_string()), None] {
        client
            .send(&InboundMessage::JoinRoom(pb::JoinRoomRequest {
                room_id: target,
            }))
            .await;
        let reply = client.recv_expect("join rejected").await;
        assert!(
            matches!(reply, OutboundMessage::Error(ref err) if err.code == longshipx_protocol::ERR_CONFLICT),
            "already joined client must explicitly leave before joining again: {reply:?}"
        );
    }
    client
        .send(&InboundMessage::JoinRoom(pb::JoinRoomRequest {
            room_id: Some(first.clone()),
        }))
        .await;
    client
        .send(&InboundMessage::RoomChat(pb::RoomChatRequest {
            text: "still here".into(),
        }))
        .await;
    let OutboundMessage::RoomEvent(event) = client.recv_expect("original room chat").await else {
        panic!("expected room event");
    };
    let Some(pb::room_event_notification::Event::Chat(chat)) = event.event else {
        panic!("repeat join must not duplicate membership or its receipt");
    };
    assert_eq!(chat.room_id, first);
    assert_eq!(chat.text, "still here");
    client
        .send(&InboundMessage::LeaveRoom(pb::LeaveRoomRequest {
            room_id: Some(first),
        }))
        .await;
    client.recv_expect("left").await;
    client.recv_expect("closed").await;
    client
        .send(&InboundMessage::JoinRoom(pb::JoinRoomRequest {
            room_id: None,
        }))
        .await;
    let OutboundMessage::RoomEvent(event) = client.recv_expect("new room").await else {
        panic!("expected room event");
    };
    assert!(matches!(
        event.event,
        Some(pb::room_event_notification::Event::MemberJoined(_))
    ));
    server.shutdown_tx.send_replace(true);
}
