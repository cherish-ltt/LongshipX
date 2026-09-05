//! 值对象:带模型校验的领域概念。
//!
//! 🔴 密码与其哈希的 `Debug`/`Display` 一律脱敏,防止日志泄漏(PRD 第 10 章)。

use crate::shared::error::DomainError;
use std::fmt;

/// 用户名:3~32 字符,仅限字母/数字/下划线;统一小写存储(数据库层另有 CITEXT 双保险)。
#[derive(Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Username(String);

impl Username {
    pub fn try_new(raw: &str) -> Result<Self, DomainError> {
        let trimmed = raw.trim();
        if !(3..=32).contains(&trimmed.len()) {
            return Err(DomainError::InvalidValue {
                field: "username",
                reason: "长度必须在 3~32 之间".into(),
            });
        }
        if !trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            return Err(DomainError::InvalidValue {
                field: "username",
                reason: "只能包含字母、数字与下划线".into(),
            });
        }
        Ok(Self(trimmed.to_ascii_lowercase()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_inner(self) -> String {
        self.0
    }
}

impl fmt::Debug for Username {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Username({})", self.0)
    }
}

/// 昵称:1~32 字符(按 Unicode 字符计数),不允许控制字符。
#[derive(Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Nickname(String);

impl Nickname {
    pub fn try_new(raw: &str) -> Result<Self, DomainError> {
        let trimmed = raw.trim();
        let count = trimmed.chars().count();
        if count == 0 || count > 32 {
            return Err(DomainError::InvalidValue {
                field: "nickname",
                reason: "长度必须在 1~32 个字符之间".into(),
            });
        }
        if trimmed.chars().any(char::is_control) {
            return Err(DomainError::InvalidValue {
                field: "nickname",
                reason: "不允许包含控制字符".into(),
            });
        }
        Ok(Self(trimmed.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Nickname {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Nickname({})", self.0)
    }
}

/// 明文密码:8~128 字节,不允许控制字符;仅在哈希前短暂存在,严禁落库/落日志。
pub struct PlainPassword(String);

impl PlainPassword {
    pub fn try_new(raw: &str) -> Result<Self, DomainError> {
        let len = raw.len();
        if !(8..=128).contains(&len) {
            return Err(DomainError::InvalidValue {
                field: "password",
                reason: "长度必须在 8~128 字节之间".into(),
            });
        }
        if raw.chars().any(char::is_control) {
            return Err(DomainError::InvalidValue {
                field: "password",
                reason: "不允许包含控制字符".into(),
            });
        }
        Ok(Self(raw.to_string()))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for PlainPassword {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PlainPassword(***)")
    }
}

impl fmt::Display for PlainPassword {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***")
    }
}

/// argon2id PHC 格式哈希串(仅由 infrastructure 的 PasswordHasher 实现产生)。
#[derive(Clone)]
pub struct PasswordHash(String);

impl PasswordHash {
    pub fn new(hash: String) -> Self {
        Self(hash)
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for PasswordHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PasswordHash(***)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invalid(field: &str, reason: &str) -> DomainError {
        DomainError::InvalidValue {
            field: "x",
            reason: format!("{field}{reason}"),
        }
    }

    #[test]
    fn username_accepts_valid_and_normalizes_case() {
        let name = Username::try_new("  Tom_01 ").unwrap();
        assert_eq!(name.as_str(), "tom_01");
    }

    #[test]
    fn username_rejects_bad_length_and_charset() {
        assert!(Username::try_new("ab").is_err());
        assert!(Username::try_new(&"a".repeat(33)).is_err());
        assert!(Username::try_new("bad name!").is_err());
        assert!(matches!(
            Username::try_new("bad name!"),
            Err(DomainError::InvalidValue {
                field: "username",
                ..
            })
        ));
        let _ = invalid("a", "b");
    }

    #[test]
    fn nickname_counts_unicode_chars_and_rejects_control() {
        assert!(Nickname::try_new("玩家一号").is_ok());
        assert!(Nickname::try_new("  ").is_err());
        assert!(Nickname::try_new(&"长".repeat(33)).is_err());
        assert!(Nickname::try_new("bad\nname").is_err());
    }

    #[test]
    fn password_length_boundary_and_masking() {
        assert!(PlainPassword::try_new(&"p".repeat(8)).is_ok());
        assert!(PlainPassword::try_new(&"p".repeat(7)).is_err());
        assert!(PlainPassword::try_new(&"p".repeat(129)).is_err());
        assert!(PlainPassword::try_new("pass\rword").is_err());
        let pwd = PlainPassword::try_new("secret-pw").unwrap();
        assert_eq!(format!("{pwd:?}"), "PlainPassword(***)");
        assert_eq!(pwd.to_string(), "***");
        assert_eq!(pwd.expose(), "secret-pw");
    }

    #[test]
    fn password_hash_is_masked_in_logs() {
        let hash = PasswordHash::new("$argon2id$v=19$secret".into());
        assert_eq!(format!("{hash:?}"), "PasswordHash(***)");
        assert!(hash.expose().starts_with("$argon2id"));
    }
}
