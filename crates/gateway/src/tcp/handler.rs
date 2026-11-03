//! 单连接主循环:读帧 → 限流/鉴权门 → 路由分发 → 有界发送(PRD 8/9)。

use super::lifecycle::ConnectionGuard;
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
use tokio::sync::{mpsc, watch};

pub(crate) struct Admission {
    pub deadline: tokio::time::Instant,
    permit: Option<crate::tcp::auth_gate::AuthPermit>,
    shutdown: watch::Receiver<bool>,
}

impl Admission {
    pub fn acquire(
        deps: &GatewayDeps,
        peer: SocketAddr,
        timeout: std::time::Duration,
        shutdown: watch::Receiver<bool>,
    ) -> Option<Self> {
        let Some(permit) = deps.auth_gate.permit(peer.ip()) else {
            metrics::counter!("longshipx_unauth_rejected_total").increment(1);
            tracing::debug!(%peer, "未鉴权连接超过单 IP 上限,拒绝");
            return None;
        };
        Some(Self {
            deadline: tokio::time::Instant::now() + timeout,
            permit: Some(permit),
            shutdown,
        })
    }
}

/// 连接处理入口(每连接一个 task,panic 只影响本连接,PRD 9.3)。
pub async fn handle_connection(
    stream: TlsTcpStream,
    peer: SocketAddr,
    deps: Arc<GatewayDeps>,
    config: TcpGatewayConfig,
) {
    let (_shutdown_tx, shutdown) = watch::channel(false);
    let Some(admission) = Admission::acquire(&deps, peer, config.unauth_timeout, shutdown) else {
        return;
    };
    handle_admitted_connection(stream, peer, deps, config, admission).await;
}

pub(crate) async fn handle_admitted_connection(
    stream: TlsTcpStream,
    peer: SocketAddr,
    deps: Arc<GatewayDeps>,
    config: TcpGatewayConfig,
    mut admission: Admission,
) {
    let conn_id = uuid::Uuid::now_v7();

    metrics::gauge!("longshipx_connections_active").increment(1.0);
    let (mut reader, outbound, mut writer) = split_connection(
        stream,
        ConnectionConfig {
            write_drain_timeout: config.write_drain_timeout,
            max_frame_size: config.max_frame_size,
            send_queue_capacity: config.send_queue_capacity,
        },
    );
    deps.connections.register(conn_id, outbound.clone());

    // 房间事件翻译 task:RoomEvent → 协议帧 → 发送队列。
    let (room_tx, room_rx) = mpsc::channel::<RoomEvent>(config.room_event_capacity);
    let ctx = ConnContext {
        conn_id,
        peer_ip: peer.ip(),
        outbound: outbound.clone(),
        room_tx: room_tx.into(),
        auth: Arc::new(parking_lot::Mutex::new(Default::default())),
        deps: deps.clone(),
    };
    let mut translator = tokio::spawn(room_event_translator(
        room_rx,
        ctx.clone(),
        config.heartbeat_timeout,
    ));
    let mut session = Session::new(chrono::Utc::now());

    let mut guard = ConnectionGuard::new(
        ctx.clone(),
        writer.abort_handle(),
        translator.abort_handle(),
        config.shutdown_timeout,
    );
    let translator_finished = tokio::select! {
        _ = run_read_loop(&ctx, &mut reader, &mut session, &config, &mut admission) => false,
        _ = &mut translator => true,
    };

    // ── 清理:离房 → 释放限流名额 → 摘除注册表 → 关闭发送队列 ──
    guard.cleanup().await;
    deps.connections.begin_drain(conn_id);
    if !translator_finished {
        translator.abort();
        let _ = translator.await;
    }
    drop(admission);
    // Drain only up to the configured deadline; a stalled peer cannot hold a connection slot.
    drop(ctx);
    drop(outbound);
    if tokio::time::timeout(config.write_drain_timeout, &mut writer)
        .await
        .is_err()
    {
        writer.abort();
        let _ = writer.await;
    }
    drop(guard);
    tracing::debug!(conn = %conn_id, %peer, "连接结束");
}

/// 读循环:超时/限流/解码/鉴权门/分发。返回即连接结束。
async fn run_read_loop(
    ctx: &ConnContext,
    reader: &mut FrameReader<TlsTcpStream>,
    session: &mut Session,
    config: &TcpGatewayConfig,
    admission: &mut Admission,
) {
    let mut bucket = TokenBucket::new(config.rate);
    loop {
        // 鉴权状态以 ctx 为准(BIND 处理器写入 ctx.auth)。
        let deadline = if ctx.is_authenticated() {
            tokio::time::Instant::now() + config.heartbeat_timeout
        } else {
            admission.deadline
        };
        tokio::select! {
            biased;
            _ = ctx.outbound.disconnected() => return,
            _ = ctx.room_tx.failed() => { ctx.outbound.disconnect(); return; },
            _ = async { let _ = admission.shutdown.wait_for(|stop| *stop).await; } => {
                let _ = ctx.try_send(OutboundMessage::Shutdown(longshipx_protocol::generated::ServerShutdownNotice {
                    message: "服务器即将维护,请保存进度并重连".into(),
                }));
                return;
            },
            frame = reader.read_frame() => match frame {
                Ok(Some(frame)) => {
                    session.touch_heartbeat(chrono::Utc::now());
                    if tokio::time::Instant::now() >= deadline {
                        let _ = ctx.try_send(error_notification(ERR_TIMEOUT, "会话超时"));
                        return;
                    }
                    let authenticated = ctx.is_authenticated();
                    let dispatch = admit_frame(ctx, bucket.as_mut(), frame, config.heartbeat_timeout);
                    let outcome = if authenticated { Ok(dispatch.await) } else { tokio::time::timeout_at(deadline, dispatch).await };
                    match outcome {
                        Ok(true) => { if ctx.is_authenticated() { admission.permit.take(); } },
                        Ok(false) => return,
                        Err(_) => {
                            let _ = ctx.try_send(error_notification(ERR_TIMEOUT, "会话超时"));
                            return;
                        },
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
    validation_timeout: std::time::Duration,
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
    if !ctx.is_authenticated() && message.opcode() != OP_C2S_BIND {
        let _ = ctx.try_send(error_notification(
            ERR_AUTH_REQUIRED_FIRST,
            "必须先完成绑定",
        ));
        return false;
    }

    if ctx.is_authenticated() && !ctx.validate_session(validation_timeout).await {
        let _ = ctx.try_send(error_notification(
            ERR_NOT_AUTHENTICATED,
            "会话无效或已过期",
        ));
        return false;
    }

    if ctx.is_authenticated() && message.opcode() == OP_C2S_BIND {
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
    ctx: ConnContext,
    validation_timeout: std::time::Duration,
) {
    while let Some(event) = rx.recv().await {
        if !ctx.validate_session(validation_timeout).await {
            let _ = ctx.try_send(error_notification(
                ERR_NOT_AUTHENTICATED,
                "会话无效或已过期",
            ));
            return;
        }
        let message = OutboundMessage::RoomEvent(room_event_to_notification(event));
        let Ok(frame) = ctx.deps.codec.encode(&message) else {
            tracing::error!("房间事件编码失败");
            ctx.outbound.disconnect();
            return;
        };
        if ctx.outbound.try_send_frame(frame).is_err() {
            tracing::debug!("发送队列不可用,房间事件翻译退出");
            return;
        }
    }
}
