//! 单连接主循环:读帧 → 限流/鉴权门 → 路由分发 → 有界发送(PRD 8/9)。

use crate::tcp::config::TcpGatewayConfig;
use crate::tcp::context::{ConnContext, GatewayDeps};
use crate::tcp::convert::room_event_to_notification;
use crate::tcp::handlers::app_error_notification;
use crate::tcp::rate_limit::TokenBucket;
use longshipx_application::RoomEvent;
use longshipx_domain::Session;
use longshipx_net_kit::ConnectionConfig;
use longshipx_net_kit::{FrameReader, TlsTcpStream, split_connection};
use longshipx_protocol::OutboundMessage;
use longshipx_protocol::messages::error_notification;
use longshipx_protocol::opcodes::*;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::mpsc;

/// 连接处理入口(每连接一个 task,panic 只影响本连接,PRD 9.3)。
pub async fn handle_connection(
    stream: TlsTcpStream,
    peer: SocketAddr,
    deps: Arc<GatewayDeps>,
    config: TcpGatewayConfig,
) {
    let conn_id = uuid::Uuid::now_v7();
    metrics::gauge!("longshipx_connections_active").increment(1.0);

    // 🔴 未鉴权连接按 IP 限量(PRD 8.3):超限直接关闭,连帧都不读。
    let holding_unauth_slot = deps.auth_gate.acquire(peer.ip());
    if !holding_unauth_slot {
        metrics::counter!("longshipx_unauth_rejected_total").increment(1);
        tracing::warn!(%peer, "未鉴权连接超过单 IP 上限,拒绝");
        return;
    }

    let (mut reader, outbound, writer) = split_connection(
        stream,
        ConnectionConfig {
            max_frame_size: config.max_frame_size,
            send_queue_capacity: config.send_queue_capacity,
        },
    );
    deps.connections.register(conn_id, outbound.clone());

    // 房间事件翻译 task:RoomEvent → 协议帧 → 发送队列。
    let (room_tx, room_rx) = mpsc::channel::<RoomEvent>(config.room_event_capacity);
    let translator = tokio::spawn(room_event_translator(
        room_rx,
        outbound.clone(),
        deps.codec.clone(),
    ));

    let ctx = ConnContext {
        conn_id,
        peer_ip: peer.ip(),
        outbound: outbound.clone(),
        room_tx,
        auth: Arc::new(parking_lot::Mutex::new(Default::default())),
        deps: deps.clone(),
    };
    let mut session = Session::new(chrono::Utc::now());

    run_read_loop(&ctx, &mut reader, &mut session, &config).await;

    // ── 清理:离房 → 释放限流名额 → 摘除注册表 → 关闭发送队列 ──
    if let Some(player) = ctx.authed_player() {
        let _ = deps
            .rooms
            .leave_all(player.player_id, "connection closed")
            .await;
    }
    if holding_unauth_slot && !ctx.is_authenticated() {
        deps.auth_gate.release(peer.ip());
    }
    deps.connections.remove(conn_id);
    translator.abort();
    drop(outbound);
    let _ = writer.await;
    metrics::gauge!("longshipx_connections_active").decrement(1.0);
    tracing::debug!(conn = %conn_id, %peer, "连接结束");
}

