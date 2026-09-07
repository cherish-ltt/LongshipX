//! 大规模连接基准的公共装置:fd 上限提升、建连闸门与看门狗。
//!
//! 被多个基准目标各自独立编译,部分条目在单个目标内未被用到属预期。
#![allow(dead_code)]

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::TcpStream;
use tokio::sync::Semaphore;

/// 建连/入房阶段的并发度:必须低于内核 accept 队列上限
/// (macOS `kern.ipc.somaxconn=128`),并依赖服务端并发握手管线即时排空队列。
pub const SETUP_CONCURRENCY: usize = 64;

/// 看门狗:事件被有界队列丢弃时宁可报错也不允许基准挂死。
pub const WATCHDOG: Duration = Duration::from_secs(120);

/// 尽力把进程 fd 上限提升到硬上限(macOS 默认 256,大规模档位必需)。
pub fn raise_fd_limit() {
    #[cfg(unix)]
    unsafe {
        let mut current = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut current) == 0
            && current.rlim_cur < current.rlim_max
        {
            let raised = libc::rlimit {
                rlim_cur: current.rlim_max,
                rlim_max: current.rlim_max,
            };
            let _ = libc::setrlimit(libc::RLIMIT_NOFILE, &raised);
        }
    }
}

/// 建连闸门:把任务压成固定并发,permit 在任务主体完成即归还。
///
/// 注意与"permit 持有到连接生命周期结束"的区别:后者会让第 N+1 批建连
/// 永久阻塞,无法到达目标连接数。
pub struct ConnectGate {
    semaphore: Arc<Semaphore>,
}

impl ConnectGate {
    pub fn new(concurrency: usize) -> Self {
        Self {
            semaphore: Arc::new(Semaphore::new(concurrency.max(1))),
        }
    }

    /// 允许的同时在途任务数。
    pub fn concurrency(&self) -> usize {
        self.semaphore.available_permits().max(1)
    }

    /// 并发运行 `tasks` 产生的全部任务:任意时刻至多 `concurrency` 个在途,
    /// 返回所有任务的输出(按完成顺序)。
    pub async fn run<T, I, F, Fut>(&self, tasks: I) -> Vec<T>
    where
        I: IntoIterator<Item = F>,
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        let mut outputs = Vec::new();
        let mut in_flight = tokio::task::JoinSet::new();
        for build in tasks {
            let permit = self
                .semaphore
                .clone()
                .acquire_owned()
                .await
                .expect("建连闸门已关闭");
            in_flight.spawn(async move {
                let output = build().await;
                drop(permit);
                output
            });
        }
        while let Some(joined) = in_flight.join_next().await {
            outputs.push(joined.expect("基准任务 panic"));
        }
        outputs
    }
}

/// 计数器等待 + 看门狗:达到 `target` 前每 5ms 轮询一次,超时即 panic。
pub async fn wait_for_count(counter: &std::sync::atomic::AtomicUsize, target: usize, what: &str) {
    use std::sync::atomic::Ordering;
    let deadline = tokio::time::Instant::now() + WATCHDOG;
    while counter.load(Ordering::Acquire) < target {
        assert!(
            tokio::time::Instant::now() < deadline,
            "等待超时:{what}(目标 {target},当前 {})",
            counter.load(Ordering::Acquire)
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// 建立到 `addr` 的 TCP 连接(负载生成器语义:`SO_REUSEADDR` + 可选 `SO_LINGER=0`)。
///
/// `linger_reset` 供高频建连-关闭的基准(如 `connect_tls_bind`)使用:正常 FIN
/// 关闭会让每个连接以 TIME_WAIT 滞留 ~30s,速率 ~1.4K/s 时需要 ~42K 临时端口,
/// 远超 macOS 默认区间(49152–65535),内核报 `EADDRNOTAVAIL`;`SO_LINGER=0`
/// 使关闭直接发 RST、端口立即可复用。**只在短命连接上启用**:长连接(房间成员/
/// 心跳客户端)必须走 FIN,否则 RST 会污染同机其他连接。非阻塞 `connect` 的
/// `EINPROGRESS` 是正常路径:等可写后用 `SO_ERROR` 确认。
pub async fn tcp_connect_reuse(
    addr: std::net::SocketAddr,
    linger_reset: bool,
) -> std::io::Result<TcpStream> {
    let local: std::net::SocketAddr = if addr.is_ipv4() {
        "0.0.0.0:0".parse().unwrap()
    } else {
        "[::]:0".parse().unwrap()
    };
    let socket = Socket::new(
        Domain::for_address(local),
        Type::STREAM,
        Some(Protocol::TCP),
    )?;
    socket.set_reuse_address(true)?;
    if linger_reset {
        socket.set_linger(Some(Duration::ZERO))?;
    }
    socket.set_nonblocking(true)?;
    socket.bind(&socket2::SockAddr::from(local))?;
    match socket.connect(&socket2::SockAddr::from(addr)) {
        Ok(()) => {},
        Err(err) if err.raw_os_error() == Some(libc::EINPROGRESS) => {},
        Err(err) => return Err(err),
    }
    let stream = TcpStream::from_std(socket.into())?;
    stream.writable().await?;
    if let Some(err) = stream.take_error()? {
        return Err(err);
    }
    Ok(stream)
}

/// 给任意异步段加看门狗:超时报错而非挂死(用于事件排空路径)。
pub async fn watchdog<F: Future>(what: &str, future: F) -> F::Output {
    tokio::time::timeout(WATCHDOG, future)
        .await
        .unwrap_or_else(|_| panic!("看门狗超时:{what}"))
}
