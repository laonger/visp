# visp 全量代码审查报告

> 审查日期：2026-06-14
> 覆盖：11 个 crate，61 个源文件，~2000 个符号
> 审查方式：每个 crate 由 @oracle 独立审查后汇总

---

## 总体概览

> 已修复 4 严重 + 10 中 + 15 低 · 剩余 **2 严重 + 32 中 + 56 低**

- **visp-core**：2 严重 / 4 中 / 1 低 — 架构违规（IO），~~错误消息 bug ✅~~
- **visp-llm**：0 严重 / 4 中 / 1 低 — ~~大段代码重复 ✅~~，工具调用计数缺失
- **visp-tools**：1 严重 / 5 中 / 5 低 — ReadFile schema 矛盾
- **visp-codegraph**：0 严重 / 4 中 / 12 低 — ~~评分算法 bug ✅~~，哨兵值设计脆弱
- **visp-context**：0 严重 / 0 中 / 5 低 — 无严重问题
- **visp-daemon**：0 严重 / 3 中 / 9 低 — ~~死代码掩盖 ✅~~、~~MCP 优雅关闭 ✅~~、~~前缀歧义误导 ✅~~
- **visp-cli**：0 严重 / 2 中 / 9 低 — ~~Box::leak 内存泄漏 ✅~~、~~VbwClient 重命名 ✅~~
- **visp (launcher)**：0 严重 / 1 中 / 5 低 — 参数重复定义
- **visp-db**：0 严重 / 4 中 / 2 低 — ~~skip_context 持久化丢失 ✅~~
- **visp-mcp**：0 严重 / 5 中 / 4 低 — ~~SSE 多行解析 ✅~~，Clone 隐患
- **visp-proto**：0 严重 / 0 中 / 5 低 — 冗余字段，模糊约定

---

## A 级：必须修复（严重）

### A1. visp-core — 违反核心设计约束（IO 操作）

**文件**：`crates/visp-core/src/rules.rs`、`crates/visp-core/src/session.rs`
**描述**：visp-core 的设计约束是"纯逻辑、不依赖任何 IO"，但这俩文件中有 10+ 处文件读取、目录遍历和环境变量访问：
- `RuleEngine::new()` 调用 `std::fs::read_to_string`（3 处）、`Path::is_file()`、`Path::is_dir()`
- `discover_agents_md()` 遍历目录树
- `collect_rules()` 调用 `std::fs::read_dir`、`std::fs::read_to_string`
- `home_dir()` 读取 `HOME` 环境变量
- `load_system_prompt_template()` 读取 `.visp/system-prompt.md`
- `load_skills_inner()` → `load_skills_from_dir()` 读取技能文件

**建议**：将所有配置文件加载逻辑移到 `visp-daemon`，core 只维护纯数据结构和无副作用的构建函数。另外 `CoreError::Io(#[from] std::io::Error)` 的存在也默许了 IO——如果 core 真的禁止 IO，这个变体也不应存在。

### ~~A2. visp-db — `skip_context` 字段在持久化中丢失~~ ✅ 已修复

**文件**：`crates/visp-db/src/message_repo.rs:166`、`crates/visp-db/src/schema.rs`
**修复内容**：
- Schema V2→V3 升级，message 表新增 `skip_context INTEGER NOT NULL DEFAULT 0` 列
- INSERT 写入 `skip_context` 字段
- SELECT 读取 `skip_context`（替代硬编码 `false`）
- 新增 3 个测试：`test_migrate_v2_to_v3`、`test_skip_context_roundtrip`、`test_skip_context_v2_default`

### ~~A3. visp-core — `SessionError::AlreadyExists` 错误消息错误~~ ✅ 已修复

**文件**：`crates/visp-core/src/error.rs:50`
**修复内容**：`#[error("Session not found: {0}")]` → `#[error("Session already exists: {0}")]`

### A4. visp-tools — ReadFile 参数 schema 与实际行为矛盾

