//! 领域错误与仓储错误定义。

use crate::room::RoomId;

/// 领域规则错误:由聚合的模型校验与状态机产生。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DomainError {
    #[error("{field} 不合法: {reason}")]
    InvalidValue { field: &'static str, reason: String },
    #[error("房间 {0} 已满")]
    RoomFull(RoomId),
    #[error("房间 {0} 已关闭")]
    RoomClosed(RoomId),
    #[error("玩家 {player_id} 不在房间 {room_id} 中")]
    NotMember {
        player_id: uuid::Uuid,
        room_id: RoomId,
    },
    #[error("房间状态机不允许从 {from} 迁移到 {to}")]
    IllegalTransition { from: String, to: String },
}

/// 仓储接口错误:由 infrastructure 的实现返回。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RepoError {
    #[error("存储故障: {0}")]
    Storage(String),
    #[error("数据冲突: {0}")]
    Conflict(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_error_messages_are_human_readable() {
        let id = uuid::Uuid::now_v7();
        let full = DomainError::RoomFull(crate::room::RoomId(id));
        assert_eq!(full.to_string(), format!("房间 {id} 已满"));
        assert_eq!(
            RepoError::Storage("db down".into()).to_string(),
            "存储故障: db down"
        );
    }
}
