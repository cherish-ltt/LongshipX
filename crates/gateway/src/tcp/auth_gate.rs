//! 同一 IP 的未鉴权连接数上限(PRD 8.3 🔴):防"大量建连不发数据"的资源耗尽攻击。

use parking_lot::Mutex;
use std::collections::HashMap;
use std::net::IpAddr;

pub struct AuthGate {
    max_per_ip: usize,
    counts: Mutex<HashMap<IpAddr, usize>>,
}

impl AuthGate {
    pub fn new(max_per_ip: usize) -> Self {
        Self {
            max_per_ip,
            counts: Mutex::new(HashMap::new()),
        }
    }

    /// 尝试为该 IP 占用一个未鉴权名额;超限返回 false。
    pub fn acquire(&self, ip: IpAddr) -> bool {
        let mut counts = self.counts.lock();
        let count = counts.entry(ip).or_insert(0);
        if *count >= self.max_per_ip {
            return false;
        }
        *count += 1;
        true
    }

    /// 释放一个未鉴权名额(鉴权成功或连接断开时调用)。
    pub fn release(&self, ip: IpAddr) {
        let mut counts = self.counts.lock();
        if let Some(count) = counts.get_mut(&ip) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                counts.remove(&ip);
            }
        }
    }

    pub fn unauth_count(&self, ip: IpAddr) -> usize {
        self.counts.lock().get(&ip).copied().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    fn ip(octets: [u8; 4]) -> IpAddr {
        IpAddr::V4(Ipv4Addr::from(octets))
    }

    #[test]
    fn enforces_per_ip_limit_and_releases_slots() {
        let gate = AuthGate::new(2);
        let a = ip([10, 0, 0, 1]);
        assert!(gate.acquire(a));
        assert!(gate.acquire(a));
        assert!(!gate.acquire(a), "第 3 条未鉴权连接应被拒绝");
        assert_eq!(gate.unauth_count(a), 2);

        gate.release(a);
        assert!(gate.acquire(a));
        gate.release(a);
        gate.release(a);
        assert_eq!(gate.unauth_count(a), 0);

        // 其他 IP 不受影响。
        assert!(gate.acquire(ip([10, 0, 0, 2])));
    }

    #[test]
    fn release_without_acquire_is_noop() {
        let gate = AuthGate::new(1);
        let a = ip([10, 0, 0, 3]);
        gate.release(a);
        assert_eq!(gate.unauth_count(a), 0);
        assert!(gate.acquire(a));
    }
}
