//! 构建脚本:用 protox(纯 Rust protobuf 编译器)编译 .proto,
//! 再交给 prost 生成 Rust 类型 —— CI 与开发机都无需安装 protoc。

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_root = std::path::Path::new("proto");
    println!("cargo:rerun-if-changed=proto/game.proto");
    let file_descriptors = protox::compile([proto_root.join("game.proto")], [proto_root])?;
    prost_build::compile_fds(file_descriptors)?;
    Ok(())
}
