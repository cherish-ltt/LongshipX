//! Model ⇄ 领域聚合的显式转换(PRD 7.4 ⚠️):多写样板,换技术选型不传染 domain。

use crate::persistence::entities::{account as account_entity, player as player_entity};
use chrono::{DateTime, Utc};
use longshipx_domain::shared::value::{Nickname, PasswordHash, Username};
use longshipx_domain::{Account, AccountId, AccountStatus, Player, PlayerId, RepoError};

/// 账号状态 ↔ 状态码(0/1/2,见 migration)。
pub fn status_from_code(code: i16) -> Result<AccountStatus, RepoError> {
    match code {
        0 => Ok(AccountStatus::Active),
        1 => Ok(AccountStatus::Suspended),
        2 => Ok(AccountStatus::Banned {
            reason: String::new(),
            until: None,
        }),
        other => Err(RepoError::Storage(format!("未知的账号状态码 {other}"))),
    }
}

pub fn status_to_parts(status: &AccountStatus) -> (i16, Option<String>, Option<DateTime<Utc>>) {
    match status {
        AccountStatus::Active => (0, None, None),
        AccountStatus::Suspended => (1, None, None),
        AccountStatus::Banned { reason, until } => (2, Some(reason.clone()), *until),
    }
}

fn to_utc(value: DateTime<chrono::FixedOffset>) -> DateTime<Utc> {
    value.with_timezone(&Utc)
}

fn to_fixed(value: DateTime<Utc>) -> DateTime<chrono::FixedOffset> {
    value.into()
}

/// 账号持久化模型 → 领域聚合。
pub fn account_to_domain(model: &account_entity::Model) -> Result<Account, RepoError> {
    let username = Username::try_new(&model.username)
        .map_err(|err| RepoError::Storage(format!("账号数据不符合领域规则: {err}")))?;
    let mut status = status_from_code(model.status)?;
    if let AccountStatus::Banned { reason, until } = &mut status {
        *reason = model.banned_reason.clone().unwrap_or_default();
        *until = model.banned_until.map(to_utc);
    }
    Ok(Account::reconstitute(
        AccountId(model.id),
        username,
        PasswordHash::new(model.password_hash.clone()),
        status,
        to_utc(model.created_at),
    ))
}

/// 领域聚合 → 账号 ActiveModel(insert/update 共用)。
pub fn account_to_active(account: &Account) -> account_entity::ActiveModel {
    use sea_orm::ActiveValue::Set;
    let (status, reason, until) = status_to_parts(account.status());
    account_entity::ActiveModel {
        id: Set(account.id().0),
        username: Set(account.username().as_str().to_string()),
        password_hash: Set(account.password_hash().expose().to_string()),
        status: Set(status),
        banned_reason: Set(reason),
        banned_until: Set(until.map(to_fixed)),
        created_at: Set(to_fixed(account.created_at())),
    }
}

/// 玩家持久化模型 → 领域聚合。
pub fn player_to_domain(model: &player_entity::Model) -> Result<Player, RepoError> {
    let nickname = Nickname::try_new(&model.nickname)
        .map_err(|err| RepoError::Storage(format!("玩家数据不符合领域规则: {err}")))?;
    Ok(Player::reconstitute(
        PlayerId(model.id),
        longshipx_domain::AccountId(model.account_id),
        nickname,
        u32::try_from(model.level.max(0)).unwrap_or(0),
        u64::try_from(model.exp.max(0)).unwrap_or(0),
        model.last_login_at.map(to_utc),
        to_utc(model.created_at),
    ))
}

