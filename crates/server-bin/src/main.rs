//! LongshipX 服务器组装根:唯一的可执行文件(PRD 4.2)。

mod bootstrap;
mod observability;
mod shutdown;

fn main() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("构建 tokio 运行时失败");
    if let Err(err) = runtime.block_on(bootstrap::run()) {
        eprintln!("服务启动失败: {err}");
        std::process::exit(1);
    }
}
