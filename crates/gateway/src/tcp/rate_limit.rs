//! 每连接令牌桶限流(PRD 18 SERVER_RATE_LIMIT_*):时间由调用方注入,可确定性测试。

#[derive(Debug)]
pub struct TokenBucket {
    capacity: f64,
    refill_per_sec: f64,
    tokens: f64,
    last_refill: std::time::Instant,
}

impl TokenBucket {
    /// 禁用时返回 None。
    pub fn new(settings: RateLimitSettings) -> Option<Self> {
        if !settings.enabled || settings.per_second == 0 || settings.burst == 0 {
            return None;
        }
        // 容量 = 突发容量;稳态速率 = per_second。
        Some(Self {
            capacity: settings.burst as f64,
            refill_per_sec: settings.per_second as f64,
            tokens: settings.burst as f64,
            last_refill: std::time::Instant::now(),
        })
    }

    /// 取一枚令牌;不足则拒绝(调用方应断开或告警)。
    pub fn try_acquire(&mut self, now: std::time::Instant) -> bool {
        let elapsed = now
            .saturating_duration_since(self.last_refill)
            .as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.refill_per_sec).min(self.capacity);
        self.last_refill = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

use crate::tcp::config::RateLimitSettings;

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn settings() -> RateLimitSettings {
        RateLimitSettings {
            enabled: true,
            per_second: 10,
            burst: 5,
        }
    }

    #[test]
    fn allows_burst_then_rejects_until_refill() {
        let mut bucket = TokenBucket::new(settings()).unwrap();
        let start = Instant::now();
        for _ in 0..5 {
            assert!(bucket.try_acquire(start), "突发容量内应放行");
        }
        assert!(!bucket.try_acquire(start), "突发耗尽后应拒绝");
        // 0.5 秒补充 5 枚。
        let later = start + Duration::from_millis(500);
        assert!(bucket.try_acquire(later));
        assert!(bucket.try_acquire(later));
    }

    #[test]
    fn refill_never_exceeds_capacity() {
        let mut bucket = TokenBucket::new(settings()).unwrap();
        let start = Instant::now();
        let much_later = start + Duration::from_secs(3600);
        for _ in 0..5 {
            assert!(bucket.try_acquire(much_later));
        }
        assert!(!bucket.try_acquire(much_later), "补充不超过突发容量");
    }

    #[test]
    fn disabled_limit_is_none() {
        assert!(
            TokenBucket::new(RateLimitSettings {
                enabled: false,
                per_second: 10,
                burst: 5
            })
            .is_none()
        );
        assert!(
            TokenBucket::new(RateLimitSettings {
                enabled: true,
                per_second: 0,
                burst: 5
            })
            .is_none()
        );
    }
}
