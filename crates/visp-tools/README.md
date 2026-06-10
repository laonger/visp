# visp-tools — 内置工具实现

提供 Agent 可调用的 9 个内置工具：

| 工具 | 功能 |
|------|------|
| `ReadFile` / `WriteFile` / `EditFile` | 文件读写与精确替换 |
| `Bash` | Shell 命令执行（安全黑名单 + 超时控制） |
| `Grep` / `Glob` | 正则搜索 / 文件名搜索 |
| `WebFetch` | 网页内容获取与提取 |
| `CodeGraphSearch` / `CodeGraphGetDetails` | AST 符号搜索与调用链查询 |

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
