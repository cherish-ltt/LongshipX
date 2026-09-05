//! argon2id 密码哈希(PRD 第 10 章 🔴):参数经配置注入,实现 application 的端口。

use argon2::password_hash::phc::PasswordHash as PhcHash;
use argon2::password_hash::{PasswordHasher as _, PasswordVerifier as _};
use argon2::{Algorithm, Argon2, Params, Version};
use longshipx_application::error::AppError;
use longshipx_application::ports::PasswordHasher;

/// argon2id 哈希器:实例持有固定参数(哈希串自带参数,verify 按串内参数重算)。
pub struct Argon2PasswordHasher {
    params: Params,
}

impl Argon2PasswordHasher {
    /// (memory_kb, iterations, parallelism) 来自 APP_PASSWORD_* 配置。
    pub fn new(memory_kb: u32, iterations: u32, parallelism: u32) -> Result<Self, AppError> {
        let params = Params::new(memory_kb, iterations, parallelism, None)
            .map_err(|err| AppError::Internal(format!("argon2 参数非法: {err}")))?;
        Ok(Self { params })
    }

    fn algorithm(&self) -> Argon2<'static> {
        Argon2::new(Algorithm::Argon2id, Version::V0x13, self.params.clone())
    }
}

impl PasswordHasher for Argon2PasswordHasher {
    fn hash(&self, password: &str) -> Result<String, AppError> {
        // password-hash 0.6:自动生成随机盐。
        let phc = self
            .algorithm()
            .hash_password(password.as_bytes())
            .map_err(|err| AppError::Internal(format!("密码哈希失败: {err}")))?;
        Ok(phc.to_string())
    }

    fn verify(&self, password: &str, hash: &str) -> Result<bool, AppError> {
        let parsed = PhcHash::new(hash)
            .map_err(|err| AppError::Internal(format!("哈希串解析失败: {err}")))?;
        // argon2 使用哈希串内嵌的参数进行重算,与存储时配置一致。
        match Argon2::default().verify_password(password.as_bytes(), &parsed) {
            Ok(()) => Ok(true),
            Err(argon2::password_hash::Error::PasswordInvalid) => Ok(false),
            Err(err) => Err(AppError::Internal(format!("密码校验异常: {err}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use longshipx_application::ports::PasswordHasher;

    /// 测试用低开销参数(默认 19MB 内存在单测中过慢)。
    fn hasher() -> Argon2PasswordHasher {
        Argon2PasswordHasher::new(8192, 2, 1).unwrap()
    }

    #[test]
    fn rejects_invalid_params() {
        assert!(Argon2PasswordHasher::new(1, 0, 0).is_err());
    }

    #[test]
    fn hash_produces_argon2id_phc_string() {
        let hasher = hasher();
        let hash = hasher.hash("super-secret").unwrap();
        assert!(
            hash.starts_with("$argon2id$"),
            "应为 argon2id PHC 格式: {hash}"
        );
        assert!(hash.contains("m=8192"));
        assert!(hash.contains("t=2"));
        assert!(hash.contains("p=1"));
        // 同一密码两次哈希产生不同盐值。
        let other = hasher.hash("super-secret").unwrap();
        assert_ne!(hash, other);
    }

    #[test]
    fn verify_accepts_correct_and_rejects_wrong_password() {
        let hasher = hasher();
        let hash = hasher.hash("super-secret").unwrap();
        assert!(hasher.verify("super-secret", &hash).unwrap());
        assert!(!hasher.verify("wrong-password", &hash).unwrap());
    }

    #[test]
    fn verify_across_instances_uses_embedded_params() {
        // 存储时的参数低于当前实例参数,verify 仍按串内参数重算成功。
        let legacy = Argon2PasswordHasher::new(8192, 2, 1).unwrap();
        let hash = legacy.hash("super-secret").unwrap();
        let stricter = Argon2PasswordHasher::new(16384, 3, 2).unwrap();
        assert!(stricter.verify("super-secret", &hash).unwrap());
    }

    #[test]
    fn malformed_hash_is_internal_error() {
        let hasher = hasher();
        let err = hasher.verify("pw", "not-a-phc-string").unwrap_err();
        assert!(matches!(err, AppError::Internal(_)));
    }
}