**文件**：`crates/visp-tools/src/file.rs:173`
**描述**：`parameters()` 声明 `"required": ["path"]`，但 `execute()` 在 path 不存在时尝试使用 `paths`（多文件模式）且能成功。`paths` 功能实际上是死代码——LLM 永远不会发送只有 `paths` 的请求。

**建议**：要么移除 `paths`，要么将 `required` 改为空数组并在 execute 中要求至少提供 path 或 paths 之一。

### ~~A5. visp-cli — `Box::leak` 内存泄漏~~ ✅ 已修复

**文件**：`crates/visp-cli/src/client.rs:116`、`app.rs:601`、`event.rs:815`
**修复内容**：
- `send_input()` 返回类型 `&'static str` → `String`
- 移除 `Box::leak(rid.into_boxed_str())`，直接返回 `rid`
- `AppState.current_request_id` 类型 `Option<&'static str>` → `Option<String>`
- `send_ack()` 调用点添加 `&` 借用

### ~~A6. visp-codegraph — 搜索评分算法在无 FTS 匹配时完全失效~~ ✅ 已修复

**文件**：`crates/visp-codegraph/src/query.rs:89-93`
**修复内容**：
- `max_fts` 初始值 `f64::NEG_INFINITY` → `0.0`（当 FTS5 返回 0 结果时，LIKE 回退结果不再得 `NEG_INFINITY` 分）
- 新增 `test_search_fts_empty_like_fallback` 测试覆盖 FTS→LIKE 回退路径

---

## B 级：建议修复（中等）

### visp-core

1. **B1** — DRY 违规：`system()/user()/assistant()/tool()` 四个构造函数逐字重复 20+ 字段（`crates/visp-core/src/message.rs:94-189`）。建议抽取私有辅助函数。
2. **B2** — token 估算不一致：`tool_call()` 不调用 `estimate_message_tokens`（`crates/visp-core/src/message.rs:191-211`）。建议统一切入点。
3. ~~**B3** — 双重分配：`register()` 接受 `Box` 但内部 `Arc::from` 重新分配~~ ✅ 已修复（`register/register_mcp/update` 参数改为 `Arc<dyn Tool>`，内部直接 push/赋值）
4. ~~**B4** — 错误类型不一致：`register()/remove()/update()` 返回 `Result<(), String>`~~ ✅ 已修复（统一为 `Result<(), CoreError>`）
5. **B5** — 函数过长：`run_agent_loop()` 700+ 行（`crates/visp-core/src/agent.rs:256-980`）。建议拆分为子函数。
6. **B6** — `finish_loop` 死参数：`_status` 从未使用，始终强制设为 Idle（`crates/visp-core/src/session.rs:447`）。建议删除参数或实际使用它。

### visp-llm

1. ~~**B7** — 大段代码重复：SSE 事件处理逻辑在两处几乎相同（~70 行）（`crates/visp-llm/src/anthropic.rs:567-677` vs `:694-744`）。建议提取为公共函数。~~ ✅ 已修复（提取 `update_anthropic_usage` + `build_done_usage_info` 两个辅助函数，消除 Emit/Usage 重复）
2. ~~**B8** — 同样的大段代码重复：`StreamEnd` 处理和流自然结束处理相同（`crates/visp-llm/src/openai.rs:514-525` vs `:589-601`）。建议提取为方法。~~ ✅ 已修复（提取 `flush_tool_acc` 嵌套函数，两处替换为单行调用）
3. **B9** — 工具调用顺序不确定：`HashMap::drain()` + `rev()` 不能保证顺序（`crates/visp-llm/src/openai.rs:486-489`）。建议使用 `BTreeMap` 或按 index 排序。
4. **B10** — 工具调用计数始终为 0：Anthropic provider 的 `UsageInfo.tool_calls`（`crates/visp-llm/src/anthropic.rs`）。建议实现工具调用计数。
5. **B11** — JSON key 类型风险：HashMap key 用 `index.to_string()`（`crates/visp-llm/src/anthropic.rs:498,512`）。建议添加注释说明假设。
6. **B12** — UTF-8 处理不一致：openai 用 `from_utf8_lossy`，anthropic 检测并返回错误（`crates/visp-llm/src/openai.rs:583`）。建议统一为检测+返回错误。

