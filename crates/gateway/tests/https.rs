//! HTTPS 入口端到端测试:真实 TLS 握手访问 /healthz、明文监听仅做 308 跳转、HSTS 头。

use axum::Router;
use axum::routing::get;
use rustls::pki_types::pem::PemObject;
use std::sync::Arc;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;

fn test_certs() -> (&'static str, &'static str) {
    static CERTS: std::sync::OnceLock<(String, String)> = std::sync::OnceLock::new();
    let pair = CERTS.get_or_init(|| {
        let key = rcgen::KeyPair::generate().unwrap();
        let params = rcgen::CertificateParams::new(vec!["localhost".to_string()]).unwrap();
        let cert = params.self_signed(&key).unwrap();
        (cert.pem(), key.serialize_pem())
    });
    (pair.0.as_str(), pair.1.as_str())
}

fn tls_client_config() -> Arc<rustls::ClientConfig> {
    let (cert_pem, _) = test_certs();
    let mut roots = rustls::RootCertStore::empty();
    for cert in rustls::pki_types::CertificateDer::pem_slice_iter(cert_pem.as_bytes()) {
        roots.add(cert.unwrap()).unwrap();
    }
    Arc::new(
        rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth(),
    )
}

fn app() -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .layer(axum::middleware::from_fn(
            longshipx_gateway::http::hsts_middleware,
        ))
}

#[tokio::test]
async fn https_endpoint_serves_real_tls_with_hsts() {
    longshipx_net_kit::tls::install_ring_provider();
    let (cert_pem, key_pem) = test_certs();
    let tls = longshipx_net_kit::tls::server_config_from_pem_bytes(cert_pem, key_pem).unwrap();
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    let addr = longshipx_gateway::http::serve_https(
        "127.0.0.1:0".parse().unwrap(),
        tls,
        app(),
        shutdown_rx,
    )
    .await
    .unwrap();
    assert_ne!(addr.port(), 0, "应回传内核分配的实际端口");

    let connector = tokio_rustls::TlsConnector::from(tls_client_config());
    let tcp = TcpStream::connect(addr).await.unwrap();
    let domain = rustls::pki_types::ServerName::try_from("localhost".to_string()).unwrap();
    let mut tls_stream = connector.connect(domain, tcp).await.unwrap();

    tls_stream
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut response = String::new();
    tls_stream.read_to_string(&mut response).await.unwrap();

    assert!(
        response.starts_with("HTTP/1.1 200"),
        "应返回 200:{response}"
    );
    assert!(
        response
            .to_ascii_lowercase()
            .contains("strict-transport-security")
    );
    assert!(response.ends_with("ok"));

    shutdown_tx.send_replace(true);
}

#[tokio::test]
async fn plaintext_listener_only_redirects_to_https() {
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let addr = longshipx_gateway::http::serve_redirect("127.0.0.1:0".parse().unwrap(), shutdown_rx)
        .await
        .unwrap();

    let mut conn = TcpStream::connect(addr).await.unwrap();
    conn.write_all(
        b"POST /login?next=/me HTTP/1.1\r\nHost: game.example.com\r\nConnection: close\r\n\r\n",
    )
    .await
    .unwrap();
    let mut response = String::new();
    conn.read_to_string(&mut response).await.unwrap();

    assert!(
        response.starts_with("HTTP/1.1 308"),
        "应返回 308:{response}"
    );
    assert!(
        response
            .to_ascii_lowercase()
            .contains("location: https://game.example.com/login?next=/me")
    );

    shutdown_tx.send_replace(true);
}

#[tokio::test]
async fn serve_https_reports_bind_failure() {
    longshipx_net_kit::tls::install_ring_provider();
    let (cert_pem, key_pem) = test_certs();
    let tls = longshipx_net_kit::tls::server_config_from_pem_bytes(cert_pem, key_pem).unwrap();
    let (tx, rx) = tokio::sync::watch::channel(false);

    // 先占用一个端口,再让 HTTPS 服务绑同一端口 → 绑定失败路径。
    let occupied = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let occupied_addr = occupied.local_addr().unwrap();
    let result = longshipx_gateway::http::serve_https(occupied_addr, tls, app(), rx).await;

    assert!(result.is_err(), "占用端口应导致绑定失败");
    drop(occupied);
    tx.send_replace(true);
}
