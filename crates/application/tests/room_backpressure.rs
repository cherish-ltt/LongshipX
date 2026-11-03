use longshipx_application::{
    AppError, EventPublisher, RoomEvent, RoomEventSink, RoomService, RoomServiceConfig,
};
use longshipx_domain::{DomainEvent, PlayerId};
use std::sync::Arc;
use tokio::sync::mpsc;

struct Events;
#[async_trait::async_trait]
impl EventPublisher for Events {
    async fn publish(&self, _: DomainEvent) -> Result<(), AppError> {
        Ok(())
    }
}

fn service(capacity: usize, max_players: u32) -> RoomService {
    RoomService::new(
        RoomServiceConfig {
            command_capacity: capacity,
            max_players,
        },
        Arc::new(Events),
    )
}
fn player() -> PlayerId {
    PlayerId(uuid::Uuid::now_v7())
}

#[tokio::test]
async fn full_event_queue_signals_disconnect_when_leave_receipt_cannot_fit() {
    let rooms = service(512, 2);
    let id = player();
    let (tx, mut rx) = mpsc::channel(64);
    let sink = RoomEventSink::from(tx);
    let room = rooms
        .join_or_create(None, id, "p".into(), sink.clone())
        .await
        .unwrap();
    // Fill the production-sized event queue: one Join receipt plus 63 chats.
    for n in 0..63 {
        rooms.chat(room, id, format!("queued {n}")).await.unwrap();
    }
    rooms.leave(room, id, "client leave".into()).await.unwrap();
    // A subsequent join waits for Leave to be processed; the room is then closed.
    let (next_tx, _next_rx) = mpsc::channel(1);
    let _ = rooms
        .join_or_create(Some(room), player(), "barrier".into(), next_tx)
        .await;
    assert!(matches!(
        rx.recv().await,
        Some(RoomEvent::MemberJoined { .. })
    ));
    for _ in 0..63 {
        assert!(matches!(rx.recv().await, Some(RoomEvent::Chat { .. })));
    }
    tokio::time::timeout(std::time::Duration::from_secs(1), sink.failed())
        .await
        .unwrap();
    rooms.close_all("probe done").await.unwrap();
}

#[tokio::test]
async fn join_and_close_failures_signal_disconnect_without_blocking_healthy_members() {
    for closed_receiver in [false, true] {
        let rooms = service(16, 4);
        let (slow_tx, slow_rx) = mpsc::channel(1);
        let sink = RoomEventSink::from(slow_tx);
        let room = rooms
            .join_or_create(None, player(), "slow".into(), sink.clone())
            .await
            .unwrap();
        if closed_receiver {
            drop(slow_rx);
        }
        let (fast_tx, mut fast_rx) = mpsc::channel(8);
        rooms
            .join_or_create(Some(room), player(), "fast".into(), fast_tx)
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), sink.failed())
            .await
            .unwrap();
        assert!(matches!(
            fast_rx.recv().await,
            Some(RoomEvent::MemberJoined { .. })
        ));
        rooms.close_all("test close").await.unwrap();
        assert!(matches!(
            fast_rx.recv().await,
            Some(RoomEvent::Closed { .. })
        ));
    }
    let rooms = service(16, 1);
    let (tx, _rx) = mpsc::channel(1);
    let sink = RoomEventSink::from(tx);
    rooms
        .join_or_create(None, player(), "slow".into(), sink.clone())
        .await
        .unwrap();
    rooms.close_all("full at close").await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(1), sink.failed())
        .await
        .unwrap();
}