### visp-tools

1. **B13** — `is_destructive_command` 遗漏 `&&` 操作符：`&&rm` 绕过检测（`crates/visp-tools/src/bash.rs:82-103`）。建议添加 `"&&rm"`、`";rm"` 模式。
2. **B14** — 阻塞 IO 在 async 函数中：`ReadFile/EditFile` 调用 `std::fs::*`（`crates/visp-tools/src/file.rs`）。建议使用 `tokio::fs` 或 `spawn_blocking`。
3. **B15** — CodeGraph 工具重复的 DB 检查逻辑：5 个工具相同代码（`crates/visp-tools/src/codegraph.rs`）。建议提取 `open_codegraph_or_error`。
4. **B16** — `CodeGraphRebuild` 与其他工具不一致：缺少 `from_toml()`（`crates/visp-tools/src/codegraph.rs`）。建议统一构造函数。
5. **B17** — 白名单检查语义不一致：`requires_approval_for` 只同步检查 daemon 级白名单（`crates/visp-tools/src/fetch.rs`）。建议同步加载项目级白名单。

### visp-codegraph

1. **B18** — `edge_kind_from_str` 死代码（`crates/visp-codegraph/src/store.rs:444-453`）。建议删除。
2. **B19** — `resolve_cross_file_edges` 歧义：同名符号任意取第一个（`crates/visp-codegraph/src/index.rs:296-303`）。建议优先匹配同文件/同包，或记录歧义。
3. **B20** — `add_edge` 哨兵值 `unwrap_or(0)`：依赖 SQLite id 从 1 开始的隐式约定（`crates/visp-codegraph/src/parser.rs:682-689`）。建议改为 `Option` 或跳过。
4. **B21** — `resolve_import_source` 死代码（`crates/visp-codegraph/src/index.rs:313-332`）。建议确认是否保留。
5. **B22** — `expand_impact_dir` 静默跳过失效邻居（`crates/visp-codegraph/src/query.rs:313-320`）。建议记录 warning。

### visp-daemon

1. ~~**B23** — 6 个死代码函数被 `#![allow(dead_code)]` 掩盖（`crates/visp-daemon/src/config.rs:139-151`）~~ ✅ 已修复
   - ~~删除 5 个死代码函数：`default_provider`、`default_model`、`default_temperature`、`default_max_tokens`、`default_max_context_tokens`~~
   - ~~删除 2 个依赖死代码的冗余测试：`test_default_max_context_tokens`、`test_default_config_max_context_tokens`~~
   - ~~移除模块级 `#![allow(dead_code)]`，改用精确的字段级注解~~
2. **B24** — `hard_limit: 200` 硬编码（`crates/visp-daemon/src/main.rs:199`）。建议加入配置或加注释。
3. ~~**B25** — Ctrl+C 不触发 MCP 优雅关闭（`crates/visp-daemon/src/main.rs:232`）。~~ ✅ 已修复（clone Arc 保留引用，Ctrl+C 后调用 `shutdown_all().await`）
4. ~~**B26** — 前缀匹配歧义时返回误导性错误：`Status::not_found("Session not found")`（`crates/visp-daemon/src/service.rs:284-286`）。~~ ✅ 已修复（改为 `Status::invalid_argument("Ambiguous session prefix")`，测试同步更新）
5. **B27** — read_file session 不存在时回退到 `"."`（`crates/visp-daemon/src/service.rs:695-696`）。建议返回 `Status::not_found`。
6. **B28** — search_symbols limit=0 强制变为 20（边界语义模糊）（`crates/visp-daemon/src/service.rs:734-735`）。建议改为 `if limit == 0 { 20 }` 并注释。

### visp-cli

