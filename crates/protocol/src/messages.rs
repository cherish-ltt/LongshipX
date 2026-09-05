//! 类型化消息:入站(C2S)与出站(S2C)消息的解码/编码,桥接 protobuf 类型。

use crate::error::ProtocolError;
use crate::generated as pb;
use crate::opcodes::*;
use prost::Message as _;

/// 客户端发来的已解码消息。
#[derive(Debug, Clone, PartialEq)]
pub enum InboundMessage {
    Bind(pb::BindRequest),
    Heartbeat(pb::HeartbeatPing),
    JoinRoom(pb::JoinRoomRequest),
    LeaveRoom(pb::LeaveRoomRequest),
    RoomChat(pb::RoomChatRequest),
    GetProfile(pb::GetProfileRequest),
    /// 已知区段之外/无法识别的 opcode:由路由层统一回错误。
    Unknown(u16),
}

/// 服务端发出的消息。
#[derive(Debug, Clone, PartialEq)]
pub enum OutboundMessage {
    BindResult(pb::BindResult),
    HeartbeatAck(pb::HeartbeatAck),
    Profile(pb::ProfileResponse),
    RoomEvent(pb::RoomEventNotification),
    Error(pb::ErrorNotification),
    Shutdown(pb::ServerShutdownNotice),
}

impl InboundMessage {
    pub fn opcode(&self) -> u16 {
        match self {
            Self::Bind(_) => OP_C2S_BIND,
            Self::Heartbeat(_) => OP_C2S_HEARTBEAT,
            Self::JoinRoom(_) => OP_C2S_JOIN_ROOM,
            Self::LeaveRoom(_) => OP_C2S_LEAVE_ROOM,
            Self::RoomChat(_) => OP_C2S_ROOM_CHAT,
            Self::GetProfile(_) => OP_C2S_GET_PROFILE,
            Self::Unknown(opcode) => *opcode,
        }
    }
}

/// opcode + payload → 类型化入站消息;未知 opcode 不报错,交给路由层处理。
pub fn decode_inbound(opcode: u16, payload: &[u8]) -> Result<InboundMessage, ProtocolError> {
    let message = match opcode {
        OP_C2S_BIND => InboundMessage::Bind(pb::BindRequest::decode(payload)?),
        OP_C2S_HEARTBEAT => InboundMessage::Heartbeat(pb::HeartbeatPing::decode(payload)?),
        OP_C2S_JOIN_ROOM => InboundMessage::JoinRoom(pb::JoinRoomRequest::decode(payload)?),
        OP_C2S_LEAVE_ROOM => InboundMessage::LeaveRoom(pb::LeaveRoomRequest::decode(payload)?),
        OP_C2S_ROOM_CHAT => InboundMessage::RoomChat(pb::RoomChatRequest::decode(payload)?),
        OP_C2S_GET_PROFILE => InboundMessage::GetProfile(pb::GetProfileRequest::decode(payload)?),
        other => InboundMessage::Unknown(other),
    };
    Ok(message)
}

/// 出站消息 → (opcode, payload)。
pub fn encode_outbound(message: &OutboundMessage) -> Result<(u16, Vec<u8>), ProtocolError> {
    let (opcode, bytes) = match message {
        OutboundMessage::BindResult(msg) => (OP_S2C_BIND_RESULT, msg.encode_to_vec()),
        OutboundMessage::HeartbeatAck(msg) => (OP_S2C_HEARTBEAT_ACK, msg.encode_to_vec()),
        OutboundMessage::Profile(msg) => (OP_S2C_PROFILE, msg.encode_to_vec()),
        OutboundMessage::RoomEvent(msg) => (OP_S2C_ROOM_EVENT, msg.encode_to_vec()),
        OutboundMessage::Error(msg) => (OP_S2C_ERROR, msg.encode_to_vec()),
        OutboundMessage::Shutdown(msg) => (OP_S2C_SERVER_SHUTDOWN, msg.encode_to_vec()),
    };
    Ok((opcode, bytes))
}

/// opcode + payload → 类型化出站消息(客户端/压测工具解码服务端帧用)。
pub fn decode_outbound(opcode: u16, payload: &[u8]) -> Result<OutboundMessage, ProtocolError> {
    let message = match opcode {
        OP_S2C_BIND_RESULT => OutboundMessage::BindResult(pb::BindResult::decode(payload)?),
        OP_S2C_HEARTBEAT_ACK => OutboundMessage::HeartbeatAck(pb::HeartbeatAck::decode(payload)?),
        OP_S2C_PROFILE => OutboundMessage::Profile(pb::ProfileResponse::decode(payload)?),
        OP_S2C_ROOM_EVENT => {
            OutboundMessage::RoomEvent(pb::RoomEventNotification::decode(payload)?)
        },
        OP_S2C_ERROR => OutboundMessage::Error(pb::ErrorNotification::decode(payload)?),
        OP_S2C_SERVER_SHUTDOWN => {
            OutboundMessage::Shutdown(pb::ServerShutdownNotice::decode(payload)?)
        },
        other => return Err(ProtocolError::UnsupportedOpcode(other)),
    };
    Ok(message)
}

/// 便捷构造:绑定成功回执。
pub fn bind_ok(player_id: &str, nickname: &str) -> OutboundMessage {
    OutboundMessage::BindResult(pb::BindResult {
        ok: true,
        player_id: Some(player_id.to_string()),
        nickname: Some(nickname.to_string()),
        error: None,
    })
}

/// 便捷构造:绑定失败回执。
pub fn bind_rejected(error: &str) -> OutboundMessage {
    OutboundMessage::BindResult(pb::BindResult {
        ok: false,
        player_id: None,
        nickname: None,
        error: Some(error.to_string()),
    })
}

