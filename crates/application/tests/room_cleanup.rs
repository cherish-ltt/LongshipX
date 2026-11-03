use longshipx_application::{AppError, EventPublisher, RoomEvent, RoomService, RoomServiceConfig};
use longshipx_domain::{DomainEvent, PlayerId};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

struct Events;

#[async_trait::async_trait]
impl EventPublisher for Events {
    async fn publish(&self, _: DomainEvent) -> Result<(), AppError> {
        Ok(())
    }
}

fn service(command_capacity: usize, max_players: u32) -> RoomService {
    RoomService::new(
        RoomServiceConfig {
            command_capacity,
            max_players,
        },
        Arc::new(Events),
    )
}

fn player() -> PlayerId {
    PlayerId(uuid::Uuid::now_v7())
}

#[tokio::test]
async fn leave_all_waits_for_full_command_queue_and_removes_member() {
    let rooms = service(1, 1);
    let id = player();
    let (tx, mut rx) = mpsc::channel(8);
    let room = rooms
        .join_or_create(None, id, "first".into(), tx)
        .await
        .unwrap();
    assert!(matches!(
        rx.recv().await,
        Some(RoomEvent::MemberJoined { .. })
    ));
    rooms
        .chat(room, id, "occupy command slot".into())
        .await
        .unwrap();
    tokio::time::timeout(
        Duration::from_secs(1),
        rooms.leave_all(id, "connection closed"),
    )
    .await
    .unwrap()
    .unwrap();
    drop(rx);
    let (next_tx, _next_rx) = mpsc::channel(8);
    let next = rooms
        .join_or_create(Some(room), player(), "next".into(), next_tx)
        .await;
    assert!(
        matches!(next, Err(AppError::NotFound(_))),
        "last member must be gone: {next:?}"
    );
    rooms.leave_all(id, "again").await.unwrap();
    rooms.leave(room, id, "already gone".into()).await.unwrap();
}

#[tokio::test]
async fn closed_rooms_are_reclaimed_without_later_requests() {
    let rooms = service(8, 1);
    let (live_tx, _live_rx) = mpsc::channel(8);
    let live = rooms
        .join_or_create(None, player(), "live".into(), live_tx)
        .await
        .unwrap();
    for _ in 0..20 {
        let id = player();
        let (tx, mut rx) = mpsc::channel(8);
        let room = rooms
            .join_or_create(None, id, "temporary".into(), tx)
            .await
            .unwrap();
        assert!(matches!(
            rx.recv().await,
            Some(RoomEvent::MemberJoined { .. })
        ));
        rooms.leave(room, id, "leave".into()).await.unwrap();
        assert!(matches!(
            rx.recv().await,
            Some(RoomEvent::MemberLeft { .. })
        ));
        assert!(matches!(rx.recv().await, Some(RoomEvent::Closed { .. })));
        assert_eq!(rx.recv().await, None);
    }
    let reclaimed = tokio::time::timeout(Duration::from_millis(100), async {
        while rooms.active_rooms().await != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(
        reclaimed.is_ok(),
        "closed rooms remain retained: {}",
        rooms.active_rooms().await
    );
    assert!(rooms.chat(live, player(), "outsider".into()).await.is_ok());
    rooms.close_all("done").await.unwrap();
}
