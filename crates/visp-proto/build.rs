fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 使用 vendored protoc：不依赖系统安装的 protoc（CI 无需 apt/brew 步骤，
    // 本地构建也不再要求 `brew install protobuf`）。
    // SAFETY: build script 在编译期单线程执行，且此时尚未调用任何读取
    // `PROTOC` 的代码；进程环境修改不会与其他线程竞争。
    unsafe {
        std::env::set_var("PROTOC", protoc_bin_vendored::protoc_bin_path()?);
    }
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
