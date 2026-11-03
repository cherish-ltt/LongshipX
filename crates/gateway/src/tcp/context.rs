//! 单连接处理上下文:路由处理器能拿到的全部能力(应用服务 + 发送端 + 鉴权状态)。

use crate::tcp::auth_gate::AuthGate;
use crate::tcp::connections::ConnectionRegistry;
use longshipx_application::ports::SessionTokenStore;
use longshipx_application::{GetPlayerProfile, RoomService};
use longshipx_domain::{PlayerId, PlayerRepository};
use longshipx_net_kit::OutboundSender;
use longshipx_protocol::Router;
use parking_lot::Mutex;
use std::net::IpAddr;
use std::sync::Arc;
use uuid::Uuid;

/// 已鉴权玩家信息(绑定成功后写入)。
#[derive(Debug, Clone)]
pub struct AuthedPlayer {
    pub player_id: PlayerId,
    pub nickname: String,
}

#[derive(Clone, Default)]
pub struct AuthState {
    pub player: Option<AuthedPlayer>,
    session_token: Option<String>,
    /// 本连接当前所在房间(一条连接同时只在一个房间,PRD Room/Scene 语义)。
    pub current_room: Option<longshipx_domain::RoomId>,
}

/// 网关共享依赖(全连接复用)。
pub struct GatewayDeps {
    pub codec: Arc<
        dyn longshipx_net_kit::codec::Codec<
                In = longshipx_protocol::InboundMessage,
                Out = longshipx_protocol::OutboundMessage,
                Error = longshipx_protocol::ProtocolError,
            >,
    >,
    pub router: Router<ConnContext>,
    pub tokens: Arc<dyn SessionTokenStore>,
    pub players: Arc<dyn PlayerRepository>,
    pub profile: Arc<GetPlayerProfile>,
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
    pub room_tx: longshipx_application::RoomEventSink,
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

    pub fn set_authed(&self, player: AuthedPlayer, token: String) {
        let mut auth = self.auth.lock();
        auth.player = Some(player);
        auth.session_token = Some(token);
    }

    pub async fn validate_session(&self, timeout: std::time::Duration) -> bool {
        let (player, token) = {
            let auth = self.auth.lock();
            (auth.player.clone(), auth.session_token.clone())
        };
        let (Some(player), Some(token)) = (player, token) else {
            return false;
        };
        match tokio::time::timeout(timeout, self.deps.tokens.resolve(&token)).await {
            Ok(Ok(Some(id))) if id == player.player_id => true,
            Ok(Ok(_)) => false,
            Ok(Err(err)) => {
                tracing::warn!(conn = %self.conn_id, error = %err, "会话校验失败,关闭连接");
                false
            },
            Err(_) => false,
        }
    }

    pub fn current_room(&self) -> Option<longshipx_domain::RoomId> {
        self.auth.lock().current_room
    }

    pub fn set_current_room(&self, room_id: Option<longshipx_domain::RoomId>) {
        self.auth.lock().current_room = room_id;
    }

    /// 尽力投递一条出站消息;队列满返回 Err(调用方应断开,PRD 8.5)。
    pub fn try_send(
        &self,
        message: longshipx_protocol::OutboundMessage,
    ) -> Result<(), longshipx_protocol::ProtocolError> {
        let frame = self.deps.codec.encode(&message)?;
        self.outbound
            .try_send_frame(frame)
            .map_err(|err| longshipx_protocol::ProtocolError::Handler(err.to_string()))
    }
}
