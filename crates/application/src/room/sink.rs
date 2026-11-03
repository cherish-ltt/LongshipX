use super::RoomEvent;
use tokio::sync::{mpsc, watch};

/// Bounded events plus a persistent disconnect signal independent of queue capacity.
#[derive(Clone)]
pub struct RoomEventSink {
    tx: mpsc::Sender<RoomEvent>,
    failed: watch::Sender<bool>,
}

impl From<mpsc::Sender<RoomEvent>> for RoomEventSink {
    fn from(tx: mpsc::Sender<RoomEvent>) -> Self {
        Self {
            tx,
            failed: watch::channel(false).0,
        }
    }
}

impl RoomEventSink {
    pub fn try_send(&self, event: RoomEvent) {
        if self.tx.try_send(event).is_err() {
            self.failed.send_replace(true);
        }
    }

    pub async fn failed(&self) {
        let mut failed = self.failed.subscribe();
        let _ = failed.wait_for(|failed| *failed).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn failure_is_sticky_after_queue_recovers() {
        let (tx, mut rx) = mpsc::channel(1);
        let sink = RoomEventSink::from(tx);
        let event = RoomEvent::Closed {
            room_id: longshipx_domain::RoomId(uuid::Uuid::now_v7()),
            reason: "test".into(),
        };
        sink.try_send(event.clone());
        sink.try_send(event);
        rx.recv().await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), sink.failed())
            .await
            .unwrap();
    }
}
