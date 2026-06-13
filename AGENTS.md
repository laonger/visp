# AGENTS.md — visp 项目指南

## 命名：vbw-* 已废弃

所有 crate 和二进制已从 `vbw-*` 重命名为 `visp-*`。代码注释、旧文档、配置文件中的 `vbw-*` 引用均为过时信息，忽略即可。

| 旧名 | 新名 |
|------|------|
| `vbw-core` | `visp-core` |
| `vbw-proto` | `visp-proto` |
| `vbw-llm` | `visp-llm` |
| `vbw-tools` | `visp-tools` |
| `vbw-codegraph` | `visp-codegraph` |
| `vbw-daemon` | `visp-daemon` |
| `vbw-cli` | `visp-cli` |
| `vbw` (bin) | `visp` (bin) |

## 运行方式

```bash
# 标准方式：一键启动 daemon + CLI
cargo run --bin visp -- -p /path/to/project

# 手动分别启动（调试用）
cargo run --bin visp-daemon            # 终端 1
cargo run --bin visp-cli -- -p <path>  # 终端 2

# 恢复 session（支持 short-id 前缀匹配）
cargo run --bin visp -- -p /path/to/project -s <session-id-or-prefix>

# 列出 session
cargo run --bin visp -- -p /path/to/project --list
```

`visp` launcher 会：启动 daemon → 健康检查（15s 超时）→ 启动 CLI → CLI 退出后发送 shutdown。

## 工作区结构

11 个 crate，均在 `crates/` 下：

```
crates/
├── visp/              ← launcher 入口，启 daemon + CLI
├── visp-core/         ← 纯逻辑，不依赖 IO（Tool trait、Agent 循环、Session、Prompt、Rules）
├── visp-proto/        ← gRPC 协议定义 + tonic-build 代码生成
├── visp-llm/          ← Anthropic / OpenAI API 集成
├── visp-tools/        ← 内置工具实现（file/bash/search/codegraph/fetch）
├── visp-codegraph/    ← tree-sitter 解析 + SQLite 索引（TS/TSX、Rust、Python、C/C++、Go）
├── visp-context/      ← DefaultContextTrimmer：HEAD+MIDDLE+TAIL 三段式裁剪
├── visp-db/           ← SQLite 持久化（SessionRepo、MessageRepo、schema 迁移）
├── visp-mcp/          ← MCP 客户端/服务端集成
├── visp-daemon/       ← gRPC 服务端，组装所有模块
└── visp-cli/          ← ratatui TUI 客户端
```

**依赖方向**：`core ← (llm, tools, context, proto, db) ← daemon/cli`。core 不依赖任何其他 crate。

## proto 代码生成

修改 `crates/visp-proto/proto/visp.proto` 后，重新编译即可自动生成 Rust 代码（`build.rs` 通过 `tonic-build` 处理）。生成的代码在 `target/` 下，无需手动维护。

如果 LSP 报 proto 字段不存在，重新 `cargo build` 一次即可。

## 质量门禁（TDD 提交前必须通过）

```bash
cargo test && cargo clippy -- -D warnings && cargo fmt -- --check
```

- `cargo test` — 全量测试（约 650+ 个）
- `cargo clippy -- -D warnings` — 零警告标准
- `cargo fmt -- --check` — 格式检查

单 crate 测试：`cargo test -p visp-core`

**提示**：修改 proto 后首次 `cargo test` 可能因编译锁冲突，先 `cargo build` 再 `cargo test`。

## 测试约定

- **测试与源码同文件**：`#[cfg(test)] mod tests { ... }`，不在 `tests/` 目录
- 异步测试用 `#[tokio::test]`
- Mock：自定义类型实现 trait（如 `TestProvider`、`MockTool`）

## 核心约束

### visp-core 禁止 IO

`visp-core` 不依赖任何 IO 操作（无文件读写、无网络、无进程启动）。所有 IO 由其他 crate 提供。新增代码若涉及文件/网络/进程，应放在 `visp-tools` 或其他 IO crate。

### 添加新工具

1. 在 `visp-tools/src/` 中实现 `Tool` trait
2. 在 `visp-daemon/src/main.rs` 中 `tool_registry.register(...)`
3. **禁止**修改 `visp-core` 的 `ToolContext`、`AgentLoopContext` 等核心结构来适配新工具

