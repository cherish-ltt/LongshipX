//! 网络层错误类型。

/// net-kit 统一错误。
#[derive(Debug, thiserror::Error)]
pub enum NetError {
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
    #[error("TLS 错误: {0}")]
    Tls(String),
    #[error("TLS 配置错误: {0}")]
    TlsConfig(String),
    #[error("帧超限: 声明 {declared} 字节,上限 {max} 字节")]
    FrameTooLarge { declared: usize, max: usize },
    #[error("非法帧: {0}")]
    Malformed(String),
    #[error("发送队列已满")]
    QueueFull,
    #[error("连接已关闭")]
    Closed,
}

impl NetError {
    /// 是否为对端正常/异常断开导致的读结束。
    pub fn is_disconnect(&self) -> bool {
        match self {
            Self::Io(err) => matches!(
                err.kind(),
                std::io::ErrorKind::UnexpectedEof
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::BrokenPipe
            ),
            Self::Closed | Self::Tls(_) => true,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disconnect_detection() {
        let eof = std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "eof");
        assert!(NetError::Io(eof).is_disconnect());
        assert!(NetError::Closed.is_disconnect());
        assert!(!NetError::QueueFull.is_disconnect());
        let oversized = NetError::FrameTooLarge {
            declared: 10,
            max: 4,
        };
        assert_eq!(oversized.to_string(), "帧超限: 声明 10 字节,上限 4 字节");
    }
}