1. ~~**B29** — 旧命名 `VbwClient`（`crates/visp-cli/src/main.rs:8`）。建议重命名为 `VispClient`。~~ ✅ 已修复
2. **B30** — `syntect` 语法集/主题每次调用重新加载（`crates/visp-cli/src/app.rs:37-38`）。建议 `OnceLock` 或 `lazy_static` 缓存。
3. **B31** — Regex 每次调用重新编译（`crates/visp-cli/src/app.rs:100`）。建议 `OnceLock<Regex>`。

### visp (launcher)

1. **B32** — CLI 参数在两个 crate 中重复定义（`crates/visp/src/main.rs` + `crates/visp-cli/src/main.rs`）。建议共享同一个参数结构。

### visp-db

1. ~~**B33** — `row_to_session` 三处代码重复（`crates/visp-db/src/session_repo.rs:48-196`）~~ ✅ 已修复（提取 `fn row_to_session(&Row) -> Result<Session>`，`get/list/list_by_project` 三处替换为单行调用）
2. **B34** — JSON 序列化失败时静默吞错（`unwrap_or_default`）（`crates/visp-db/src/message_repo.rs:46-53`、`crates/visp-db/src/session_repo.rs:17-19`）。建议改为返回错误或记录 `tracing::warn!`。
3. **B35** — `Session.created_at` (Instant) 从 DB 加载时硬编码为 `Instant::now()`（`crates/visp-db/src/session_repo.rs:81,127,179`）。建议改为 `Option<Instant>` 标记或仅依赖 `created_at_unix`。
4. **B36** — `update()` 返回值未检查（`crates/visp-db/src/store.rs:121-123`）。建议检查受影响行数，不存在时返回 `NotFound`。
5. **B37** — `title` 列始终插入空字符串（死列）（`crates/visp-db/src/session_repo.rs:28`）。建议添加 `title` 字段或从 schema 中移除。

### visp-mcp

1. ~~**B38** — SSE 不处理多行 data（`crates/visp-mcp/src/http_client.rs:113-131`）~~ ✅ 已修复（重写 `extract_sse_data`，收集同事件多行 `data:` 用 `\n` 拼接；新增 `test_extract_sse_data_multiline`）
2. **B39** — `HttpPostClient` Clone 后 `initialized` 字段丢失（`crates/visp-mcp/src/http_client.rs:27`）。建议放入 `Arc<AtomicBool>` 或手动实现 Clone。
3. **B40** — `connected` 字段与 `session.is_some()` 冗余（`crates/visp-mcp/src/client.rs:42`）。建议移除 `connected` 字段。
4. **B41** — 自定义 `urlencoding()` 不完整/双重编码风险（`crates/visp-mcp/src/get_client.rs:284-298`）。建议使用 `url` crate。
5. **B42** — 30 秒轮询检测连接断开（`crates/visp-mcp/src/manager.rs:158-175`）。建议对 StdioSse 使用 `child.wait()` 即时通知。
6. **B43** — `execute()` 在整个调用期间持有 session 锁（`crates/visp-mcp/src/tool.rs:113-153`）。当前串行可接受，并行化时需优化。

---

## C 级：可优化的低优先级问题

### visp-core

- ~~`crates/visp-core/src/error.rs`：无意义的通条测试 `test_stuck_in_loop_match`（第 177-184 行）~~ ✅ 已删除
- ~~`crates/visp-core/src/error.rs`：5 个 `test_llmerror_*_display` 测试 thiserror 宏本身~~ ✅ 已删除（连带空模块一并移除）
- `crates/visp-core/src/message.rs`：`actual_*` 可选字段过多（20 个字段），建议聚合为 `MessageMetadata`
- `crates/visp-core/src/tool_registry.rs`：`drop(core_names)` 显式释放锁是绕过嵌套问题的临时方案
- `crates/visp-core/src/agent.rs`：`chrono::Local::now()` 时间系统调用（违反无 IO？）
- `crates/visp-core/src/agent.rs`：`render_tool_guide` 可见性应为私有
- `crates/visp-core/src/agent.rs`：`llm_error_to_code` 内部 clone String，可直接消费值
- `crates/visp-core/src/agent.rs`：`AgentConfig` 中 `bash_confirm_mode` 和 `file_max_size_bytes` 在 agent 循环中未使用
- `crates/visp-core/src/prompt.rs`：`user_query_instruction()` 公开函数仅为测试服务

