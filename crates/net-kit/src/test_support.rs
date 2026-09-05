//! 测试支持(仅 #[cfg(test)]):进程内一次性生成自签证书,提供 TLS 客户端连接工具。

use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject;
use std::net::SocketAddr;
use std::sync::OnceLock;

struct CertFixture {
    cert_pem: String,
    key_pem: String,
}

static FIXTURE: OnceLock<CertFixture> = OnceLock::new();

fn fixture() -> &'static CertFixture {
    FIXTURE.get_or_init(|| {
        let key = rcgen::KeyPair::generate().expect("生成密钥失败");
        // 参数化 DNS SAN:客户端以 SNI "localhost" 完成校验。
        let params =
            rcgen::CertificateParams::new(vec!["localhost".to_string()]).expect("构造证书参数失败");
        let cert = params.self_signed(&key).expect("签发证书失败");
        CertFixture {
            cert_pem: cert.pem(),
            key_pem: key.serialize_pem(),
        }
    })
}

/// 默认测试证书与私钥(PEM)。
pub fn cert_pems() -> (&'static str, &'static str) {
    let f = fixture();
    (f.cert_pem.as_str(), f.key_pem.as_str())
}

/// 每次调用都生成一把新私钥(用于"证书与私钥不匹配"用例)。
pub fn fresh_key_pem() -> String {
    let key = rcgen::KeyPair::generate().expect("生成密钥失败");
    key.serialize_pem()
}

/// 用测试证书为信任根的 TLS 客户端连接服务端(完成握手)。
pub async fn tls_client_connect(
    addr: SocketAddr,
) -> Result<tokio_rustls::client::TlsStream<tokio::net::TcpStream>, String> {
    let (cert_pem, _) = cert_pems();
    let mut roots = rustls::RootCertStore::empty();
    for cert in CertificateDer::pem_slice_iter(cert_pem.as_bytes()) {
        roots
            .add(cert.map_err(|err| format!("证书解析失败: {err}"))?)
            .map_err(|err| format!("信任根添加失败: {err}"))?;
    }
    let config = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .map_err(|err| format!("TLS 版本配置失败: {err}"))?
    .with_root_certificates(roots)
    .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(std::sync::Arc::new(config));
    let tcp = tokio::net::TcpStream::connect(addr)
        .await
        .map_err(|err| format!("TCP 连接失败: {err}"))?;
    let domain = rustls::pki_types::ServerName::try_from("localhost".to_string())
        .map_err(|err| format!("服务器名解析失败: {err}"))?;
    connector
        .connect(domain, tcp)
        .await
        .map_err(|err| format!("TLS 握手失败: {err}"))
}
