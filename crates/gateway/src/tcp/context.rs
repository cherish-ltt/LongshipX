//! 单连接处理上下文:路由处理器能拿到的全部能力(应用服务 + 发送端 + 鉴权状态)。

use crate::tcp::auth_gate::AuthGate;
use crate::tcp::connections::ConnectionRegistry;
use parking_lot::Mutex;
use ppt_tcp_application::RoomService;
use ppt_tcp_application::ports::SessionTokenStore;
use ppt_tcp_domain::{PlayerId, PlayerRepository};
use ppt_tcp_net_kit::OutboundSender;
use ppt_tcp_protocol::Router;
use std::net::IpAddr;
use std::sync::Arc;
use tokio::sync::mpsc;
use uuid::Uuid;

/// 已鉴权玩家信息(绑定成功后写入)。
#[derive(Debug, Clone)]
pub struct AuthedPlayer {
    pub player_id: PlayerId,
    pub nickname: String,
}

#[derive(Debug, Clone, Default)]
pub struct AuthState {
    pub player: Option<AuthedPlayer>,
    /// 本连接当前所在房间(一条连接同时只在一个房间,PRD Room/Scene 语义)。
    pub current_room: Option<ppt_tcp_domain::RoomId>,
}

/// 网关共享依赖(全连接复用)。
pub struct GatewayDeps {
    pub codec: Arc<
        dyn ppt_tcp_net_kit::codec::Codec<
                In = ppt_tcp_protocol::InboundMessage,
                Out = ppt_tcp_protocol::OutboundMessage,
                Error = ppt_tcp_protocol::ProtocolError,
            >,
    >,
    pub router: Router<ConnContext>,
    pub tokens: Arc<dyn SessionTokenStore>,
    pub players: Arc<dyn PlayerRepository>,
    pub rooms: Arc<RoomService>,
    pub auth_gate: Arc<AuthGate>,
    pub connections: Arc<ConnectionRegistry>,
}

/// 单连接上下文(Clone 便宜:全是 Arc/通道句柄)。
#[derive(Clone)]
pub struct ConnContext {
    pub conn_id: Uuid,
    pub peer_ip: IpAddr,
    pub outbound: OutboundSender,
    /// 本连接的房间事件入口(加入房间时作为成员 sink 交给 Actor)。
    pub room_tx: mpsc::Sender<ppt_tcp_application::RoomEvent>,
    pub auth: Arc<Mutex<AuthState>>,
    pub deps: Arc<GatewayDeps>,
}

impl ConnContext {
    pub fn is_authenticated(&self) -> bool {
        self.auth.lock().player.is_some()
    }

    pub fn authed_player(&self) -> Option<AuthedPlayer> {
        self.auth.lock().player.clone()
    }

    pub fn set_authed(&self, player: AuthedPlayer) {
        self.auth.lock().player = Some(player);
    }

    pub fn current_room(&self) -> Option<ppt_tcp_domain::RoomId> {
        self.auth.lock().current_room
    }

    pub fn set_current_room(&self, room_id: Option<ppt_tcp_domain::RoomId>) {
        self.auth.lock().current_room = room_id;
    }

    /// 尽力投递一条出站消息;队列满返回 Err(调用方应断开,PRD 8.5)。
    pub fn try_send(
        &self,
        message: ppt_tcp_protocol::OutboundMessage,
    ) -> Result<(), ppt_tcp_protocol::ProtocolError> {
        let frame = self.deps.codec.encode(&message)?;
        self.outbound
            .try_send_frame(frame)
            .map_err(|err| ppt_tcp_protocol::ProtocolError::Handler(err.to_string()))
    }
}