### visp-llm

- ~~`crates/visp-llm/src/anthropic.rs`：Tool 消息缺少 `tool_call_id` 时用空字符串 fallback~~ ✅ `tracing::warn!` → `tracing::error!` 提升可见性
- ~~`crates/visp-llm/src/openai.rs`：`_unused` 变量冗余~~ ✅ 已删除无用行
- ~~`crates/visp-llm/src/openai.rs`：`RESERVED_FIELDS` 包含不必要的 `"name"` 字段~~ ✅ 已移除
- 缺乏 `byte_stream_to_chat_events` 的端到端流测试

### visp-tools

- `crates/visp-tools/src/file.rs`：`validate_write_path` 逻辑过于复杂
- `crates/visp-tools/src/file.rs`：使用 `block_on` 而非 `#[tokio::test]`
- `crates/visp-tools/src/file.rs`：缺少 `validate_write_path` 的独立单元测试
- `crates/visp-tools/src/codegraph.rs`：测试仅验证元数据，不覆盖 `execute()` 路径
- `crates/visp-tools/src/codegraph.rs`：测试中 assert 已删除内容的描述 → ℹ️ 经审查保留：`test_codegraph_search_updated_description` 是有效的回归测试，防止旧文本回退

### visp-codegraph

- `crates/visp-codegraph/src/store.rs`：`insert_symbols` 每次重新 prepare
- `crates/visp-codegraph/src/parser.rs`：`handle_variable_declaration` 冗余去重检查
- `crates/visp-codegraph/src/parser.rs`：`walk_children` 10 个参数，建议重构为 `ParserCtx`
- `crates/visp-codegraph/src/index.rs`：`language_name` 与 `crates/visp-codegraph/src/parser.rs` 的 `lang_str_for_ext` 重复映射
- `crates/visp-codegraph/src/index.rs`：`collect_files` 扩展名检查双重检查
- `crates/visp-codegraph/src/query.rs`：`read_source` 每次重新打开文件
- `crates/visp-codegraph/src/watcher.rs`：使用 `sleep(300ms)` 等待就绪（fragile）
- `crates/visp-codegraph/src/watcher.rs`：`unbounded_channel` 无背压
- `crates/visp-codegraph/src/lib.rs`：`test_index_visp` 写入真实项目数据库
- ~~`crates/visp-codegraph/src/graph.rs`：多个结构体字段读写测试（纯验证编译器行为）~~ ✅ 已删除（4个：`test_symbol_creation`、`test_file_info_creation`、`test_edge_resolved`、`test_edge_unresolved`）
- ~~`crates/visp-codegraph/src/query.rs`：`test_new_accepts_3params` 无运行时断言~~ ✅ 已删除
- ~~`crates/visp-codegraph/src/query.rs`：`test_project_name_tokens_empty` 无实际断言~~ ✅ 已删除

### visp-context

- `crates/visp-context/src/lib.rs:247-253`：`confirmed_tool_ids` 收集时机合适但可加注释
- `crates/visp-context/src/lib.rs:104-106`：硬编码 `4_000` 最小输出预留，小模型时过于激进
- `crates/visp-context/Cargo.toml`：`tempfile` dev-dependency 未使用

### visp-daemon

- `crates/visp-daemon/src/config.rs`：`LlmSection::effective_models()` 仅一行 clone
- `crates/visp-daemon/src/config.rs`：`default_file_max_size()` 同时用于 tools 和 agent（共享需注释说明）
- `crates/visp-daemon/src/main.rs`：`#[allow(dead_code)] mod command` 掩盖未使用代码
- `crates/visp-daemon/src/main.rs`：`model_configs.first().map().unwrap_or()` 可简化为 `map_or`
- `crates/visp-daemon/src/service.rs`：`create_llm_provider` 非 openai 协议静默 fallback 到 anthropic
- `crates/visp-daemon/src/service.rs`：`create_session` 用值比较判断"是否设置"（脆弱）
- `crates/visp-daemon/src/command/init.rs`：同步 IO 在 async fn 中

