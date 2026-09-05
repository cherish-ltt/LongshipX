//! 在线连接注册表:优雅停机时用于广播维护通知(PRD 13.2)。

use parking_lot::Mutex;
use ppt_tcp_net_kit::OutboundSender;
use ppt_tcp_net_kit::codec::Codec;
use ppt_tcp_protocol::{OutboundMessage, ProtocolError};
use std::collections::HashMap;
use uuid::Uuid;

#[derive(Default)]
pub struct ConnectionRegistry {
    conns: Mutex<HashMap<Uuid, OutboundSender>>,
}

impl ConnectionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, conn_id: Uuid, sender: OutboundSender) {
        self.conns.lock().insert(conn_id, sender);
    }

    pub fn remove(&self, conn_id: Uuid) -> Option<OutboundSender> {
        self.conns.lock().remove(&conn_id)
    }

    pub fn active_count(&self) -> usize {
        self.conns.lock().len()
    }

    /// 向所有在线连接尽力投递一条消息(如停机通知),返回成功入队数。
    pub fn broadcast(
        &self,
        codec: &dyn Codec<
            In = ppt_tcp_protocol::InboundMessage,
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
            .filter(|sender| sender.try_send_frame(frame.clone()).is_ok())
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ppt_tcp_net_kit::codec::Frame;
    use ppt_tcp_protocol::GameCodec;
    use ppt_tcp_protocol::generated::HeartbeatAck;

    struct EchoCodec;

    impl Codec for EchoCodec {
        type In = ppt_tcp_protocol::InboundMessage;
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
