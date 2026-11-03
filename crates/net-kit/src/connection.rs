//! 连接生命周期:读写分离(PRD 8.1 ⚠️)。
//!
//! 读半部交给调用方的处理循环;写半部由专属 task 从有界队列取帧写入 socket,
//! "发消息给某个连接"只是"往它的队列里塞一条消息",不在多 task 间竞争锁。

use crate::backpressure::OutboundSender;
use crate::codec::{Frame, read_frame};
use crate::error::NetError;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt as _, ReadHalf, WriteHalf};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// 连接级参数(来自环境变量配置,PRD 18.1)。
#[derive(Debug, Clone, Copy)]
pub struct ConnectionConfig {
    pub write_drain_timeout: std::time::Duration,
    /// 单帧长度上限(字节)。
    pub max_frame_size: usize,
    /// 发送队列容量(有界,PRD 8.5 🔴)。
    pub send_queue_capacity: usize,
}

/// 读半部封装:调用方循环调用 `read_frame`。
pub struct FrameReader<S> {
    stream: ReadHalf<S>,
    max_frame_size: usize,
}

impl<S: AsyncRead + Unpin> FrameReader<S> {
    pub async fn read_frame(&mut self) -> Result<Option<Frame>, NetError> {
        read_frame(&mut self.stream, self.max_frame_size).await
    }
}

/// 拆分连接:返回读半部、发送端与写 task 句柄。
/// 最后发送端关闭后按 write_drain_timeout 有限排空,随后释放 socket 写半部。
pub fn split_connection<S>(
    io: S,
    config: ConnectionConfig,
) -> (FrameReader<S>, OutboundSender, JoinHandle<()>)
where
    S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    let (read_half, write_half) = tokio::io::split(io);
    let (tx, rx) = mpsc::channel(config.send_queue_capacity);
    let (lifetime_tx, mut lifetime_rx) = tokio::sync::watch::channel(());
    let writer = tokio::spawn(async move {
        let work = write_task(write_half, rx, config.max_frame_size);
        tokio::pin!(work);
        tokio::select! {
            _ = &mut work => {},
            _ = lifetime_rx.changed() => {
                if tokio::time::timeout(config.write_drain_timeout, work).await.is_err() {
                    tracing::debug!("writer drain deadline exceeded");
                }
            },
        }
    });
    (
        FrameReader {
            stream: read_half,
            max_frame_size: config.max_frame_size,
        },
        OutboundSender::with_lifetime(tx, lifetime_tx),
        writer,
    )
}

/// 写 task:串行消费队列;IO 错误或队列关闭即退出。
async fn write_task<S>(mut sink: WriteHalf<S>, mut rx: mpsc::Receiver<Frame>, max_frame_size: usize)
where
    S: AsyncWrite + Send + Unpin + 'static,
{
    while let Some(frame) = rx.recv().await {
        let bytes = match crate::codec::encode_frame(&frame, max_frame_size) {
            Ok(bytes) => bytes,
            Err(err) => {
                tracing::warn!(error = %err, opcode = frame.opcode, "待发送帧非法,跳过");
                continue;
            },
        };
        let write = async {
            sink.write_all(&bytes).await?;
            sink.flush().await
        };
        if let Err(err) = write.await {
            tracing::debug!(error = %err, "写半部关闭,连接写 task 退出");
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::write_frame;
    use std::time::Duration;

    const CONFIG: ConnectionConfig = ConnectionConfig {
        write_drain_timeout: Duration::from_millis(50),
        max_frame_size: 1024,
        send_queue_capacity: 4,
    };

    #[tokio::test]
    async fn frames_flow_from_sender_to_reader() {
        // 方向语义:本端发送队列的数据到达**对端**读半部;对端写半部 → 本端读半部。
        let (client, server) = tokio::io::duplex(4096);
        let (mut local_reader, local_sender, _writer) = split_connection(client, CONFIG);
        let (mut remote_reader, mut remote_writer) = tokio::io::split(server);

        local_sender
            .try_send_frame(Frame::new(0x0001, b"hello".to_vec()))
            .unwrap();
        let sent = crate::codec::read_frame(&mut remote_reader, CONFIG.max_frame_size)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(sent, Frame::new(0x0001, b"hello".to_vec()));

        write_frame(
            &mut remote_writer,
            &Frame::new(0x8002, b"ack".to_vec()),
            CONFIG.max_frame_size,
        )
        .await
        .unwrap();
        let received = local_reader.read_frame().await.unwrap().unwrap();
        assert_eq!(received, Frame::new(0x8002, b"ack".to_vec()));
    }

    #[tokio::test]
    async fn dropping_sender_ends_writer_task() {
        let (client, _server) = tokio::io::duplex(512);
        let (_reader, sender, writer) = split_connection(client, CONFIG);
        drop(sender);
        let result = tokio::time::timeout(Duration::from_secs(1), writer).await;
        assert!(result.is_ok(), "发送端关闭后写 task 应当退出");
    }

    #[tokio::test]
    async fn dropping_sender_flushes_queued_frame_for_reading_peer() {
        let (local, mut peer) = tokio::io::duplex(512);
        let (_reader, sender, writer) = split_connection(local, CONFIG);
        sender.try_send_frame(Frame::new(1, vec![7; 128])).unwrap();
        drop(sender);
        let frame = tokio::time::timeout(
            Duration::from_secs(1),
            crate::codec::read_frame(&mut peer, 1024),
        )
        .await
        .unwrap()
        .unwrap()
        .unwrap();
        assert_eq!(frame.payload, vec![7; 128]);
        writer.await.unwrap();
    }

    #[tokio::test]
    async fn oversized_frame_is_skipped_not_fatal() {
        let (client, _server) = tokio::io::duplex(512);
        let (_reader, sender, _writer) = split_connection(client, CONFIG);
        // 超过 max_frame_size 的帧在编码时被拒,写 task 不应崩溃。
        let oversized = Frame::new(1, vec![0u8; CONFIG.max_frame_size]);
        assert!(
            sender.try_send_frame(oversized).is_ok(),
            "入队本身不受帧长限制"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    #[tokio::test]
    async fn writer_stops_after_senders_drop_even_when_peer_does_not_read() {
        let (local, _peer) = tokio::io::duplex(1);
        let (_reader, sender, mut writer) = split_connection(
            local,
            ConnectionConfig {
                write_drain_timeout: Duration::from_millis(50),
                max_frame_size: 1024,
                send_queue_capacity: 1,
            },
        );
        sender.try_send_frame(Frame::new(1, vec![0; 128])).unwrap();
        tokio::task::yield_now().await;
        drop(sender);
        let completed = tokio::time::timeout(std::time::Duration::from_millis(200), &mut writer)
            .await
            .is_ok();
        writer.abort();
        assert!(
            completed,
            "writer remains blocked after every sender was dropped"
        );
    }
}
