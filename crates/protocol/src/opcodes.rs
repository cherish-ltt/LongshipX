//! 消息 opcode 常量:2 字节,u16 BE。
//!
//! 区段划分:0x0001~0x0FFF 为 C2S;0x8001~0x8FFF 为 S2C。

pub const OP_C2S_BIND: u16 = 0x0001;
pub const OP_C2S_HEARTBEAT: u16 = 0x0002;
pub const OP_C2S_JOIN_ROOM: u16 = 0x0010;
pub const OP_C2S_LEAVE_ROOM: u16 = 0x0011;
pub const OP_C2S_ROOM_CHAT: u16 = 0x0012;
pub const OP_C2S_GET_PROFILE: u16 = 0x0013;

pub const OP_S2C_BIND_RESULT: u16 = 0x8001;
pub const OP_S2C_HEARTBEAT_ACK: u16 = 0x8002;
pub const OP_S2C_PROFILE: u16 = 0x8003;
pub const OP_S2C_ROOM_EVENT: u16 = 0x8010;
pub const OP_S2C_ERROR: u16 = 0x8011;
pub const OP_S2C_SERVER_SHUTDOWN: u16 = 0x8012;

/// 错误码(ErrorNotification.code):1xxx 协议层,2xxx 业务层。
pub const ERR_PROTOCOL: u32 = 1000;
pub const ERR_FRAME_TOO_LARGE: u32 = 1001;
pub const ERR_RATE_LIMITED: u32 = 1002;
pub const ERR_TIMEOUT: u32 = 1003;
pub const ERR_NOT_AUTHENTICATED: u32 = 1004;
pub const ERR_AUTH_REQUIRED_FIRST: u32 = 1005;
pub const ERR_ALREADY_BOUND: u32 = 1006;
pub const ERR_SERVER_BUSY: u32 = 2001;
pub const ERR_SERVER_SHUTDOWN: u32 = 2002;
pub const ERR_INVALID_INPUT: u32 = 2003;
pub const ERR_CONFLICT: u32 = 2004;
pub const ERR_NOT_FOUND: u32 = 2005;
pub const ERR_FORBIDDEN: u32 = 2006;
