//! argon2id 密码哈希(PRD 第 10 章 🔴):参数经配置注入,实现 application 的端口。

use argon2::password_hash::phc::PasswordHash as PhcHash;
use argon2::password_hash::{PasswordHasher as _, PasswordVerifier as _};
use argon2::{Algorithm, Argon2, Params, Version};
use longshipx_application::error::AppError;
use longshipx_application::ports::PasswordHasher;
use std::sync::Arc;
use tokio::sync::Semaphore;

/// argon2id 哈希器:实例持有固定参数(哈希串自带参数,verify 按串内参数重算)。
pub struct Argon2PasswordHasher {
    params: Params,
    permits: Arc<Semaphore>,
}

impl Argon2PasswordHasher {
    /// APP_PASSWORD_* parameters and the shared hash/verify concurrency bound.
    pub fn new(
        memory_kb: u32,
        iterations: u32,
        parallelism: u32,
        max_concurrency: usize,
    ) -> Result<Self, AppError> {
        if max_concurrency == 0 || max_concurrency > Semaphore::MAX_PERMITS {
            return Err(AppError::Internal("invalid password concurrency".into()));
        }
        let params = Params::new(memory_kb, iterations, parallelism, None)
            .map_err(|err| AppError::Internal(format!("argon2 参数非法: {err}")))?;
        Ok(Self {
            params,
            permits: Arc::new(Semaphore::new(max_concurrency)),
        })
    }

    async fn run<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> Result<T, AppError> + Send + 'static,
    ) -> Result<T, AppError> {
        let permit = self
            .permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| AppError::Busy)?;
        tokio::task::spawn_blocking(move || {
            // Cancellation of the request must not release capacity while CPU work still runs.
            let _permit = permit;
            work()
        })
        .await
        .map_err(|_| AppError::Internal("password worker failed".into()))?
    }
}

#[async_trait::async_trait]
impl PasswordHasher for Argon2PasswordHasher {
    async fn hash(&self, password: &str) -> Result<String, AppError> {
        let params = self.params.clone();
        let password = password.to_owned();
        self.run(move || {
            // password-hash 0.6:自动生成随机盐。
            Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
                .hash_password(password.as_bytes())
                .map(|phc| phc.to_string())
                .map_err(|err| AppError::Internal(format!("密码哈希失败: {err}")))
        })
        .await
    }

    async fn verify(&self, password: &str, hash: &str) -> Result<bool, AppError> {
        let password = password.to_owned();
        let hash = hash.to_owned();
        self.run(move || {
            let parsed = PhcHash::new(&hash)
                .map_err(|err| AppError::Internal(format!("哈希串解析失败: {err}")))?;
            // argon2 使用哈希串内嵌的参数进行重算,与存储时配置一致。
            match Argon2::default().verify_password(password.as_bytes(), &parsed) {
                Ok(()) => Ok(true),
                Err(argon2::password_hash::Error::PasswordInvalid) => Ok(false),
                Err(err) => Err(AppError::Internal(format!("密码校验异常: {err}"))),
            }
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use longshipx_application::ports::PasswordHasher;

    /// 测试用低开销参数(默认 19MB 内存在单测中过慢)。
    fn hasher() -> Argon2PasswordHasher {
        Argon2PasswordHasher::new(8192, 2, 1, 2).unwrap()
    }

    #[test]
    fn rejects_invalid_params() {
        assert!(Argon2PasswordHasher::new(1, 0, 0, 2).is_err());
        assert!(Argon2PasswordHasher::new(8192, 2, 1, 0).is_err());
    }

    #[tokio::test]
    async fn hash_produces_argon2id_phc_string() {
        let hasher = hasher();
        let hash = hasher.hash("super-secret").await.unwrap();
        assert!(
            hash.starts_with("$argon2id$"),
            "应为 argon2id PHC 格式: {hash}"
        );
        assert!(hash.contains("m=8192"));
        assert!(hash.contains("t=2"));
        assert!(hash.contains("p=1"));
        // 同一密码两次哈希产生不同盐值。
        let other = hasher.hash("super-secret").await.unwrap();
        assert_ne!(hash, other);
    }

    #[tokio::test]
    async fn verify_accepts_correct_and_rejects_wrong_password() {
        let hasher = hasher();
        let hash = hasher.hash("super-secret").await.unwrap();
        assert!(hasher.verify("super-secret", &hash).await.unwrap());
        assert!(!hasher.verify("wrong-password", &hash).await.unwrap());
    }

    #[tokio::test]
    async fn verify_across_instances_uses_embedded_params() {
        // 存储时的参数低于当前实例参数,verify 仍按串内参数重算成功。
        let legacy = Argon2PasswordHasher::new(8192, 2, 1, 2).unwrap();
        let hash = legacy.hash("super-secret").await.unwrap();
        let stricter = Argon2PasswordHasher::new(16384, 3, 2, 2).unwrap();
        assert!(stricter.verify("super-secret", &hash).await.unwrap());
    }

    #[tokio::test]
    async fn malformed_hash_is_internal_error() {
        let hasher = hasher();
        let err = hasher.verify("pw", "not-a-phc-string").await.unwrap_err();
        assert!(matches!(err, AppError::Internal(_)));
    }
    #[tokio::test]
    async fn cancelled_request_keeps_worker_permit_until_completion() {
        let hasher = Arc::new(Argon2PasswordHasher::new(8192, 2, 1, 1).unwrap());
        let worker_hasher = hasher.clone();
        let (started, ready) = tokio::sync::oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let request = tokio::spawn(async move {
            worker_hasher
                .run(move || {
                    started.send(()).unwrap();
                    wait.recv().unwrap();
                    Ok(())
                })
                .await
        });
        ready.await.unwrap();
        assert_eq!(hasher.hash("pw").await, Err(AppError::Busy));
        request.abort();
        assert!(request.await.unwrap_err().is_cancelled());
        assert_eq!(hasher.verify("pw", "hash").await, Err(AppError::Busy));
        release.send(()).unwrap();
        let permit =
            tokio::time::timeout(std::time::Duration::from_secs(2), hasher.permits.acquire())
                .await
                .unwrap()
                .unwrap();
        drop(permit);
        assert!(hasher.hash("pw").await.is_ok());
    }

    #[tokio::test]
    async fn worker_error_and_panic_release_capacity() {
        let hasher = Argon2PasswordHasher::new(8192, 2, 1, 1).unwrap();
        let failure: Result<(), _> = hasher
            .run(|| Err(AppError::Internal("test failure".into())))
            .await;
        assert_eq!(failure, Err(AppError::Internal("test failure".into())));
        let panic: Result<(), _> = hasher.run(|| panic!("test worker panic")).await;
        assert!(matches!(panic, Err(AppError::Internal(_))));
        assert!(hasher.hash("pw").await.is_ok());
    }
}
