//! 长度前缀帧协议(PRD 8.2):`[4字节长度 u32 BE][2字节 opcode][protobuf payload]`。
//!
//! 🔴 帧长度必须校验上限:不校验等于给恶意客户端一个用极小流量耗尽内存的攻击面。

use crate::error::NetError;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// 帧头长度字段的字节数。
pub const LENGTH_PREFIX_BYTES: usize = 4;
/// opcode 字段的字节数。
pub const OPCODE_BYTES: usize = 2;
/// 帧体最小长度(至少包含 opcode)。
pub const MIN_FRAME_LEN: usize = OPCODE_BYTES;

/// 一条线上帧:opcode + 业务消息体(不含长度前缀)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub opcode: u16,
    pub payload: Vec<u8>,
}

impl Frame {
    pub fn new(opcode: u16, payload: Vec<u8>) -> Self {
        Self { opcode, payload }
    }

    /// 长度前缀字段的值 = opcode + payload 的总字节数。
    pub fn wire_len(&self) -> usize {
        OPCODE_BYTES + self.payload.len()
    }
}

/// 协议实现方(net-kit 之外,如 protocol crate)把帧与类型化消息互转。
pub trait Codec: Send + Sync + 'static {
    type In;
    type Out;
    type Error: std::error::Error + Send + Sync + 'static;

    fn decode(&self, frame: &Frame) -> Result<Self::In, Self::Error>;
    fn encode(&self, message: &Self::Out) -> Result<Frame, Self::Error>;
}

/// 把帧编码为带长度前缀的字节流;超过 `max_frame_size` 直接拒绝(PRD 8.2 🔴)。
pub fn encode_frame(frame: &Frame, max_frame_size: usize) -> Result<Vec<u8>, NetError> {
    let wire_len = frame.wire_len();
    if wire_len > max_frame_size {
        return Err(NetError::FrameTooLarge {
            declared: wire_len,
            max: max_frame_size,
        });
    }
    if wire_len < MIN_FRAME_LEN {
        return Err(NetError::Malformed("帧体必须包含 opcode".into()));
    }
    let mut buf = Vec::with_capacity(LENGTH_PREFIX_BYTES + wire_len);
    buf.extend_from_slice(&(wire_len as u32).to_be_bytes());
    buf.extend_from_slice(&frame.opcode.to_be_bytes());
    buf.extend_from_slice(&frame.payload);
    Ok(buf)
}

/// 从字节流解析长度前缀值并校验上限。
pub fn decode_wire_len(
    prefix: &[u8; LENGTH_PREFIX_BYTES],
    max_frame_size: usize,
) -> Result<usize, NetError> {
    let declared = u32::from_be_bytes(*prefix) as usize;
    if declared < MIN_FRAME_LEN {
        return Err(NetError::Malformed(format!(
            "帧长 {declared} 小于最小值 {MIN_FRAME_LEN}"
        )));
    }
    if declared > max_frame_size {
        return Err(NetError::FrameTooLarge {
            declared,
            max: max_frame_size,
        });
    }
    Ok(declared)
}

/// 读取一帧;对端干净关闭(首个字节即 EOF)返回 `Ok(None)`。
pub async fn read_frame<R: AsyncRead + Unpin>(
    reader: &mut R,
    max_frame_size: usize,
) -> Result<Option<Frame>, NetError> {
    let mut prefix = [0u8; LENGTH_PREFIX_BYTES];
    match reader.read_exact(&mut prefix).await {
        Ok(_) => {},
        Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(err) => return Err(err.into()),
    }
    let wire_len = decode_wire_len(&prefix, max_frame_size)?;
    let mut opcode_buf = [0u8; OPCODE_BYTES];
    reader.read_exact(&mut opcode_buf).await?;
    let mut payload = vec![0u8; wire_len - OPCODE_BYTES];
    reader.read_exact(&mut payload).await?;
    Ok(Some(Frame {
        opcode: u16::from_be_bytes(opcode_buf),
        payload,
    }))
}

/// 写入一帧(含长度前缀)并冲刷。
pub async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    frame: &Frame,
    max_frame_size: usize,
) -> Result<(), NetError> {
    let bytes = encode_frame(frame, max_frame_size)?;
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    const MAX: usize = 64;

    fn frame(opcode: u16, payload: &[u8]) -> Frame {
        Frame::new(opcode, payload.to_vec())
    }

    #[test]
    fn encode_prefix_is_big_endian_total_len() {
        let bytes = encode_frame(&frame(0x0102, b"hi"), MAX).unwrap();
        assert_eq!(
            &bytes[..4],
            &4u32.to_be_bytes(),
            "长度前缀 = opcode(2) + payload(2)"
        );
        assert_eq!(&bytes[4..6], &[0x01, 0x02]);
        assert_eq!(&bytes[6..], b"hi");
    }

    #[test]
    fn encode_rejects_oversize_and_allows_empty_body() {
        let big = Frame::new(1, vec![0u8; MAX]);
        assert!(matches!(
            encode_frame(&big, MAX),
            Err(NetError::FrameTooLarge { declared, max }) if declared == MAX + 2 && max == MAX
        ));
        // 仅含 opcode 的空 payload 是合法帧(wire_len = 2)。
        assert!(encode_frame(&Frame::new(1, vec![]), MAX).is_ok());
    }

    #[test]
    fn decode_wire_len_validates_bounds() {
        let ok = (MIN_FRAME_LEN as u32).to_be_bytes();
        assert_eq!(decode_wire_len(&ok, MAX).unwrap(), MIN_FRAME_LEN);
        let tiny = 1u32.to_be_bytes();
        assert!(matches!(
            decode_wire_len(&tiny, MAX),
            Err(NetError::Malformed(_))
        ));
        let huge = ((MAX as u32) + 1).to_be_bytes();
        assert!(matches!(
            decode_wire_len(&huge, MAX),
            Err(NetError::FrameTooLarge { .. })
        ));
    }

    #[tokio::test]
    async fn read_write_roundtrip() {
        let (mut client, mut server) = duplex(256);
        let sent = frame(0x00ff, b"payload");
        write_frame(&mut client, &sent, MAX).await.unwrap();
        let received = read_frame(&mut server, MAX).await.unwrap().unwrap();
        assert_eq!(received, sent);
    }

    #[tokio::test]
    async fn read_frame_returns_none_on_clean_eof() {
        let (client, mut server) = duplex(64);
        drop(client);
        assert!(read_frame(&mut server, MAX).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn read_frame_rejects_oversized_declaration() {
        let (mut client, mut server) = duplex(64);
        client
            .write_all(&((MAX as u32) + 1).to_be_bytes())
            .await
            .unwrap();
        drop(client);
        assert!(matches!(
            read_frame(&mut server, MAX).await,
            Err(NetError::FrameTooLarge { .. })
        ));
    }

    #[tokio::test]
    async fn read_frame_rejects_malformed_short_frame() {
        let (mut client, mut server) = duplex(64);
        client.write_all(&1u32.to_be_bytes()).await.unwrap();
        drop(client);
        assert!(matches!(
            read_frame(&mut server, MAX).await,
            Err(NetError::Malformed(_))
        ));
    }
}
