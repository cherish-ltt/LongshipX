//! 房间聚合根:成员与状态机。房间内的实时状态由 Room Actor 串行持有(无锁,PRD 9.1),
//! 本聚合只承载可校验的领域规则,Actor 在每个命令到达时驱动它变更。

use crate::player::PlayerId;
use crate::shared::error::DomainError;
use chrono::{DateTime, Utc};
use std::fmt;

/// 房间标识(UUIDv7)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct RoomId(pub uuid::Uuid);

impl fmt::Display for RoomId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// 房间生命周期状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum RoomState {
    Waiting,
    InProgress,
    Settling,
    Closed,
}

impl fmt::Display for RoomState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Waiting => "Waiting",
            Self::InProgress => "InProgress",
            Self::Settling => "Settling",
            Self::Closed => "Closed",
        };
        f.write_str(name)
    }
}

/// 房间人数硬上限,防止配置错误导致资源放大(可被 APP_MAX_PLAYERS_PER_ROOM 覆盖)。
pub const MAX_ROOM_PLAYERS: u32 = 1024;

/// 房间聚合根。
#[derive(Debug, Clone)]
pub struct Room {
    id: RoomId,
    members: Vec<PlayerId>,
    max_players: u32,
    state: RoomState,
    created_at: DateTime<Utc>,
}

impl Room {
    /// 开启新房间,人数上限收敛到 1..=MAX_ROOM_PLAYERS。
    pub fn open(max_players: u32, now: DateTime<Utc>) -> Self {
        Self {
            id: RoomId(uuid::Uuid::now_v7()),
            members: Vec::new(),
            max_players: max_players.clamp(1, MAX_ROOM_PLAYERS),
            state: RoomState::Waiting,
            created_at: now,
        }
    }

    /// 由仓储/Actor 从既有数据重建。
    pub fn reconstitute(
        id: RoomId,
        members: Vec<PlayerId>,
        max_players: u32,
        state: RoomState,
        created_at: DateTime<Utc>,
    ) -> Self {
        Self {
            id,
            members,
            max_players,
            state,
            created_at,
        }
    }

    pub fn id(&self) -> RoomId {
        self.id
    }

    pub fn members(&self) -> &[PlayerId] {
        &self.members
    }

    pub fn max_players(&self) -> u32 {
        self.max_players
    }

    pub fn state(&self) -> RoomState {
        self.state
    }

    pub fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }

    pub fn member_count(&self) -> usize {
        self.members.len()
    }

    pub fn is_full(&self) -> bool {
        self.member_count() >= self.max_players as usize
    }

    pub fn is_closed(&self) -> bool {
        self.state == RoomState::Closed
    }

    /// 加入房间:返回 Ok(true) 表示新加入;Ok(false) 表示已是成员(幂等)。
    pub fn try_join(&mut self, player: PlayerId) -> Result<bool, DomainError> {
        if self.is_closed() {
            return Err(DomainError::RoomClosed(self.id));
        }
        if self.members.contains(&player) {
            return Ok(false);
        }
        if self.is_full() {
            return Err(DomainError::RoomFull(self.id));
        }
        self.members.push(player);
        Ok(true)
    }

    /// 离开房间:返回 Ok(true) 表示已移除;Ok(false) 表示本就不是成员。
    pub fn try_leave(&mut self, player: PlayerId) -> Result<bool, DomainError> {
        if self.is_closed() {
            return Err(DomainError::RoomClosed(self.id));
        }
        let before = self.members.len();
        self.members.retain(|member| *member != player);
        Ok(self.members.len() != before)
    }

    /// 状态迁移:Waiting → InProgress → Settling → Closed,不允许跳迁或回退。
    pub fn transition_to(&mut self, target: RoomState) -> Result<(), DomainError> {
        let allowed = matches!(
            (self.state, target),
            (RoomState::Waiting, RoomState::InProgress)
                | (RoomState::InProgress, RoomState::Settling)
                | (RoomState::Settling, RoomState::Closed)
        );
        if !allowed {
            return Err(DomainError::IllegalTransition {
                from: self.state.to_string(),
                to: target.to_string(),
            });
        }
        self.state = target;
        Ok(())
    }

    /// 关闭房间(终态);重复关闭返回 false。
    pub fn close(&mut self) -> bool {
        if self.is_closed() {
            return false;
        }
        self.state = RoomState::Closed;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn player() -> PlayerId {
        PlayerId(uuid::Uuid::now_v7())
    }

    fn room(max: u32) -> Room {
        Room::open(max, Utc::now())
    }

    #[test]
    fn open_clamps_capacity_and_starts_waiting() {
        let small = room(0);
        assert_eq!(small.max_players(), 1);
        assert_eq!(small.state(), RoomState::Waiting);
        assert!(room(10_000).max_players() <= MAX_ROOM_PLAYERS);
        assert!(small.id().0.get_version_num() == 7);
    }

    #[test]
    fn join_is_idempotent_and_enforces_capacity() {
        let mut r = room(2);
        let p1 = player();
        let p2 = player();
        let p3 = player();
        assert!(r.try_join(p1).unwrap());
        assert!(!r.try_join(p1).unwrap());
        assert!(r.try_join(p2).unwrap());
        assert!(matches!(r.try_join(p3), Err(DomainError::RoomFull(_))));
        assert_eq!(r.member_count(), 2);
        assert!(r.is_full());
    }

    #[test]
    fn leave_only_removes_members() {
        let mut r = room(2);
        let p1 = player();
        let outsider = player();
        r.try_join(p1).unwrap();
        assert!(r.try_leave(p1).unwrap());
        assert!(!r.try_leave(p1).unwrap());
        assert!(!r.try_leave(outsider).unwrap());
        assert_eq!(r.member_count(), 0);
    }

    #[test]
    fn closed_room_rejects_join_and_leave() {
        let mut r = room(2);
        let p = player();
        r.close();
        assert!(matches!(r.try_join(p), Err(DomainError::RoomClosed(_))));
        assert!(matches!(r.try_leave(p), Err(DomainError::RoomClosed(_))));
        assert!(!r.close());
    }

    #[test]
    fn state_machine_allows_only_forward_transitions() {
        let mut r = room(2);
        assert!(matches!(
            r.transition_to(RoomState::Settling),
            Err(DomainError::IllegalTransition { from, to }) if from == "Waiting" && to == "Settling"
        ));
        r.transition_to(RoomState::InProgress).unwrap();
        r.transition_to(RoomState::Settling).unwrap();
        r.transition_to(RoomState::Closed).unwrap();
        assert_eq!(r.state(), RoomState::Closed);
        assert!(matches!(
            r.transition_to(RoomState::Waiting),
            Err(DomainError::IllegalTransition { .. })
        ));
    }
}
