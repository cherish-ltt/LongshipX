//! 在线连接注册表:优雅停机时用于广播维护通知(PRD 13.2)。

use longshipx_net_kit::OutboundSender;
use longshipx_net_kit::codec::Codec;
use longshipx_protocol::{OutboundMessage, ProtocolError};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use uuid::Uuid;

#[derive(Default)]
pub struct ConnectionRegistry {
    conns: Mutex<HashMap<Uuid, Option<OutboundSender>>>,
    admitted: Arc<AtomicUsize>,
}

pub(crate) struct ConnectionPermit(Arc<AtomicUsize>);

impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

impl ConnectionRegistry {
    pub(crate) fn try_acquire(&self, limit: usize) -> Option<ConnectionPermit> {
        self.admitted
            .try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < limit).then(|| count + 1)
            })
            .ok()?;
        Some(ConnectionPermit(self.admitted.clone()))
    }
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, conn_id: Uuid, sender: OutboundSender) {
        self.conns.lock().insert(conn_id, Some(sender));
    }

    pub fn remove(&self, conn_id: Uuid) -> Option<OutboundSender> {
        self.conns.lock().remove(&conn_id).flatten()
    }

    pub(crate) fn begin_drain(&self, conn_id: Uuid) {
        if let Some(sender) = self.conns.lock().get_mut(&conn_id) {
            sender.take();
        }
    }

    pub fn active_count(&self) -> usize {
        self.conns.lock().len()
    }

    /// 向所有在线连接尽力投递一条消息(如停机通知),返回成功入队数。
    pub fn broadcast(
        &self,
        codec: &dyn Codec<
            In = longshipx_protocol::InboundMessage,
            Out = OutboundMessage,
            Error = ProtocolError,
        >,
        message: &OutboundMessage,
    ) -> usize {
        let Ok(frame) = codec.encode(message) else {
            tracing::error!("广播消息编码失败");
            return 0;
        };
        let conns = self.conns.lock();
        conns
            .values()
            .flatten()
            .filter(|sender| sender.try_send_frame(frame.clone()).is_ok())
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use longshipx_net_kit::codec::Frame;
    use longshipx_protocol::GameCodec;
    use longshipx_protocol::generated::HeartbeatAck;

    struct EchoCodec;

    impl Codec for EchoCodec {
        type In = longshipx_protocol::InboundMessage;
        type Out = OutboundMessage;
        type Error = ProtocolError;

        fn decode(&self, _frame: &Frame) -> Result<Self::In, Self::Error> {
            unreachable!("broadcast 测试只编码")
        }

        fn encode(&self, message: &Self::Out) -> Result<Frame, Self::Error> {
            GameCodec.encode(message)
        }
    }

    fn ack() -> OutboundMessage {
        OutboundMessage::HeartbeatAck(HeartbeatAck { server_ts_ms: 1 })
    }

    #[test]
    fn shared_admission_limit_is_atomic_and_raii_releases() {
        let registry = Arc::new(ConnectionRegistry::new());
        let barrier = Arc::new(std::sync::Barrier::new(16));
        let handles: Vec<_> = (0..16)
            .map(|_| {
                let registry = registry.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    registry.try_acquire(4)
                })
            })
            .collect();
        let permits: Vec<_> = handles
            .into_iter()
            .filter_map(|handle| handle.join().unwrap())
            .collect();
        assert_eq!(permits.len(), 4);
        assert!(registry.try_acquire(4).is_none());
        drop(permits);
        assert!(registry.try_acquire(1).is_some());
        assert!(registry.try_acquire(0).is_none());
    }

    #[test]
    fn register_count_remove_lifecycle() {
        let registry = ConnectionRegistry::new();
        let (tx, _rx) = tokio::sync::mpsc::channel::<Frame>(4);
        let conn = Uuid::now_v7();
        registry.register(conn, OutboundSender::new(tx));
        assert_eq!(registry.active_count(), 1);
        assert!(registry.remove(conn).is_some());
        assert_eq!(registry.active_count(), 0);
    }

    #[tokio::test]
    async fn broadcast_delivers_to_live_queues() {
        let registry = ConnectionRegistry::new();
        let codec = EchoCodec;
        let (tx1, mut rx1) = tokio::sync::mpsc::channel::<Frame>(4);
        let (tx2, mut rx2) = tokio::sync::mpsc::channel::<Frame>(4);
        registry.register(Uuid::now_v7(), OutboundSender::new(tx1));
        registry.register(Uuid::now_v7(), OutboundSender::new(tx2));
        let sent = registry.broadcast(&codec, &ack());
        assert_eq!(sent, 2);
        assert!(rx1.recv().await.is_some());
        assert!(rx2.recv().await.is_some());

        // 队列关闭后不再计入。
        drop(rx1);
        drop(rx2);
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert_eq!(registry.broadcast(&codec, &ack()), 0);
    }
}