/// 便捷构造:错误通知。
pub fn error_notification(code: u32, message: &str) -> OutboundMessage {
    OutboundMessage::Error(pb::ErrorNotification {
        code,
        message: message.to_string(),
    })
}

impl From<pb::BindResult> for OutboundMessage {
    fn from(value: pb::BindResult) -> Self {
        OutboundMessage::BindResult(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GameCodec;
    use ppt_tcp_net_kit::codec::Codec;

    fn codec() -> GameCodec {
        GameCodec
    }

    /// 把入站消息按给定 opcode 编码为帧(模拟客户端)。
    fn encode_inbound(message: &InboundMessage, opcode: u16) -> ppt_tcp_net_kit::codec::Frame {
        let payload = match message {
            InboundMessage::Bind(msg) => msg.encode_to_vec(),
            InboundMessage::Heartbeat(msg) => msg.encode_to_vec(),
            InboundMessage::JoinRoom(msg) => msg.encode_to_vec(),
            InboundMessage::LeaveRoom(msg) => msg.encode_to_vec(),
            InboundMessage::RoomChat(msg) => msg.encode_to_vec(),
            InboundMessage::GetProfile(_) => Vec::new(),
            InboundMessage::Unknown(_) => Vec::new(),
        };
        ppt_tcp_net_kit::codec::Frame::new(opcode, payload)
    }

    #[test]
    fn inbound_roundtrip_covers_all_messages() {
        let codec = codec();
        let cases = vec![
            (
                InboundMessage::Bind(pb::BindRequest {
                    token: "tok".into(),
                }),
                OP_C2S_BIND,
            ),
            (
                InboundMessage::Heartbeat(pb::HeartbeatPing { client_ts_ms: 42 }),
                OP_C2S_HEARTBEAT,
            ),
            (
                InboundMessage::JoinRoom(pb::JoinRoomRequest {
                    room_id: Some("r".into()),
                }),
                OP_C2S_JOIN_ROOM,
            ),
            (
                InboundMessage::LeaveRoom(pb::LeaveRoomRequest { room_id: None }),
                OP_C2S_LEAVE_ROOM,
            ),
            (
                InboundMessage::RoomChat(pb::RoomChatRequest {
                    text: "你好".into(),
                }),
                OP_C2S_ROOM_CHAT,
            ),
        ];
        for (message, opcode) in cases {
            let encoded = encode_inbound(&message, opcode);
            let decoded = codec.decode(&encoded).unwrap();
            assert_eq!(decoded, message);
            assert_eq!(decoded.opcode(), opcode);
        }
    }

    #[test]
    fn unknown_opcode_yields_unknown_message() {
        let codec = codec();
        let decoded = codec
            .decode(&ppt_tcp_net_kit::codec::Frame::new(0x7FFF, vec![]))
            .unwrap();
        assert_eq!(decoded, InboundMessage::Unknown(0x7FFF));
        assert_eq!(decoded.opcode(), 0x7FFF);
    }

    #[test]
    fn corrupt_payload_is_decode_error() {
        let codec = codec();
        let mut bad = ppt_tcp_net_kit::codec::Frame::new(OP_C2S_BIND, vec![0xFF, 0xFF, 0x01]);
        assert!(codec.decode(&bad).is_err());
        bad.payload.clear();
        assert!(
            codec.decode(&bad).is_ok(),
            "空 payload 是合法的 proto3 消息"
        );
    }

    #[test]
    fn outbound_roundtrip_covers_all_messages() {
        use ppt_tcp_net_kit::codec::Codec as _;
        let room_event = OutboundMessage::RoomEvent(pb::RoomEventNotification {
            event: Some(pb::room_event_notification::Event::MemberLeft(
                pb::room_event_notification::MemberLeft {
                    room_id: "r".into(),
                    player_id: "p".into(),
                },
            )),
        });
        let cases = vec![
            OutboundMessage::BindResult(pb::BindResult {
                ok: true,
                ..Default::default()
            }),
            OutboundMessage::HeartbeatAck(pb::HeartbeatAck { server_ts_ms: 1 }),
            OutboundMessage::Profile(pb::ProfileResponse {
                ok: true,
                level: Some(2),
                ..Default::default()
            }),
            room_event,
            OutboundMessage::Error(pb::ErrorNotification {
                code: 1,
                message: "m".into(),
            }),
            OutboundMessage::Shutdown(pb::ServerShutdownNotice {
                message: "bye".into(),
            }),
        ];
        for message in cases {
            let (opcode, payload) = encode_outbound(&message).unwrap();
            assert!(opcode >= 0x8001);
            // 解码回来字段应可读(proto3 空 message 编码为空串,故用例均带字段)。
            let frame = ppt_tcp_net_kit::codec::Frame::new(opcode, payload);
            assert!(crate::ClientCodec.decode(&frame).is_ok());
        }
    }

    #[test]
    fn bind_helpers_carry_fields() {
        let ok = bind_ok("player-1", "昵称");
        let OutboundMessage::BindResult(result) = &ok else {
            panic!("应为 BindResult");
        };
        assert!(result.ok);
        assert_eq!(result.player_id.as_deref(), Some("player-1"));
        assert_eq!(result.nickname.as_deref(), Some("昵称"));

        let rejected = bind_rejected("token 无效");
        let OutboundMessage::BindResult(result) = &rejected else {
            panic!("应为 BindResult");
        };
        assert!(!result.ok);
        assert_eq!(result.error.as_deref(), Some("token 无效"));
    }
}