/// 领域聚合 → 玩家 ActiveModel。
pub fn player_to_active(player: &Player) -> player_entity::ActiveModel {
    use sea_orm::ActiveValue::Set;
    player_entity::ActiveModel {
        id: Set(player.id().0),
        account_id: Set(player.account_id().0),
        nickname: Set(player.nickname().as_str().to_string()),
        level: Set(i32::try_from(player.level()).unwrap_or(i32::MAX)),
        exp: Set(i64::try_from(player.exp()).unwrap_or(i64::MAX)),
        last_login_at: Set(player.last_login_at().map(to_fixed)),
        created_at: Set(to_fixed(player.created_at())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use longshipx_domain::Nickname;

    fn account() -> Account {
        let name = Username::try_new("carol").unwrap();
        Account::register(name, PasswordHash::new("$argon2id$hash".into()), Utc::now())
    }

    #[test]
    fn account_roundtrip_preserves_fields() {
        let original = account();
        let active = account_to_active(&original);
        let model = account_entity::Model {
            id: active.id.clone().unwrap(),
            username: active.username.clone().unwrap(),
            password_hash: active.password_hash.clone().unwrap(),
            status: active.status.clone().unwrap(),
            banned_reason: None,
            banned_until: None,
            created_at: active.created_at.clone().unwrap(),
        };
        let restored = account_to_domain(&model).unwrap();
        assert_eq!(restored.id(), original.id());
        assert_eq!(restored.username().as_str(), "carol");
        assert_eq!(restored.password_hash().expose(), "$argon2id$hash");
        assert_eq!(restored.status(), &AccountStatus::Active);
        assert_eq!(restored.created_at(), original.created_at());
    }

    #[test]
    fn banned_status_roundtrip_with_reason_and_until() {
        let mut original = account();
        let until = Utc::now() + chrono::Duration::days(3);
        original.ban("恶意刷屏".into(), Some(until));
        let active = account_to_active(&original);
        assert_eq!(active.status.clone().unwrap(), 2);
        assert_eq!(
            active.banned_reason.clone().unwrap().unwrap().as_str(),
            "恶意刷屏"
        );

        let model = account_entity::Model {
            banned_reason: active.banned_reason.clone().unwrap(),
            banned_until: active.banned_until.clone().unwrap(),
            status: 2,
            ..test_account_model()
        };
        let restored = account_to_domain(&model).unwrap();
        assert_eq!(
            restored.status(),
            &AccountStatus::Banned {
                reason: "恶意刷屏".into(),
                until: Some(until)
            }
        );
    }

    #[test]
    fn unknown_status_code_is_storage_error() {
        assert!(matches!(status_from_code(9), Err(RepoError::Storage(_))));
    }

    #[test]
    fn player_roundtrip_preserves_fields() {
        let nickname = Nickname::try_new("铁头娃").unwrap();
        let mut player = Player::create(AccountId(uuid::Uuid::now_v7()), nickname, Utc::now());
        player.gain_exp(250);
        player.record_login(Utc::now());

        let active = player_to_active(&player);
        let model = player_entity::Model {
            id: active.id.clone().unwrap(),
            account_id: active.account_id.clone().unwrap(),
            nickname: active.nickname.clone().unwrap(),
            level: active.level.clone().unwrap(),
            exp: active.exp.clone().unwrap(),
            last_login_at: active.last_login_at.clone().unwrap(),
            created_at: active.created_at.clone().unwrap(),
        };
        let restored = player_to_domain(&model).unwrap();
        assert_eq!(restored.id(), player.id());
        assert_eq!(restored.level(), player.level());
        assert_eq!(restored.exp(), player.exp());
        assert_eq!(restored.nickname().as_str(), "铁头娃");
        assert_eq!(restored.last_login_at(), player.last_login_at());
    }

    #[test]
    fn player_clamps_negative_columns() {
        let model = player_entity::Model {
            level: -5,
            exp: -100,
            ..test_player_model()
        };
        let restored = player_to_domain(&model).unwrap();
        assert_eq!(restored.level(), 0);
        assert_eq!(restored.exp(), 0);
    }

    fn test_account_model() -> account_entity::Model {
        account_entity::Model {
            id: uuid::Uuid::now_v7(),
            username: "carol".into(),
            password_hash: "hash".into(),
            status: 0,
            banned_reason: None,
            banned_until: None,
            created_at: Utc::now().into(),
        }
    }

    fn test_player_model() -> player_entity::Model {
        player_entity::Model {
            id: uuid::Uuid::now_v7(),
            account_id: uuid::Uuid::now_v7(),
            nickname: "铁头娃".into(),
            level: 1,
            exp: 0,
            last_login_at: None,
            created_at: Utc::now().into(),
        }
    }
}
