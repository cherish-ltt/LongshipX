//! 协议层错误。

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProtocolError {
    #[error("protobuf 解码失败: {0}")]
    Decode(String),
    #[error("protobuf 编码失败: {0}")]
    Encode(String),
    #[error("未支持的 opcode: {0:#06x}")]
    UnsupportedOpcode(u16),
    #[error("路由处理器错误: {0}")]
    Handler(String),
}

impl From<prost::DecodeError> for ProtocolError {
    fn from(err: prost::DecodeError) -> Self {
        Self::Decode(err.to_string())
    }
}

impl From<prost::EncodeError> for ProtocolError {
    fn from(err: prost::EncodeError) -> Self {
        Self::Encode(err.to_string())
    }
}
