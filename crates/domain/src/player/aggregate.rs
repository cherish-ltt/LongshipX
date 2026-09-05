//! 玩家聚合根:档案与成长数值(服务端权威,PRD 第 10 章)。

use crate::account::AccountId;
use crate::shared::value::Nickname;
use chrono::{DateTime, Utc};
use std::fmt;

/// 玩家标识(UUIDv7)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct PlayerId(pub uuid::Uuid);

impl fmt::Display for PlayerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// 等级上限:防止极端经验值驱动的升级循环不可终止。
pub const MAX_LEVEL: u32 = 999;

/// 升级到下一级所需经验 = 当前等级 * EXP_PER_LEVEL(示例线性曲线,按玩法扩展)。
pub const EXP_PER_LEVEL: u64 = 100;

/// 玩家聚合根。⚠️ 货币/背包/装备等应拆成独立聚合,避免"上帝对象"(PRD 5.1)。
#[derive(Debug, Clone)]
pub struct Player {
    id: PlayerId,
    account_id: AccountId,
    nickname: Nickname,
    level: u32,
    exp: u64,
    last_login_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
}

impl Player {
    /// 新建玩家:等级 1、经验 0。
    pub fn create(account_id: AccountId, nickname: Nickname, now: DateTime<Utc>) -> Self {
        Self {
            id: PlayerId(uuid::Uuid::now_v7()),
            account_id,
            nickname,
            level: 1,
            exp: 0,
            last_login_at: None,
            created_at: now,
        }
    }

    /// 由仓储从持久化数据重建聚合。
    #[allow(clippy::too_many_arguments)]
    pub fn reconstitute(
        id: PlayerId,
        account_id: AccountId,
        nickname: Nickname,
        level: u32,
        exp: u64,
        last_login_at: Option<DateTime<Utc>>,
        created_at: DateTime<Utc>,
    ) -> Self {
        Self {
            id,
            account_id,
            nickname,
            level,
            exp,
            last_login_at,
            created_at,
        }
    }

    pub fn id(&self) -> PlayerId {
        self.id
    }

    pub fn account_id(&self) -> AccountId {
        self.account_id
    }

    pub fn nickname(&self) -> &Nickname {
        &self.nickname
    }

    pub fn level(&self) -> u32 {
        self.level
    }

    pub fn exp(&self) -> u64 {
        self.exp
    }

    pub fn last_login_at(&self) -> Option<DateTime<Utc>> {
        self.last_login_at
    }

    pub fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }

    /// 增加经验并结算升级;发生升级时返回升级后的新等级(服务端权威计算)。
    pub fn gain_exp(&mut self, amount: u64) -> Option<u32> {
        self.exp = self.exp.saturating_add(amount);
        let mut new_level = None;
        while self.level < MAX_LEVEL && self.exp >= Self::exp_to_next(self.level) {
            self.exp -= Self::exp_to_next(self.level);
            self.level = self.level.saturating_add(1);
            new_level = Some(self.level);
        }
        new_level
    }

    fn exp_to_next(level: u32) -> u64 {
        EXP_PER_LEVEL.saturating_mul(u64::from(level))
    }

    /// 记录一次成功登录。
    pub fn record_login(&mut self, at: DateTime<Utc>) {
        self.last_login_at = Some(at);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::value::Nickname;

    fn fixture() -> Player {
        let account = AccountId(uuid::Uuid::now_v7());
        let nick = Nickname::try_new("勇者").unwrap();
        Player::create(account, nick, Utc::now())
    }

    #[test]
    fn create_starts_at_level_one() {
        let player = fixture();
        assert_eq!(player.level(), 1);
        assert_eq!(player.exp(), 0);
        assert_eq!(player.last_login_at(), None);
        assert!(player.id().0.get_version_num() == 7);
        assert_eq!(player.account_id(), player.account_id());
    }

    #[test]
    fn gain_exp_levels_up_exactly_at_threshold() {
        let mut player = fixture();
        // 等级 1 → 2 需要 100 经验,99 不升、100 升。
        assert_eq!(player.gain_exp(99), None);
        assert_eq!(player.level(), 1);
        assert_eq!(player.exp(), 99);
        assert_eq!(player.gain_exp(1), Some(2));
        assert_eq!(player.level(), 2);
        assert_eq!(player.exp(), 0);
    }

    #[test]
    fn gain_exp_can_skip_multiple_levels() {
        let mut player = fixture();
        // 100 + 200 + 300 = 600 → 连升 3 级到 4。
        assert_eq!(player.gain_exp(600), Some(4));
        assert_eq!(player.level(), 4);
        assert_eq!(player.exp(), 0);
    }

    #[test]
    fn gain_exp_saturates_at_max_level() {
        let mut player = fixture();
        assert_eq!(player.gain_exp(u64::MAX), Some(MAX_LEVEL));
        assert_eq!(player.level(), MAX_LEVEL);
        // 满级后经验持续累积但不再升级。
        assert_eq!(player.gain_exp(u64::MAX), None);
        assert_eq!(player.level(), MAX_LEVEL);
    }

    #[test]
    fn record_login_updates_last_login() {
        let mut player = fixture();
        let at = Utc::now();
        player.record_login(at);
        assert_eq!(player.last_login_at(), Some(at));
    }
}
