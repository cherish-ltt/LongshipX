//! 各 opcode 的路由处理器:绑定/心跳/加入房间/离开房间/房间聊天。

use crate::tcp::context::{AuthedPlayer, ConnContext};
use crate::tcp::convert::room_event_to_notification;
use chrono::Utc;
use ppt_tcp_application::error::AppError;
use ppt_tcp_domain::RoomId;
use ppt_tcp_protocol::InboundMessage;
use ppt_tcp_protocol::error::ProtocolError;
use ppt_tcp_protocol::generated as pb;
use ppt_tcp_protocol::generated::RoomEventNotification;
use ppt_tcp_protocol::messages::{OutboundMessage, bind_ok, bind_rejected, error_notification};
use ppt_tcp_protocol::opcodes::*;
use uuid::Uuid;

/// C2S_BIND:建连后的第一条消息,携带 opaque token 完成鉴权(PRD 8.3)。
pub async fn handle_bind(
    ctx: ConnContext,
    message: InboundMessage,
) -> Result<Option<OutboundMessage>, ProtocolError> {
    let InboundMessage::Bind(request) = message else {
        return Err(ProtocolError::Handler("BIND 路由收到异构消息".into()));
    };
    if ctx.is_authenticated() {
        return Ok(Some(error_notification(
            ERR_ALREADY_BOUND,
            "连接已完成绑定",
        )));
    }
    let token = request.token.trim();
    if token.is_empty() {
        return Ok(Some(bind_rejected("缺少 token")));
    }
    let player_id = match ctx.deps.tokens.resolve(token).await {
        Ok(Some(player_id)) => player_id,
        Ok(None) => {
            metrics::counter!("ppt_tcp_bind_rejected_total").increment(1);
            return Ok(Some(bind_rejected("token 无效或已过期")));
        },
        Err(err) => return Err(ProtocolError::Handler(err.to_string())),
    };
    let player = ctx
        .deps
        .players
        .find_by_id(player_id)
        .await
        .map_err(|err| ProtocolError::Handler(err.to_string()))?
        .ok_or_else(|| ProtocolError::Handler("token 对应的玩家不存在".into()))?;

    let nickname = player.nickname().as_str().to_string();
    ctx.set_authed(AuthedPlayer {
        player_id,
        nickname: nickname.clone(),
    });
    // 鉴权完成,退出未鉴权名额计数。
    ctx.deps.auth_gate.release(ctx.peer_ip);
    metrics::counter!("ppt_tcp_bind_total").increment(1);
    tracing::info!(conn = %ctx.conn_id, player = %player_id, "连接绑定成功");
    Ok(Some(bind_ok(&player_id.0.to_string(), &nickname)))
}

/// C2S_HEARTBEAT:刷新会话活跃时间并应答。
pub async fn handle_heartbeat(
    ctx: ConnContext,
    message: InboundMessage,
) -> Result<Option<OutboundMessage>, ProtocolError> {
    let InboundMessage::Heartbeat(ping) = message else {
        return Err(ProtocolError::Handler("HEARTBEAT 路由收到异构消息".into()));
    };
    tracing::trace!(conn = %ctx.conn_id, client_ts = ping.client_ts_ms, "心跳");
    Ok(Some(OutboundMessage::HeartbeatAck(pb::HeartbeatAck {
        server_ts_ms: Utc::now().timestamp_millis(),
    })))
}

/// C2S_JOIN_ROOM:加入/创建房间;回执通过房间广播(MemberJoined)送达。
pub async fn handle_join_room(
    ctx: ConnContext,
    message: InboundMessage,
) -> Result<Option<OutboundMessage>, ProtocolError> {
    let InboundMessage::JoinRoom(request) = message else {
        return Err(ProtocolError::Handler("JOIN_ROOM 路由收到异构消息".into()));
    };
    let Some(player) = ctx.authed_player() else {
        return Ok(Some(error_notification(
            ERR_NOT_AUTHENTICATED,
            "请先完成绑定",
        )));
    };
    let target = match parse_optional_room_id(request.room_id.as_deref()) {
        Ok(room_id) => room_id,
        Err(message_text) => return Ok(Some(error_notification(ERR_INVALID_INPUT, &message_text))),
    };
    match ctx
        .deps
        .rooms
        .join_or_create(
            target,
            player.player_id,
            player.nickname,
            ctx.room_tx.clone(),
        )
        .await
    {
        Ok(room_id) => {
            ctx.set_current_room(Some(room_id));
            Ok(None)
        },
        Err(err) => Ok(Some(app_error_notification(err))),
    }
}

