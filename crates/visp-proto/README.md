# visp-proto — gRPC 协议定义

protobuf 协议定义 + `tonic-build` 自动代码生成。修改 `.proto` 后重新编译即可，生成代码在 `target/` 下，无需手动维护。

## 关键文件

- `proto/visp.proto` — 服务与消息定义
- `build.rs` — tonic-build 编译配置

## 依赖

无内部 crate 依赖。

## 测试

```bash
cargo test -p visp-proto
```
