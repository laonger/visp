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
```

`visp` launcher 会：启动 daemon → 健康检查（15s 超时）→ 启动 CLI → CLI 退出后发送 shutdown。

## 工作区结构

8 个 crate，均在 `crates/` 下：

```
crates/
├── visp/            ← launcher 入口，启 daemon + CLI
├── visp-core/       ← 纯逻辑，不依赖 IO（Tool trait、Agent 循环、Session、Prompt、Rules）
├── visp-proto/      ← gRPC 协议定义 + tonic-build 代码生成
├── visp-llm/        ← Anthropic API 集成
├── visp-tools/      ← 内置工具实现（file/bash/search/codegraph/fetch）
├── visp-codegraph/  ← tree-sitter 解析 + SQLite 索引（支持 TS/TSX、Rust、Python、C/C++、Go）
├── visp-daemon/     ← gRPC 服务端，组装所有模块
└── visp-cli/        ← ratatui TUI 客户端
```

## proto 代码生成

修改 `crates/visp-proto/proto/visp.proto` 后，重新编译即可自动生成 Rust 代码（`build.rs` 通过 `tonic-build` 处理）。生成的代码在 `target/` 下，无需手动维护。

## 质量门禁（TDD 提交前必须通过）

```bash
cargo test && cargo clippy -- -D warnings && cargo fmt -- --check
```

- `cargo test` — 全量测试（约 200+ 个）
- `cargo clippy -- -D warnings` — 零警告标准
- `cargo fmt -- --check` — 格式检查

单 crate 测试：`cargo test -p visp-core`

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

## 提交规范

```
feat(scope): 描述
fix(scope): 描述
docs(scope): 描述
refactor(scope): 描述
test(scope): 描述
chore(scope): 描述
```

## 配置路径

- daemon 配置：`~/.config/visp/daemon.toml`（可选，所有字段有默认值）
- 项目规则：`.visp/rules/`（Markdown + YAML frontmatter）
- 全局规则：`~/.config/visp/rules/`
- daemon 日志：`~/.visp/logs/daemon-<timestamp>.log`

## 探索本项目代码

本项目有 CodeGraph MCP 工具（`codegraph_*`）。探索 visp 源码时优先使用 `codegraph_*` 而非 `grep`：

| 目的 | 工具 |
|---|---|
| 符号定位 | `codegraph_search` |
| 上下文理解 | `codegraph_context` → `codegraph_explore` |
| 调用链 | `codegraph_trace` / `codegraph_callers` / `codegraph_callees` |
| 影响分析 | `codegraph_impact` |
| 文件树 | `codegraph_files` |

CodeGraph 结果来自 AST 解析，不要用 grep 去二次验证。索引有 ~500ms 延迟，写入文件后不要立即查询。

## 任务流程

中/复杂任务需要先写设计文档，再写工作计划，审核通过后执行：

- 设计文档 → `docs/design/visp-design-*.md`（只描述架构/流程/职责，不含代码）
- 工作计划 → `docs/plans/visp-plan-*.md`（TDD 步骤 + 测试清单 + 验证标准）
- 简单任务（单文件、<50 行、无架构影响）跳过文档，直接进入 TDD 循环
