# visp-proto — gRPC 协议定义

protobuf 协议定义 + `tonic-build` 自动代码生成。修改 `.proto` 后重新编译即可，生成代码在 `target/` 下，无需手动维护。

## 服务定义

`CoderDaemon` 服务，基于 tonic：

| RPC | 类型 | 说明 |
|-----|------|------|
| `Chat` | 双向流 | 核心对话通道 |
| `CreateSession` / `ListSessions` / `DeleteSession` | 一元 | 会话管理 |
| `ReadFile` | 一元 | 快速文件读取（跳过 LLM） |
| `SearchSymbols` / `GetSymbolDetails` | 一元 | 代码符号查询 |
| `HealthCheck` / `Shutdown` | 一元 | 健康检查 / 优雅关闭 |

## 关键文件

- `proto/visp.proto` — 服务与消息定义
- `build.rs` — tonic-build 编译配置

## 依赖

无内部 crate 依赖。

## 测试

```bash
cargo test -p visp-proto
```
