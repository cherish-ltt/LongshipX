//! 网关启动期错误(运行期错误在协议/HTTP 层内联处理)。

#[derive(Debug, thiserror::Error)]
pub enum GatewayError {
    #[error("网络错误: {0}")]
    Net(#[from] ppt_tcp_net_kit::error::NetError),
    #[error("地址解析失败: {0}")]
    Addr(String),
    #[error("网关配置错误: {0}")]
    Config(String),
}
