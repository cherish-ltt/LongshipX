//! 用例的输入输出 DTO:网关与用例之间的稳定契约,不暴露领域聚合内部结构。

use chrono::{DateTime, Utc};
use longshipx_domain::{AccountId, Player, PlayerId, RoomId};

/// 注册输入(明文密码仅在此短暂存在,交给 PasswordHasher)。
pub struct RegisterCommand {
    pub username: String,
    pub password: String,
    pub nickname: String,
}

#[derive(Debug, Clone)]
pub struct RegisterResult {
    pub account_id: AccountId,
    pub player_id: PlayerId,
    pub nickname: String,
}

/// 登录输入。
pub struct LoginCommand {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone)]
pub struct LoginResult {
    pub token: String,
    pub player_id: PlayerId,
    pub nickname: String,
    pub expires_in_secs: u64,
}

/// 玩家档案查询输出。
#[derive(Debug, Clone)]
pub struct PlayerProfile {
    pub player_id: PlayerId,
    pub nickname: String,
    pub level: u32,
    pub exp: u64,
    pub last_login_at: Option<DateTime<Utc>>,
}

impl From<&Player> for PlayerProfile {
    fn from(player: &Player) -> Self {
        Self {
            player_id: player.id(),
            nickname: player.nickname().as_str().to_string(),
            level: player.level(),
            exp: player.exp(),
            last_login_at: player.last_login_at(),
        }
    }
}

/// 房间概要(用于建房/加入成功后的回执)。
#[derive(Debug, Clone)]
pub struct RoomSummary {
    pub room_id: RoomId,
    pub member_count: u32,
    pub max_players: u32,
    pub state: String,
}
