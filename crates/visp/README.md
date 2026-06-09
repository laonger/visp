# visp — Launcher

visp 启动器，一键启动 daemon → 健康检查 → CLI → 等 CLI 退出 → 发 shutdown → 等 daemon 退出。

## 关键文件

- `src/main.rs` — 唯一入口，完整的启动与关闭编排逻辑

## 依赖

- `visp-proto`（gRPC 客户端，用于健康检查和 shutdown）

## 测试

```bash
cargo test -p visp
```