/// C2S_LEAVE_ROOM:离开指定房间;未携带 room_id 时离开当前/全部房间。
pub async fn handle_leave_room(
    ctx: ConnContext,
    message: InboundMessage,
) -> Result<Option<OutboundMessage>, ProtocolError> {
    let InboundMessage::LeaveRoom(request) = message else {
        return Err(ProtocolError::Handler("LEAVE_ROOM 路由收到异构消息".into()));
    };
    let Some(player) = ctx.authed_player() else {
        return Ok(Some(error_notification(
            ERR_NOT_AUTHENTICATED,
            "请先完成绑定",
        )));
    };
    let target = match parse_optional_room_id(request.room_id.as_deref()) {
        Ok(room_id) => room_id,
        Err(message_text) => return Ok(Some(error_notification(ERR_INVALID_INPUT, &message_text))),
    };
    let outcome = match target {
        Some(room_id) => {
            let result = ctx
                .deps
                .rooms
                .leave(room_id, player.player_id, "client leave".into())
                .await;
            if result.is_ok() && ctx.current_room() == Some(room_id) {
                ctx.set_current_room(None);
            }
            result
        },
        None => {
            let result = ctx
                .deps
                .rooms
                .leave_all(player.player_id, "client leave")
                .await;
            if result.is_ok() {
                ctx.set_current_room(None);
            }
            result
        },
    };
    match outcome {
        Ok(()) => Ok(None),
        Err(err) => Ok(Some(app_error_notification(err))),
    }
}

/// C2S_ROOM_CHAT:投递聊天命令,内容广播走房间事件通道。
pub async fn handle_room_chat(
    ctx: ConnContext,
    message: InboundMessage,
) -> Result<Option<OutboundMessage>, ProtocolError> {
    let InboundMessage::RoomChat(request) = message else {
        return Err(ProtocolError::Handler("ROOM_CHAT 路由收到异构消息".into()));
    };
    let Some(player) = ctx.authed_player() else {
        return Ok(Some(error_notification(
            ERR_NOT_AUTHENTICATED,
            "请先完成绑定",
        )));
    };
    let Some(room_id) = ctx.current_room() else {
        return Ok(Some(error_notification(ERR_NOT_FOUND, "请先加入一个房间")));
    };
    match ctx
        .deps
        .rooms
        .chat(room_id, player.player_id, request.text)
        .await
    {
        Ok(()) => Ok(None),
        Err(err) => Ok(Some(app_error_notification(err))),
    }
}

/// C2S_GET_PROFILE:查询自己档案,服务端权威数值(PRD 第 10 章)。
pub async fn handle_get_profile(
    ctx: ConnContext,
    message: InboundMessage,
) -> Result<Option<OutboundMessage>, ProtocolError> {
    let InboundMessage::GetProfile(_) = message else {
        return Err(ProtocolError::Handler(
            "GET_PROFILE 路由收到异构消息".into(),
        ));
    };
    let Some(player) = ctx.authed_player() else {
        return Ok(Some(error_notification(
            ERR_NOT_AUTHENTICATED,
            "请先完成绑定",
        )));
    };
    match ctx.deps.profile.execute(player.player_id).await {
        Ok(profile) => Ok(Some(OutboundMessage::Profile(pb::ProfileResponse {
            ok: true,
            player_id: Some(profile.player_id.0.to_string()),
            nickname: Some(profile.nickname),
            level: Some(profile.level),
            exp: Some(profile.exp),
            last_login_at_ms: profile.last_login_at.map(|at| at.timestamp_millis()),
            error: None,
        }))),
        Err(err) => Ok(Some(app_error_notification(err))),
    }
}

/// 把应用层错误映射为面向客户端的错误通知(不泄漏内部细节)。
pub fn app_error_notification(err: AppError) -> OutboundMessage {
    let (code, message) = match err {
        AppError::Validation(msg) => (ERR_INVALID_INPUT, msg),
        AppError::Unauthorized(msg) => (ERR_NOT_AUTHENTICATED, msg),
        AppError::Forbidden(msg) => (ERR_FORBIDDEN, msg),
        AppError::NotFound(msg) => (ERR_NOT_FOUND, msg),
        AppError::Conflict(msg) => (ERR_CONFLICT, msg),
        AppError::Busy => (ERR_SERVER_BUSY, "服务繁忙,请稍后重试".into()),
        AppError::RateLimited => (ERR_RATE_LIMITED, "请求过于频繁".into()),
        AppError::Storage(_) | AppError::Internal(_) => {
            tracing::error!(error = %err, "应用层内部错误");
            (ERR_PROTOCOL, "服务器内部错误".into())
        },
    };
    error_notification(code, &message)
}

fn parse_optional_room_id(raw: Option<&str>) -> Result<Option<RoomId>, String> {
    match raw {
        None => Ok(None),
        Some(text) if text.trim().is_empty() => Ok(None),
        Some(text) => Uuid::parse_str(text.trim())
            .map(|uuid| Some(RoomId(uuid)))
            .map_err(|_| "room_id 不是合法的 UUID".into()),
    }
}

/// 由 `RoomEvent` 生成广播通知(供房间事件翻译 task 复用)。
pub fn room_event_message(event: ppt_tcp_application::RoomEvent) -> OutboundMessage {
    let notification: RoomEventNotification = room_event_to_notification(event);
    OutboundMessage::RoomEvent(notification)
}
