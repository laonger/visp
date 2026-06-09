# visp-daemon — gRPC 服务端

常驻守护进程，组装所有模块并提供 gRPC 服务。新工具注册在 `main.rs` 的 `tool_registry.register()` 进行。

## 关键文件

- `main.rs` — 入口，模块组装与工具注册
- `service.rs` — gRPC CoderDaemon trait 实现
- `server.rs` — gRPC 服务器启动
- `config.rs` — TOML 配置加载

## 依赖

- `visp-core`、`visp-proto`、`visp-llm`、`visp-tools`、`visp-codegraph`

## 测试

```bash
cargo test -p visp-daemon
```
