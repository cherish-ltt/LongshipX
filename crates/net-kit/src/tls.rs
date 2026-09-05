//! TLS 服务端配置:强制 TLS 1.3(PRD 第 10 章 🔴 禁止降级)。

use crate::error::NetError;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::path::Path;
use std::sync::Arc;

/// 安装 ring 加密提供器(进程内幂等)。
pub fn install_ring_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// 从 PEM 证书/私钥构建 TLS 1.3-only 的服务端接收器。
pub fn server_acceptor_from_files(
    cert_path: &Path,
    key_path: &Path,
) -> Result<crate::TlsAcceptor, NetError> {
    let cert_pem = std::fs::read(cert_path).map_err(|err| {
        NetError::TlsConfig(format!("读取证书 {:?} 失败: {err}", cert_path.display()))
    })?;
    let key_pem = std::fs::read(key_path).map_err(|err| {
        NetError::TlsConfig(format!("读取私钥 {:?} 失败: {err}", key_path.display()))
    })?;
    server_acceptor_from_pem_bytes(
        &String::from_utf8_lossy(&cert_pem),
        &String::from_utf8_lossy(&key_pem),
    )
}

/// 从 PEM 字节串构建 TLS 1.3-only 的服务端接收器(便于测试)。
pub fn server_acceptor_from_pem_bytes(
    cert_pem: &str,
    key_pem: &str,
) -> Result<crate::TlsAcceptor, NetError> {
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(cert_pem.as_bytes())
        .collect::<Result<_, _>>()
        .map_err(|err| NetError::TlsConfig(format!("解析证书失败: {err}")))?;
    if certs.is_empty() {
        return Err(NetError::TlsConfig("证书文件中没有任何证书".into()));
    }
    let key = PrivateKeyDer::from_pem_slice(key_pem.as_bytes())
        .map_err(|err| NetError::TlsConfig(format!("解析私钥失败: {err}")))?;
    build_acceptor(certs, key)
}

fn build_acceptor(
    certs: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
) -> Result<crate::TlsAcceptor, NetError> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|err| NetError::TlsConfig(format!("TLS 版本配置失败: {err}")))?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|err| NetError::TlsConfig(format!("证书与私钥不匹配: {err}")))?;
    Ok(crate::TlsAcceptor::from(Arc::new(config)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support;

    #[test]
    fn builds_acceptor_from_test_pem() {
        install_ring_provider();
        let (cert, key) = test_support::cert_pems();
        assert!(server_acceptor_from_pem_bytes(cert, key).is_ok());
    }

    #[test]
    fn rejects_bad_pem() {
        assert!(server_acceptor_from_pem_bytes("not a pem", "not a pem").is_err());
        let (cert, _key) = test_support::cert_pems();
        assert!(matches!(
            server_acceptor_from_pem_bytes(cert, "broken"),
            Err(NetError::TlsConfig(_))
        ));
    }

    #[test]
    fn rejects_mismatched_key() {
        let (cert, _key) = test_support::cert_pems();
        let other_key = test_support::fresh_key_pem();
        // 证书相同但私钥来自另一个实例 → 证书/私钥不匹配。
        assert!(server_acceptor_from_pem_bytes(cert, &other_key).is_err());
    }

    #[test]
    fn missing_file_is_config_error() {
        let err = server_acceptor_from_files(
            Path::new("/nonexistent.crt"),
            Path::new("/nonexistent.key"),
        );
        assert!(matches!(err, Err(NetError::TlsConfig(_))));
    }
}
