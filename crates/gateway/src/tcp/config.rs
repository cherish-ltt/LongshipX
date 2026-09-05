//! TCP 网关运行参数(由 server-bin 从 Config 映射而来)。

use std::time::Duration;

#[derive(Debug, Clone, Copy)]
pub struct RateLimitSettings {
    pub enabled: bool,
    /// 每连接每秒允许的消息数。
    pub per_second: u64,
    /// 令牌桶突发容量。
    pub burst: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct TcpGatewayConfig {
    pub bind_addr: std::net::SocketAddr,
    pub max_frame_size: usize,
    pub send_queue_capacity: usize,
    pub room_event_capacity: usize,
    /// 未鉴权连接最大存活时间(PRD 8.3 🔴)。
    pub unauth_timeout: Duration,
    /// 已鉴权连接的心跳超时(PRD 8.4)。
    pub heartbeat_timeout: Duration,
    pub rate: RateLimitSettings,
    /// 最大同时在线连接数。
    pub max_connections: usize,
    /// TCP listen backlog。
    pub backlog: u32,
    /// 是否开启 TCP_NODELAY(禁用 Nagle)。
    pub nodelay: bool,
    /// TCP keepalive 探测间隔。
    pub keepalive: Duration,
}
