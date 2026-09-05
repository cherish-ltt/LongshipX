//! 可观测性初始化:结构化日志(PRD 12)与 Prometheus 指标端点。

use axum::Router;
use axum::routing::get;
use std::net::SocketAddr;

/// 初始化 tracing:LOG_LEVEL 控制级别,LOG_FORMAT 选 json/pretty。
pub fn init_tracing(level: &str, format: &str) {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_new(level).unwrap_or_else(|_| EnvFilter::new("info"));
    let builder = tracing_subscriber::fmt().with_env_filter(filter);
    if format.eq_ignore_ascii_case("json") {
        builder.json().init();
    } else {
        builder.init();
    }
}

/// 拉起 /metrics 端点(Prometheus 文本格式)。
pub async fn serve_metrics(
    addr: SocketAddr,
    recorder_handle: metrics_exporter_prometheus::PrometheusHandle,
) {
    let app = Router::new().route(
        "/metrics",
        get(move || async move { recorder_handle.render() }),
    );
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(listener) => listener,
        Err(err) => {
            tracing::error!(%addr, error = %err, "指标端口绑定失败(服务继续运行)");
            return;
        },
    };
    tracing::info!(%addr, "Prometheus 指标端点已就绪");
    if let Err(err) = axum::serve(listener, app).await {
        tracing::error!(error = %err, "指标服务异常退出");
    }
}