### visp-db schema 迁移

DB schema 版本号在 `crates/visp-db/src/schema.rs`（`Migrator::VERSION`）。`run()` 方法通过 `PRAGMA user_version` 检查版本，只运行未应用的迁移。新增列使用 `ALTER TABLE ADD COLUMN`，用 `pragma_table_info` 检查列是否已存在来保证幂等。

当前 Schema V2：message 表含 `tool_calls_json TEXT` 列（完整 tool_calls JSON）。

## CLI 命令系统

TUI 中的斜杠命令在 `crates/visp-cli/src/event.rs` 的 `handle_command()` 中处理：

| 命令 | 行为 |
|------|------|
| `/clear` | 清除聊天面板 |
| `/help` | 切换帮助弹窗 |
| `/list` | 列出所有 session（显示 short-id + 状态 + 最后一条用户消息） |
| `/sessions [id]` | 无参 = 同 `/list`；有参 = 切换到指定 session |
| `/new` | 创建新 session 并切换 |
| `/temp <0.0–1.0>` | 设置 temperature |
| `/model <name>` | 直接切换模型 |
| `/model` | 无参时弹出交互式选择器（↑↓选择，Enter切换） |
| `/init` | 发送初始化提示给 LLM |
| `/mouse` | 切换鼠标捕获模式 |

Tab 键会自动补全斜杠命令。

CLI 与 daemon 通过 gRPC 双向流通信（`Chat` RPC）。`ChatHandle` 封装了发送请求和接收响应的 mpsc channel。异步操作（如 `/new`、`/list`、`/sessions`）通过设置 `pending_*` 标志，由主 `tokio::select!` 循环处理。

## Session 管理

- `get_session` 支持 **前缀匹配**：先精确匹配，失败则前缀匹配（唯一匹配时返回）
- 恢复 session 后，daemon 在 `StatusUpdate.user_inputs` 中携带所有 User 消息，CLI 自动加载到 `input_history`，按 **↑↓** 键可翻找历史提问
- `/sessions <short-id>` 切换 session 时，自动 cancel 当前 loop → 更新 session_id → send_join 加载新历史 → 重置 UI

## 配置路径

- daemon 配置：`~/.config/visp/daemon.toml`（可选，所有字段有默认值）
  - 多模型配置示例：
    ```toml
    [llm]
    models = [
      { name = "Claude Sonnet", provider = "Anthropic", protocol = "anthropic", model = "claude-sonnet-4-20250514" },
      { name = "GPT-4o", provider = "OpenAI", protocol = "openai", model = "gpt-4o", api_key = "${OPENAI_API_KEY}" },
    ]
    ```
  - 无 `models` 时回退到单 `model` 字段（向后兼容）
- 项目规则：`.visp/rules/`（Markdown + YAML frontmatter）
- 全局规则：`~/.config/visp/rules/`
- daemon 日志：`~/.visp/logs/daemon-<timestamp>.log`
- 数据库：`~/.visp/data/visp.db`（默认，SQLite）

## 提交规范

```
feat(scope): 描述
fix(scope): 描述
docs(scope): 描述
refactor(scope): 描述
test(scope): 描述
chore(scope): 描述
```

scope 常用：`core`, `cli`, `daemon`, `db`, `proto`, `tools`, `llm`, `context`

## 探索本项目代码

本项目有 CodeGraph MCP 工具（`codegraph_*`）。**注意**：需要先运行 `codegraph init -i` 构建索引才能使用。索引有 ~500ms 延迟，写入文件后不要立即查询。

| 目的 | 工具 |
|---|---|
| 符号定位 | `codegraph_search` |
| 上下文理解 | `codegraph_context` → `codegraph_explore` |
| 调用链 | `codegraph_trace` / `codegraph_callers` / `codegraph_callees` |
| 影响分析 | `codegraph_impact` |
| 文件树 | `codegraph_files` |

## 任务流程

中/复杂任务需要先写设计文档，再写工作计划，审核通过后执行：

- 设计文档 → `docs/design/visp-design-*.md`（只描述架构/流程/职责，不含代码）
- 工作计划 → `docs/plans/visp-plan-*.md`（TDD 步骤 + 测试清单 + 验证标准）
- 简单任务（单文件、<50 行、无架构影响）跳过文档，直接进入 TDD 循环
