//! 有界发送队列与慢客户端策略(PRD 8.5 🔴)。
//!
//! 每个连接拥有一个有界 `mpsc` 发送队列,由专属写 task 消费;
//! 队列满即视为慢客户端,按"断开该连接"策略处理,防止内存被无限撑大。

use crate::codec::Frame;
use crate::error::NetError;
use tokio::sync::{mpsc, watch};

/// 发送队列的入站端:克隆廉价,可被连接处理器、房间广播、停机通知共享。
#[derive(Clone)]
pub struct OutboundSender {
    tx: mpsc::Sender<Frame>,
    disconnected: watch::Sender<bool>,
}

impl OutboundSender {
    pub fn new(tx: mpsc::Sender<Frame>) -> Self {
        Self {
            tx,
            disconnected: watch::channel(false).0,
        }
    }

    pub fn capacity(&self) -> usize {
        self.tx.max_capacity()
    }

    /// 非阻塞入队:满 → `QueueFull` 并永久标记断开,关闭 → `Closed`。
    pub fn try_send_frame(&self, frame: Frame) -> Result<(), NetError> {
        if self.is_closed() {
            return Err(NetError::Closed);
        }
        let result = match self.tx.try_send(frame) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => Err(NetError::QueueFull),
            Err(mpsc::error::TrySendError::Closed(_)) => Err(NetError::Closed),
        };
        if result.is_err() {
            self.disconnect();
        }
        result
    }

    /// 阻塞入队:仅用于低频关键消息(如停机通知)。
    pub async fn send_frame(&self, frame: Frame) -> Result<(), NetError> {
        tokio::select! {
            biased;
            _ = self.disconnected() => Err(NetError::Closed),
            result = self.tx.send(frame) => result.map_err(|_| NetError::Closed),
        }
    }

    pub fn is_closed(&self) -> bool {
        self.tx.is_closed() || *self.disconnected.borrow()
    }

    pub fn disconnect(&self) {
        self.disconnected.send_replace(true);
    }

    pub async fn disconnected(&self) {
        let mut signal = self.disconnected.subscribe();
        tokio::select! {
            _ = signal.wait_for(|closed| *closed) => {},
            _ = self.tx.closed() => {},
        }
    }

    pub(crate) fn disconnect_signal(&self) -> watch::Receiver<bool> {
        self.disconnected.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(n: u8) -> Frame {
        Frame::new(u16::from(n), vec![n])
    }

    #[tokio::test]
    async fn bounded_queue_reports_full_then_closed() {
        let (tx, mut rx) = mpsc::channel(2);
        let sender = OutboundSender::new(tx);
        sender.try_send_frame(frame(1)).unwrap();
        sender.try_send_frame(frame(2)).unwrap();
        assert!(matches!(
            sender.try_send_frame(frame(3)),
            Err(NetError::QueueFull)
        ));
        rx.recv().await.unwrap();
        assert!(
            sender.is_closed(),
            "draining old frames must not revive a failed queue"
        );
        assert!(matches!(
            sender.try_send_frame(frame(4)),
            Err(NetError::Closed)
        ));
        drop(rx);
        assert!(matches!(
            sender.try_send_frame(frame(4)),
            Err(NetError::Closed)
        ));
        assert!(sender.is_closed());
    }

    #[tokio::test]
    async fn send_frame_waits_for_capacity_and_fails_when_closed() {
        let (tx, mut rx) = mpsc::channel(1);
        let sender = OutboundSender::new(tx);
        sender.send_frame(frame(1)).await.unwrap();
        let pending = {
            let sender = sender.clone();
            tokio::spawn(async move { sender.send_frame(frame(2)).await })
        };
        assert_eq!(rx.recv().await.unwrap(), frame(1));
        pending.await.unwrap().unwrap();
        drop(rx);
        assert!(matches!(
            sender.send_frame(frame(3)).await,
            Err(NetError::Closed)
        ));
    }

    #[tokio::test]
    async fn capacity_reflects_bound() {
        let (tx, _rx) = mpsc::channel::<Frame>(7);
        let sender = OutboundSender::new(tx);
        assert_eq!(sender.capacity(), 7);
    }
}
