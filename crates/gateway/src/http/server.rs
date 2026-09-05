//! HTTP 入口的 TLS 终结与 HTTP→HTTPS 跳转。
//!
//! * `serve_https`:只绑定 TLS 端口——进程内不存在明文路径(PRD 红线 R1);
//! * `serve_redirect`:可选的明文跳转监听,**仅当配置注入了地址才绑定**
//!   (不写死端口),且该监听只做一件事:308 到对应 HTTPS 地址。

use crate::error::GatewayError;
use axum::Router;
use axum::extract::Request;
use axum::http::HeaderValue;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::any;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;

/// 优雅停机窗口:收到停机信号后,给在途请求的收尾时间。
const GRACEFUL_SHUTDOWN_WINDOW: Duration = Duration::from_secs(15);

/// 绑定并运行 HTTPS 服务;返回实际绑定地址(端口 0 时由内核回传,供日志与测试)。
/// 收到停机信号后优雅退出(停止接受新连接,等待在途请求完成)。
pub async fn serve_https(
    addr: SocketAddr,
    tls: Arc<rustls::ServerConfig>,
    app: Router,
    mut shutdown: watch::Receiver<bool>,
) -> Result<SocketAddr, GatewayError> {
    let handle = axum_server::Handle::new();
    let tls = axum_server::tls_rustls::RustlsConfig::from_config(tls);
    let server = axum_server::tls_rustls::bind_rustls(addr, tls).handle(handle.clone());

    // 停机触发:信号 → 优雅停机。
    let shutdown_handle = handle.clone();
    tokio::spawn(async move {
        if shutdown.wait_for(|done| *done).await.is_ok() {
            shutdown_handle.graceful_shutdown(Some(GRACEFUL_SHUTDOWN_WINDOW));
        }
    });

    // serve 阻塞运行;绑定结果(含失败)经 Handle::listening 回传。
    let (bound_tx, bound_rx) = tokio::sync::oneshot::channel();
    let listening = handle;
    tokio::spawn(async move {
        let bound = listening.listening().await;
        let _ = bound_tx.send(bound.ok_or(GatewayError::Config("HTTPS 端口绑定失败".into())));
    });
    tokio::spawn(async move {
        if let Err(err) = server.serve(app.into_make_service()).await {
            tracing::error!(error = %err, "HTTPS 服务异常退出");
        }
    });

    bound_rx
        .await
        .map_err(|_| GatewayError::Config("HTTPS 监听任务意外退出".into()))?
}

/// HTTP→HTTPS 跳转路由:保留 Host 与 path?query,308 保持请求方法与语义。
pub fn redirect_router() -> Router {
    async fn to_https(request: Request) -> Response {
        let (parts, _) = request.into_parts();
        let host = parts
            .headers
            .get("host")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let path = parts
            .uri
            .path_and_query()
            .map(|part| part.as_str().to_owned())
            .unwrap_or_else(|| "/".to_string());
        let target = format!("https://{host}{path}");
        tracing::debug!(%target, "HTTP 请求跳转 HTTPS");
        Redirect::permanent(&target).into_response()
    }
    Router::new().fallback(any(to_https))
}

/// 绑定可选的明文跳转监听;仅输出 308 跳转,不承载任何业务。返回实际绑定地址。
pub async fn serve_redirect(
    addr: SocketAddr,
    mut shutdown: watch::Receiver<bool>,
) -> Result<SocketAddr, GatewayError> {
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|err| GatewayError::Net(err.into()))?;
    let bound = listener
        .local_addr()
        .map_err(|err| GatewayError::Net(err.into()))?;
    tracing::info!(%bound, "HTTP→HTTPS 跳转监听已就绪");
    let mut shutdown_for_axum = shutdown.clone();
    tokio::spawn(async move {
        let serve_future =
            axum::serve(listener, redirect_router()).with_graceful_shutdown(async move {
                let _ = shutdown_for_axum.wait_for(|done| *done).await;
            });
        if let Err(err) = serve_future.await {
            tracing::error!(error = %err, "跳转监听异常退出");
        }
    });
    let _ = &mut shutdown;
    Ok(bound)
}

/// 严格传输安全:HTTPS 响应统一携带 HSTS。
pub async fn hsts_middleware(request: Request, next: axum::middleware::Next) -> Response {
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        "strict-transport-security",
        HeaderValue::from_static("max-age=63072000; includeSubDomains"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::StatusCode;
    use tower::ServiceExt as _;

    #[tokio::test]
    async fn redirect_keeps_host_path_and_method_semantics() {
        let request = Request::builder()
            .method("POST")
            .uri("http://10.0.0.1/login?next=/me")
            .header("host", "game.example.com")
            .body(Body::empty())
            .unwrap();
        let response = redirect_router().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::PERMANENT_REDIRECT);
        assert_eq!(
            response.headers()["location"],
            "https://game.example.com/login?next=/me"
        );
    }

    #[tokio::test]
    async fn redirect_defaults_to_slash_when_path_missing() {
        let request = Request::builder()
            .uri("http://10.0.0.1")
            .header("host", "h.local")
            .body(Body::empty())
            .unwrap();
        let response = redirect_router().oneshot(request).await.unwrap();
        assert_eq!(response.headers()["location"], "https://h.local/");
    }

    #[tokio::test]
    async fn hsts_header_is_always_present() {
        let app = Router::new()
            .route("/x", axum::routing::get(|| async { "ok" }))
            .layer(axum::middleware::from_fn(hsts_middleware));
        let response = app
            .oneshot(Request::builder().uri("/x").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(
            response.headers()["strict-transport-security"],
            "max-age=63072000; includeSubDomains"
        );
    }
}