### visp-cli

- `crates/visp-cli/src/client.rs`：`#![allow(dead_code)]` 模块级掩盖检查
- `crates/visp-cli/src/client.rs`：`recv()` 吞掉传输错误
- `crates/visp-cli/src/app.rs`：`#![allow(dead_code)]` + `#![allow(clippy::bool_assert_comparison)]`
- `crates/visp-cli/src/app.rs`：`insert_tool_result` 中未找到 call 时创建空 name 的 ToolCall
- `crates/visp-cli/src/app.rs`：`result_summary` 只处理 `read_file/read_files`
- `crates/visp-cli/src/event.rs`：Ctrl+C 处理在确认/普通模式中重复
- `crates/visp-cli/src/theme.rs`：`USAGE_STYLE` 已废弃但映射存在
- `crates/visp-cli/src/ui.rs`：状态栏 token 统计固定 36 字符宽度
- `crates/visp-cli/src/ui.rs`：hint 行可能与输入区重叠

### visp (launcher)

- `crates/visp/src/main.rs`：gRPC 客户端连接代码重复（`connect_and_check` vs `send_shutdown`）
- `crates/visp/src/main.rs`：缺少 `--list` 参数透传
- 缺少 `resolve_bin` 和 `format_timestamp` 的单元测试

### visp-db

- `crates/visp-db/src/schema.rs`：`PRAGMA cache_size = -64000` 硬编码 64MB
- `crates/visp-db/src/store.rs`：`Mutex<Connection>` 在 tokio 中阻塞（当前可接受）

### visp-mcp

- `crates/visp-mcp/src/get_client.rs`：`CallToolResponse::is_error` fallback 硬编码 `Some(false)`
- `crates/visp-mcp/src/get_client.rs`：`ContentEntry._type` 字段解析后未使用
- `crates/visp-mcp/src/manager.rs`：`Drop` 中 `tasks.get_mut().unwrap()` 可能 panic
- `crates/visp-mcp/src/manager.rs`：`restart()` 中旧 task 未完全中止时可能新旧共存

### visp-proto

- `crates/visp-proto/proto/visp.proto`：`available_models` 和 `model_names` 两个字段冗余
- `crates/visp-proto/proto/visp.proto`：`UserQuery.options` 空列表表示"审批模式"的约定不明确
- `crates/visp-proto/proto/visp.proto`：`ReadFileResponse` 缺少 `session_id`

---

## 测试质量评估

- **visp-core**（~140+ 测试）— 质量：中。~~有冗余测试（刷覆盖率）~~ ✅ 已清理，核心 agent 逻辑缺少集成测试。
- **visp-llm**（56 测试）— 质量：中。~~大段代码重复 ~~ ✅ 已提取公共函数，缺少 `byte_stream_to_chat_events` 端到端测试。
- **visp-tools**（~30+ 测试）— 质量：中。CodeGraph 工具均无 execute 测试，HTTP fetch 无集成测试。
- **visp-codegraph**（~120+ 测试）— 质量：高。~~测试覆盖较好，但有低价值结构体测试。~~ ✅ 冗余测试已清理
- **visp-context**（~20+ 测试）— 质量：高。孤儿过滤测试充分，缺少省略标记测试。
- **visp-daemon**（35 测试）— 质量：**低**。~~缺少冗余测试~~ ✅ 已清理，缺少 read_file/search_symbols RPC 测试，chat 流测试。
- **visp-cli**（~32 测试）— 质量：中。event.rs 无测试，ui.rs 无测试。
- **visp (launcher)**（6 测试）— 质量：**低**。缺少进程管理/健康检查测试。
- **visp-db**（42 测试）— 质量：高。schema 迁移测试充分，~~但 skip_context 丢失测试未覆盖。~~ ✅ skip_context 持久化测试已补充
- **visp-mcp**（89 测试）— 质量：高。数量多且覆盖全面，少数 CI 脆性测试。~~SSE 多行解析~~ ✅ 已修复
- **visp-proto**（N/A）— 代码生成，无可测内容。

