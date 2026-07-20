fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 配置 tonic-build：从 .proto 编译出 Rust gRPC 代码
    tonic_build::configure()
        // 生成服务端 trait（供 daemon 实现）
        .build_server(true)
        // 生成客户端 stubs（供 CLI 调用）
        .build_client(true)
        // 编译 proto/visp.proto，搜索路径为 proto/ 目录
        .compile_protos(&["proto/visp.proto"], &["proto/"])?;
    Ok(())
}
