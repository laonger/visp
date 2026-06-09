# visp-tools — 内置工具实现

提供 Agent 可调用的内置工具：文件读写、bash 执行、搜索、网页获取、代码图谱分析等。

## 关键文件

- `file.rs` — ReadFile / WriteFile / EditFile
- `bash.rs` — Shell 执行
- `search.rs` — Grep / Glob
- `codegraph.rs` — CodeGraphSearch / CodeGraphGetDetails
- `fetch.rs` — WebFetch 网页获取

## 依赖

- `visp-core`（Tool trait、ToolContext、ToolResult）

## 测试

```bash
cargo test -p visp-tools
```