/// 读循环:超时/限流/解码/鉴权门/分发。返回即连接结束。
async fn run_read_loop(
    ctx: &ConnContext,
    reader: &mut FrameReader<TlsTcpStream>,
    session: &mut Session,
    config: &TcpGatewayConfig,
) {
    let mut bucket = TokenBucket::new(config.rate);
    loop {
        // 鉴权状态以 ctx 为准(BIND 处理器写入 ctx.auth)。
        let timeout = if ctx.is_authenticated() {
            config.heartbeat_timeout
        } else {
            config.unauth_timeout
        };
        let deadline = tokio::time::Instant::now() + timeout;
        tokio::select! {
            frame = reader.read_frame() => match frame {
                Ok(Some(frame)) => {
                    session.touch_heartbeat(chrono::Utc::now());
                    if !admit_frame(ctx, bucket.as_mut(), frame).await {
                        return;
                    }
                }
                Ok(None) => return, // 对端干净关闭
                Err(err) => {
                    if err.is_disconnect() {
                        tracing::debug!(conn = %ctx.conn_id, error = %err, "对端断开");
                    } else {
                        let code = if matches!(err, longshipx_net_kit::NetError::FrameTooLarge { .. }) {
                            ERR_FRAME_TOO_LARGE
                        } else {
                            ERR_PROTOCOL
                        };
                        let _ = ctx.try_send(error_notification(code, &err.to_string()));
                        tracing::warn!(conn = %ctx.conn_id, error = %err, "读帧失败,断开");
                    }
                    return;
                }
            },
            _ = tokio::time::sleep_until(deadline) => {
                tracing::info!(conn = %ctx.conn_id, "会话超时,断开");
                let _ = ctx.try_send(error_notification(ERR_TIMEOUT, "会话超时"));
                return;
            }
        }
    }
}

/// 帧的准入与分发;返回 false 表示连接应终止。
async fn admit_frame(
    ctx: &ConnContext,
    bucket: Option<&mut TokenBucket>,
    frame: longshipx_net_kit::Frame,
) -> bool {
    if let Some(bucket) = bucket
        && !bucket.try_acquire(std::time::Instant::now())
    {
        metrics::counter!("longshipx_rate_limited_total").increment(1);
        let _ = ctx.try_send(error_notification(ERR_RATE_LIMITED, "请求过于频繁"));
        return false;
    }
    let message = match ctx.deps.codec.decode(&frame) {
        Ok(message) => message,
        Err(err) => {
            // 解码失败意味着数据不可信:直接断开。
            let _ = ctx.try_send(error_notification(ERR_PROTOCOL, &err.to_string()));
            return false;
        },
    };

    // 🔴 鉴权门:第一条消息必须是 BIND;绑定后拒绝重复 BIND。
    if !ctx.is_authenticated() {
        if message.opcode() != OP_C2S_BIND {
            let _ = ctx.try_send(error_notification(
                ERR_AUTH_REQUIRED_FIRST,
                "必须先完成绑定",
            ));
            return false;
        }
    } else if message.opcode() == OP_C2S_BIND {
        let _ = ctx.try_send(error_notification(ERR_ALREADY_BOUND, "连接已完成绑定"));
        return true;
    }

    match ctx.deps.router.dispatch(ctx.clone(), message).await {
        Ok(Some(reply)) => ctx.try_send(reply).is_ok(),
        Ok(None) => true,
        Err(err) => {
            tracing::warn!(conn = %ctx.conn_id, error = %err, "处理器错误");
            ctx.try_send(app_error_notification(
                longshipx_application::AppError::Internal(err.to_string()),
            ))
            .is_ok()
        },
    }
}

/// 房间事件翻译:Actor 广播 → 协议帧;发送队列满即断开该连接。
async fn room_event_translator(
    mut rx: mpsc::Receiver<RoomEvent>,
    outbound: longshipx_net_kit::OutboundSender,
    codec: Arc<
        dyn longshipx_net_kit::codec::Codec<
                In = longshipx_protocol::InboundMessage,
                Out = OutboundMessage,
                Error = longshipx_protocol::ProtocolError,
            >,
    >,
) {
    while let Some(event) = rx.recv().await {
        let message = OutboundMessage::RoomEvent(room_event_to_notification(event));
        let Ok(frame) = codec.encode(&message) else {
            tracing::error!("房间事件编码失败");
            continue;
        };
        if outbound.try_send_frame(frame).is_err() {
            tracing::debug!("发送队列不可用,房间事件翻译退出");
            return;
        }
    }
}
