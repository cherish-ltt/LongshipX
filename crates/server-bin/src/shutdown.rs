//! 优雅停机信号处理(PRD 13.2 🔴):SIGTERM / SIGINT → 停机令牌。

use tokio::sync::watch;

/// 阻塞直到收到 SIGTERM(Ctrl+C)或停止令牌已被触发。
pub async fn wait_for_signal(shutdown_tx: watch::Sender<bool>) {
    wait_signal().await;
    tracing::info!("收到停机信号,开始优雅停机");
    shutdown_tx.send_replace(true);
}

async fn wait_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = signal(SignalKind::terminate()).expect("注册 SIGTERM 失败");
        let mut int = signal(SignalKind::interrupt()).expect("注册 SIGINT 失败");
        tokio::select! {
            _ = term.recv() => {}
            _ = int.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
