//! 游戏消息协议层(PRD 8.2 / 4.2):
//!
//! * `generated`:prost 从 proto/game.proto 生成(构建期);
//! * `messages`:类型化出/入站消息与帧编解码适配;
//! * `router`:opcode → handler 路由表;
//! * `GameCodec`:实现 net-kit 的 `Codec` trait,网关直接复用。

pub mod error;
pub mod messages;
pub mod opcodes;
pub mod router;

use prost::Message as _;

/// prost 生成的 protobuf 类型(package game)。
pub mod generated {
    include!(concat!(env!("OUT_DIR"), "/game.rs"));
}

pub use error::ProtocolError;
pub use messages::{InboundMessage, OutboundMessage, decode_inbound, encode_outbound};
pub use opcodes::*;
pub use router::Router;

/// 帧 ⇄ 类型化消息的编解码器(服务端视角:实现 net-kit::codec::Codec)。
pub struct GameCodec;

impl net_kit_codec::Codec for GameCodec {
    type In = InboundMessage;
    type Out = OutboundMessage;
    type Error = ProtocolError;

    fn decode(&self, frame: &net_kit_codec::Frame) -> Result<Self::In, Self::Error> {
        messages::decode_inbound(frame.opcode, &frame.payload)
    }

    fn encode(&self, message: &Self::Out) -> Result<net_kit_codec::Frame, Self::Error> {
        let (opcode, payload) = messages::encode_outbound(message)?;
        Ok(net_kit_codec::Frame::new(opcode, payload))
    }
}

/// 客户端视角编解码器(测试客户端/压测工具复用,PRD 11)。
pub struct ClientCodec;

impl net_kit_codec::Codec for ClientCodec {
    type In = OutboundMessage;
    type Out = InboundMessage;
    type Error = ProtocolError;

    fn decode(&self, frame: &net_kit_codec::Frame) -> Result<Self::In, Self::Error> {
        messages::decode_outbound(frame.opcode, &frame.payload)
    }

    fn encode(&self, message: &Self::Out) -> Result<net_kit_codec::Frame, Self::Error> {
        let (opcode, payload) = match message {
            InboundMessage::Bind(msg) => (opcodes::OP_C2S_BIND, msg.encode_to_vec()),
            InboundMessage::Heartbeat(msg) => (opcodes::OP_C2S_HEARTBEAT, msg.encode_to_vec()),
            InboundMessage::JoinRoom(msg) => (opcodes::OP_C2S_JOIN_ROOM, msg.encode_to_vec()),
            InboundMessage::LeaveRoom(msg) => (opcodes::OP_C2S_LEAVE_ROOM, msg.encode_to_vec()),
            InboundMessage::RoomChat(msg) => (opcodes::OP_C2S_ROOM_CHAT, msg.encode_to_vec()),
            InboundMessage::GetProfile(msg) => (opcodes::OP_C2S_GET_PROFILE, msg.encode_to_vec()),
            InboundMessage::Unknown(opcode) => {
                return Err(ProtocolError::UnsupportedOpcode(*opcode));
            },
        };
        Ok(net_kit_codec::Frame::new(opcode, payload))
    }
}

/// net-kit 类型别名(避免在文档与本 crate 中重复全路径)。
pub(crate) mod net_kit_codec {
    pub use ppt_tcp_net_kit::codec::{Codec, Frame};
}
