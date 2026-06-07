# CodeGraph 工具设计

## 1. 设计原则

CodeGraphSearch 和 CodeGraphGetDetails 是**普通的 Tool**，和 Bash、ReadFile、Grep 一样——自包含，**不修改任何核心数据结构**。

工具从 `ToolContext.working_dir` 获取项目路径，直接打开 `.vibewisp/codegraph.db` 查询。

## 2. 数据流

### 2.1 codegraph_search

```
Agent 调用 → ToolRegistry.execute("codegraph_search", {query, limit})
  │
  ▼
CodeGraphSearch::execute(args, context: ToolContext)
  │
  ├─ 提取参数: query, limit (default 20)
  │
  ├─ db_path = context.working_dir / ".vibewisp" / "codegraph.db"
  │   └─ 不存在 → 返回 "CodeGraph not initialized"
  │
  ├─ cg = CodeGraph::open(context.working_dir)
  │
  ├─ results = cg.search(query, limit)
  │     │
  │     └─ QueryEngine::search(query, limit)
  │           └─ Store::search_symbols(query, limit)
  │                 └─ SQLite (LIKE / FTS5)
  │                       └─ Vec<SymbolInfo>
  │
  └─ 格式化结果为文本返回
```

**返回文本示例：**
```
src/main.ts:42  function  hello  (name: string): string
src/parser.rs:120  class  Parser
src/types.ts:8  interface  Config
```

### 2.2 codegraph_get_details

```
Agent 调用 → ToolRegistry.execute("codegraph_get_details", {name})
  │
  ▼
CodeGraphGetDetails::execute(args, context: ToolContext)
  │
  ├─ 提取参数: name
  │
  ├─ db_path = context.working_dir / ".vibewisp" / "codegraph.db"
  │   └─ 不存在 → 返回 "CodeGraph not initialized"
  │
  ├─ cg = CodeGraph::open(context.working_dir)
  │
  ├─ results = cg.get_details(name)
  │     │
  │     └─ QueryEngine::get_details(name)
  │           └─ Store (SQLite)
  │                 └─ Vec<SymbolDetails>
  │
  └─ 格式化结果为文本返回
```

**返回文本示例：**
```
src/parser.rs:120  class  Parser
  signature: fn new() -> Result<Self, Box<dyn Error>>
  doc: Creates a new parser with TypeScript language
  callers: setup, test_parse
  callees: TsParser::new, parser.set_language
```

## 3. 完整调用链路

```
┌──────────────────────────────────────────────────────────────────┐
│  Daemon                                                          │
│  ┌──────────────────────────────────────────────────────────┐   │
│  │  chat handler                                              │   │
│  │  run_agent_loop(provider, tool_registry, ctx, msg, tx)     │   │
│  │       │                                                    │   │
│  │       ▼                                                    │   │
│  │  AgentLoop: 迭代直到 max_iterations 或无 tool call          │   │
│  │       │                                                    │   │
│  │       ├─ provider.chat_stream() → ChatEvent 流             │   │
│  │       ├─ 收集 ToolCallRequest                              │   │
│  │       └─ 执行工具:                                         │   │
│  │            let tool_ctx = ToolContext {                     │   │
│  │                working_dir: ctx.working_dir.clone(),        │   │
│  │                session_id: Some(ctx.session_id),            │   │
│  │            };                                               │   │
│  │            registry.execute(&name, args, &tool_ctx)         │   │
│  │                 │                                           │   │
│  └─────────────────│─────────────────────────────────────────┘   │
│                    ▼                                             │
│  ┌──────────────────────────────────────────────────────────┐   │
│  │  codegraph_search / codegraph_get_details                 │   │
│  │                                                           │   │
│  │  execute(args, ToolContext { working_dir, session_id })   │   │
│  │       │                                                   │   │
│  │       ▼                                                   │   │
│  │  CodeGraph::open(&working_dir)                            │   │
│  │       │                                                   │   │
│  │       ▼                                                   │   │
│  │  .vibewisp/codegraph.db  (SQLite WAL + FTS5)              │   │
│  │       │                                                   │   │
│  │       ├─ search(query, limit)                              │   │
│  │       └─ get_details(name)                                 │   │
│  └──────────────────────────────────────────────────────────┘   │
└──────────────────────────────────────────────────────────────────┘
```

## 4. 核心代码

```rust
// crates/vbw-tools/src/codegraph.rs（简化）
async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
    let query = args["query"].as_str()?;
    let limit = args["limit"].as_i64().unwrap_or(20) as usize;

    let db = ctx.working_dir.join(".vibewisp").join("codegraph.db");
    if !db.exists() {
        return ToolResult::error("CodeGraph not initialized. Run `vbw init`.");
    }

    let cg = match vbw_codegraph::CodeGraph::open(&ctx.working_dir) {
        Ok(cg) => cg,
        Err(e) => return ToolResult::error(format!("CodeGraph open failed: {e}")),
    };

    match cg.search(query, limit) {
        Ok(results) => ToolResult::success(format_results(results)),
        Err(e) => ToolResult::error(e),
    }
}
```

## 5. 文件变动

| 文件 | 改动 |
|------|------|
| `vbw-tools/Cargo.toml` | 新增 `vbw-codegraph` 依赖 |
| `vbw-tools/src/codegraph.rs` | **新增**：`CodeGraphSearch` + `CodeGraphGetDetails` |
| `vbw-tools/src/lib.rs` | 新增 `pub mod codegraph` |
| `vbw-daemon/src/main.rs` | 注册两个工具 |

**核心层零改动**：`ToolContext`、`AgentLoopContext`、`session.rs`、`lib.rs` 均未修改。

## 6. 工具定义

### codegraph_search

| 字段 | 值 |
|------|-----|
| name | `codegraph_search` |
| 描述 | 按名称搜索项目代码中的符号，返回文件路径、行号、类型和签名 |
| 参数 query | string, 必填 |
| 参数 limit | integer, 可选, 默认 20 |

### codegraph_get_details

| 字段 | 值 |
|------|-----|
| name | `codegraph_get_details` |
| 描述 | 获取符号详细信息，包括签名、文档注释、调用者和被调用者 |
| 参数 name | string, 必填 |

## 7. 数据结构

```rust
// vbw-codegraph 已有
struct SymbolInfo {
    name: String,
    kind: String,       // function / class / interface / variable ...
    file_path: String,
    line: u32,
    column: u32,
    signature: Option<String>,
}

struct SymbolDetails {
    // SymbolInfo 字段 +
    docstring: Option<String>,
    source: String,
    callers: Vec<String>,
    callees: Vec<String>,
}
```

## 8. 测试策略

| 层级 | 方式 |
|------|------|
| 工具逻辑 | mock working_dir 指向测试项目目录 |
| SQLite 查询 | 沿用 `vbw-codegraph` 已有测试 |
| 集成 | 启动 daemon，在有 codegraph.db 的项目中通过 agent 调用 |