**低质量测试示例**（已全部清理 ✅）：
- ~~`error.rs:test_stuck_in_loop_match`~~ — 已删除
- ~~`error.rs:5 个 test_llmerror_*_display`~~ — 已删除
- ~~`codegraph/graph.rs` 中多个结构体字段读写测试~~ — 已删除
- ~~`codegraph/query.rs:test_new_accepts_3params`~~ — 已删除
- ~~`codegraph/query.rs:test_project_name_tokens_empty`~~ — 已删除
- ~~`config.rs:test_default_max_context_tokens`~~ — 已删除（B23 死代码清理）
- ~~`config.rs:test_default_config_max_context_tokens`~~ — 已删除（B23 死代码清理）

---

## 跨 crate 兼容性总结

- ✅ **`Tool` trait 实现** — 所有工具正确实现，与 core 兼容。
- ✅ **`LlmProvider` trait 实现** — anthropic/openai/mock 正确实现。
- ✅ **`SessionStore` trait 实现** — `SqliteSessionStore` 完整实现。
- ✅ **`ContextTrimmer` trait 实现** — `DefaultContextTrimmer` 正确实现。
- ✅ **gRPC 双向流协议** — proto ↔ daemon ↔ cli 映射正确。
- ❌ **依赖方向（core 不依赖 IO crate）** — core 包含 IO 操作（见 A1）。
- ⚠️ **visp-codegraph 依赖 visp-core** — 在 Cargo.toml 中声明但实际未使用，可安全移除。

---

## 紧急程度排序

### 🔴 尚未修复（P0）
1. **A1** — core 的 IO 违规（架构矛盾，影响后续所有开发）
2. **A4** — ReadFile schema 矛盾（工具行为与声明不一致）

### 🟡 尚未修复（P1）
3. **B27** — read_file session 不存在回退到 `"."`
4. **B28** — search_symbols limit=0 强制变 20
5. **B34** — JSON 序列化失败静默吞错
7. **B35** — `Session.created_at` 硬编码 `Instant::now()`
8. **B36** — `update()` 返回值未检查
9. **B39** — `HttpPostClient` Clone 后 `initialized` 丢失

### 🟢 已修复 ✅

- **A2** — skip_context 持久化（`db/schema` + `message_repo`）：Schema V3，INSERT/SELECT 同步读写
- **A3** — AlreadyExists 错误消息（`core/error.rs`）：错误消息字符串修正
- **A5** — Box::leak 内存泄漏（`cli/app` + `client` + `event`）：`&'static str` → `String`
- **A6** — 搜索评分算法 bug（`codegraph/query.rs`）：`NEG_INFINITY` → `0.0` + 测试
- **B3/B4** — tool_registry 分配+错误（`core/tool_registry` + `daemon/main`）：`Box` → `Arc`，`String` → `CoreError`
- **B7/B8** — anthropic/openai 代码重复（`llm/anthropic` + `llm/openai`）：提取 `update_anthropic_usage`、`build_done_usage_info`、`flush_tool_acc` 三个辅助函数
- **B23** — daemon config 死代码（`daemon/config.rs`）：删除 5 函数 + 冗余测试 + 移除模块级 `#![allow()]`
- **B25** — Ctrl+C MCP 优雅关闭（`daemon/main.rs`）：clone Arc 保留引用，信号处理中调用 `shutdown_all()`
- **B26** — 前缀歧义误导（`daemon/service.rs`）：`not_found` → `invalid_argument`，测试同步更新
- **B29** — 旧命名 VbwClient（`cli/client` + `main` + `event`）：`VbwClient` → `VispClient`
- **B33** — row_to_session 三处重复（`db/session_repo.rs`）：提取公共函数
- **B38** — SSE 多行 data（`mcp/http_client.rs`）：累积拼接 + 测试

### 🟢 长期规划（C 级+剩余 B 级）
其余 B 级（33 项）和 C 级（56 项）可安排在后续迭代中逐步解决。
