//! 在线会话:一条连接的服务端视角状态(未鉴权 → 已绑定玩家)。

use crate::player::PlayerId;
use chrono::{DateTime, Duration, Utc};

/// 会话标识(等于连接 ID,UUIDv7)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct SessionId(pub uuid::Uuid);

/// 会话聚合:由 gateway 每连接持有一份,心跳即更新 `last_heartbeat_at`。
#[derive(Debug, Clone)]
pub struct Session {
    id: SessionId,
    player_id: Option<PlayerId>,
    connected_at: DateTime<Utc>,
    last_heartbeat_at: DateTime<Utc>,
}

impl Session {
    /// 连接建立时创建,处于未鉴权状态。
    pub fn new(now: DateTime<Utc>) -> Self {
        Self {
            id: SessionId(uuid::Uuid::now_v7()),
            player_id: None,
            connected_at: now,
            last_heartbeat_at: now,
        }
    }

    /// 由网关从连接状态重建(用于测试与未来会话恢复)。
    pub fn reconstitute(
        id: SessionId,
        player_id: Option<PlayerId>,
        connected_at: DateTime<Utc>,
        last_heartbeat_at: DateTime<Utc>,
    ) -> Self {
        Self {
            id,
            player_id,
            connected_at,
            last_heartbeat_at,
        }
    }

    pub fn id(&self) -> SessionId {
        self.id
    }

    /// 完成鉴权绑定,None 表示已连接但未鉴权(PRD 5.1)。
    pub fn bind_player(&mut self, player_id: PlayerId, now: DateTime<Utc>) {
        self.player_id = Some(player_id);
        self.last_heartbeat_at = now;
    }

    pub fn player_id(&self) -> Option<PlayerId> {
        self.player_id
    }

    pub fn is_authenticated(&self) -> bool {
        self.player_id.is_some()
    }

    pub fn connected_at(&self) -> DateTime<Utc> {
        self.connected_at
    }

    pub fn last_heartbeat_at(&self) -> DateTime<Utc> {
        self.last_heartbeat_at
    }

    /// 收到任何合法帧(含心跳)都刷新活跃时间。
    pub fn touch_heartbeat(&mut self, now: DateTime<Utc>) {
        self.last_heartbeat_at = now;
    }

    /// 心跳超时判定:超过 `timeout` 未活跃即视为掉线(PRD 8.4)。
    pub fn is_heartbeat_expired(&self, now: DateTime<Utc>, timeout: Duration) -> bool {
        now.signed_duration_since(self.last_heartbeat_at) > timeout
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> (DateTime<Utc>, Session) {
        let now = Utc::now();
        (now, Session::new(now))
    }

    #[test]
    fn new_session_is_unauthenticated() {
        let (now, session) = base();
        assert!(!session.is_authenticated());
        assert_eq!(session.player_id(), None);
        assert_eq!(session.connected_at(), now);
        assert!(session.id().0.get_version_num() == 7);
    }

    #[test]
    fn bind_player_authenticates_and_touches() {
        let (now, mut session) = base();
        let player = PlayerId(uuid::Uuid::now_v7());
        session.bind_player(player, now);
        assert!(session.is_authenticated());
        assert_eq!(session.player_id(), Some(player));
        assert_eq!(session.last_heartbeat_at(), now);
    }

    #[test]
    fn heartbeat_expiry_respects_threshold() {
        let (now, mut session) = base();
        let timeout = Duration::seconds(60);
        assert!(!session.is_heartbeat_expired(now, timeout));
        session.touch_heartbeat(now + Duration::seconds(30));
        assert!(!session.is_heartbeat_expired(now + Duration::seconds(60), timeout));
        assert!(session.is_heartbeat_expired(now + Duration::seconds(91), timeout));
    }
}
