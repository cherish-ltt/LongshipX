//! 消息路由装配:opcode → 处理器(PRD 4.3 protocol/router.rs 的填充方)。

use crate::tcp::context::ConnContext;
use crate::tcp::handlers;
use longshipx_protocol::Router;
use longshipx_protocol::opcodes::*;

pub fn build_router() -> Router<ConnContext> {
    let mut router = Router::new();
    router.route(OP_C2S_BIND, handlers::handle_bind);
    router.route(OP_C2S_HEARTBEAT, handlers::handle_heartbeat);
    router.route(OP_C2S_JOIN_ROOM, handlers::handle_join_room);
    router.route(OP_C2S_LEAVE_ROOM, handlers::handle_leave_room);
    router.route(OP_C2S_ROOM_CHAT, handlers::handle_room_chat);
    router.route(OP_C2S_GET_PROFILE, handlers::handle_get_profile);
    router
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_c2s_opcodes_have_routes() {
        let router = build_router();
        assert_eq!(router.route_count(), 6);
        for opcode in [
            OP_C2S_BIND,
            OP_C2S_HEARTBEAT,
            OP_C2S_JOIN_ROOM,
            OP_C2S_LEAVE_ROOM,
            OP_C2S_ROOM_CHAT,
            OP_C2S_GET_PROFILE,
        ] {
            assert!(router.has_route(opcode), "缺少 opcode {opcode:#06x} 的路由");
        }
    }
}
