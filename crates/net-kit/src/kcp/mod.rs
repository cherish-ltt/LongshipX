//! Bounded kcp-ovo byte streams. Application traffic must be wrapped in TLS 1.3.

mod driver;
mod stream;
mod transport;

#[cfg(test)]
mod tests;

pub use stream::KcpStream;
pub use transport::KcpTransport;

use std::io;
use std::time::Duration;

#[derive(Clone, Copy, Debug)]
pub struct KcpSettings {
    pub mtu: u32,
    pub window: u16,
    pub interval: Duration,
    pub idle_timeout: Duration,
    pub max_sessions: usize,
    pub queue_capacity: usize,
}

impl KcpSettings {
    pub fn validate(self) -> io::Result<()> {
        if !(576..=1400).contains(&self.mtu)
            || self.window == 0
            || !(Duration::from_millis(1)..=Duration::from_secs(1)).contains(&self.interval)
            || self.idle_timeout < self.interval
            || self.max_sessions == 0
            || self.max_sessions > tokio::sync::Semaphore::MAX_PERMITS
            || self.queue_capacity == 0
            || self.queue_capacity > tokio::sync::Semaphore::MAX_PERMITS
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid KCP limits",
            ));
        }
        Ok(())
    }

    fn core_config(self) -> kcp_ovo::KcpConfig {
        kcp_ovo::KcpConfig {
            mtu: self.mtu,
            snd_wnd: self.window,
            rcv_wnd: self.window,
            interval: self.interval.as_millis() as u64,
            nocwnd: false,
            ..kcp_ovo::KcpConfig::stream_mode()
        }
    }
}
