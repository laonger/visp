# vibewisp Phase 5 工作计划：CodeGraph 代码智能

## 概述

Phase 5 实现 tree-sitter 代码解析引擎和 SQLite 索引查询系统。创建新 crate vbw-codegraph，扩展 vbw-daemon。

每个子步骤都是一个独立的 TDD 循环：**红 → 绿 → 测试 → 类型检查 → 重构 → 提交**。

---

## 步骤 1：vbw-codegraph 项目骨架 + 依赖

### 🔴 红 — 验证

`cargo build -p vbw-codegraph` 失败（crate 尚不存在）。

### 🟢 绿 — 实现

- 创建 `crates/vbw-codegraph/Cargo.toml`，依赖：`vbw-core`, `tree-sitter`, `tree-sitter-typescript`, `rusqlite` (feature: `bundled`), `walkdir`, `notify`, `tokio`
- 创建 `crates/vbw-codegraph/src/lib.rs`（空模块声明）
- Workspace Cargo.toml 中 `members` 添加 `"crates/vbw-codegraph"`
- Workspace Cargo.toml 中添加 `tree-sitter`、`tree-sitter-typescript`、`rusqlite`、`walkdir` 的 `[workspace.dependencies]`

### 🧪 测试 → 🔍 类型检查

```bash
cargo build -p vbw-codegraph && cargo clippy -p vbw-codegraph -- -D warnings
```

### 📦 提交

```bash
git add crates/vbw-codegraph/ Cargo.toml Cargo.lock
git commit -m "feat(vbw-codegraph): create crate skeleton with dependencies"
```

---

## 步骤 2：核心数据类型（graph.rs）

### 2a：Symbol + SymbolKind

#### 🔴 红 — 测试

| # | 测试用例 |
|---|---|
| 1 | `test_symbol_creation` — 创建 Symbol，各字段可正确读取 |
| 2 | `test_symbol_kind_variants` — SymbolKind 枚举包含 Function/Method/Class/Interface/TypeAlias/Variable/Enum |

#### 🟢 绿 — 实现

- `SymbolKind` 枚举（7 个变体，derive Debug/Clone/PartialEq）
- `Symbol` 结构体：`id: u64`, `name: String`, `kind: SymbolKind`, `file_path: String`, `line: u32`, `column: u32`, `signature: Option<String>`, `docstring: Option<String>`

#### 📦 提交

```bash
git add crates/vbw-codegraph/src/graph.rs
git commit -m "feat(vbw-codegraph): Symbol and SymbolKind types"
```

### 2b：Edge + EdgeKind

#### 🔴 红 — 测试

| # | 测试用例 |
|---|---|
| 1 | `test_edge_resolved` — 已解析边（target_id 有值，target_name 为 None） |
| 2 | `test_edge_unresolved` — 未解析边（target_id 为 None，target_name 有值） |
| 3 | `test_edge_kind_variants` — EdgeKind 枚举包含 Call/Reference/Implementation/Inheritance |

#### 🟢 绿 — 实现

- `EdgeKind` 枚举（4 个变体）
- `Edge` 结构体：`source_id: u64`, `target_id: Option<u64>`, `target_name: Option<String>`, `kind: EdgeKind`

#### 📦 提交

```bash
git add crates/vbw-codegraph/src/graph.rs
git commit -m "feat(vbw-codegraph): Edge and EdgeKind types with unresolved edge support"
```

### 2c：FileInfo

#### 🔴 红 — 测试

| # | 测试用例 |
|---|---|
| 1 | `test_file_info_creation` — FileInfo 各字段可正确读取 |

#### 🟢 绿 — 实现

- `FileInfo` 结构体：`path: String`, `language: String`, `symbol_count: u32`, `last_indexed_at: u64`

#### 📦 提交

```bash
git add crates/vbw-codegraph/src/graph.rs
git commit -m "feat(vbw-codegraph): FileInfo type for indexed file metadata"
```

---

## 步骤 3：SQLite 存储层（store.rs）

### 3a：Schema + 建表

#### 🔴 红 — 测试

| # | 测试用例 |
|---|---|
| 1 | `test_init_creates_tables` — 初始化数据库后五张表都存在 |
| 2 | `test_init_idempotent` — 重复初始化不报错 |

#### 🟢 绿 — 实现

- `Store` 结构体：持有 `Arc<Mutex<Connection>>`
- `Store::open(db_path: &Path) -> Result<Self>` — 打开/创建数据库，自动创建目录
- 建表 SQL（symbols, edges, files, imports, exports），`PRAGMA foreign_keys = ON`，`PRAGMA journal_mode = WAL`

#### 📦 提交

```bash
git add crates/vbw-codegraph/src/store.rs crates/vbw-codegraph/src/lib.rs
git commit -m "feat(vbw-codegraph): Store with SQLite schema, WAL mode, and foreign keys"
```

### 3b：CRUD 操作

#### 🔴 红 — 测试

| # | 测试用例 |
|---|---|
| 1 | `test_insert_symbols` — 批量插入符号，验证 id 回填 |
| 2 | `test_insert_replace_symbols` — UNIQUE 约束下 INSERT OR REPLACE 保留原 id |
| 3 | `test_delete_by_file` — 按文件删除符号，级联删除关联边和导出 |
| 4 | `test_insert_edge_resolved` — 插入已解析边 |
| 5 | `test_insert_edge_unresolved` — 插入未解析边（target_id=NULL, target_name="foo"） |
| 6 | `test_search_symbols` — 前缀搜索返回正确结果 |
| 7 | `test_get_callers` — 查询某符号的调用者 |
| 8 | `test_get_callees` — 查询某符号的被调用者 |
| 9 | `test_resolve_edges` — 将未解析边批量更新为目标 ID |
| 10 | `test_insert_exports_regular` — 写入常规导出映射 |
| 11 | `test_insert_exports_re_export` — 写入 re-export 映射 |

#### 🟢 绿 — 实现

所有 CRUD 方法：
- `insert_symbols(symbols: &mut [Symbol])` — 批量插入，回填 id
- `delete_by_file(path: &str)` — 按文件删除 symbols + edges + imports + exports
- `insert_edges(edges: &[Edge])`
- `insert_imports(file_path: &str, imports: &[(local_name, import_source)])`
- `insert_exports(file_path: &str, exports: &[(export_name, symbol_id, re_export_source)])`
- `search_symbols(prefix: &str, limit: usize) -> Vec<Symbol>`
- `get_symbols_by_name(name: &str) -> Vec<Symbol>`
- `get_callers(symbol_id: u64) -> Vec<(String, String)>` — 返回 (名称, 文件路径)
- `get_callees(symbol_id: u64) -> Vec<(String, String)>`
- `get_unresolved_edges() -> Vec<Edge>` — 获取所有未解析边
- `resolve_edge(edge_id: u64, target_id: u64)` — 解析单条边
- `upsert_file(path, language, count)`
- `delete_file_record(path)`
- `list_indexed_files() -> Vec<String>`

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p vbw-codegraph && cargo clippy -p vbw-codegraph -- -D warnings
```

#### 📦 提交

```bash
git add crates/vbw-codegraph/src/store.rs
git commit -m "feat(vbw-codegraph): Store CRUD operations for symbols, edges, imports, exports"
```

---

## 步骤 4：tree-sitter 解析器（parser.rs）

### 🔴 红 — 测试

创建测试用的 TypeScript 代码片段，验证解析结果。

| # | 测试用例 |
|---|---|
| 1 | `test_parse_function_declaration` — `function foo() {}` → Symbol(foo, Function) |
| 2 | `test_parse_class_declaration` — `class Bar {}` → Symbol(Bar, Class) |
| 3 | `test_parse_variable_declaration` — `const x = 1` → Symbol(x, Variable) |
| 4 | `test_parse_arrow_function` — `const fn = () => {}` → Symbol(fn, Function)，且不与 Variable 重复 |
| 5 | `test_parse_method` — `class A { m() {} }` → Symbol(m, Method) |
| 6 | `test_parse_interface` — `interface I {}` → Symbol(I, Interface) |
| 7 | `test_parse_type_alias` — `type T = string` → Symbol(T, TypeAlias) |
| 8 | `test_parse_enum` — `enum E {}` → Symbol(E, Enum) |
| 9 | `test_parse_call_edge` — `foo()` 创建 Edge(Call, target_name="foo")，标记为未解析 |
| 10 | `test_parse_import_statement` — `import { bar } from './other'` → 记录 imports 映射 |
| 11 | `test_parse_export_statement` — `export function baz() {}` → 记录 exports 映射（baz → 本地符号） |
| 12 | `test_parse_re_export` — `export { qux } from './other'` → 记录 re-export（qux → re_export_source） |
| 13 | `test_parse_inheritance` — `class D extends E {}` → Edge(Inheritance) |
| 14 | `test_parse_syntax_error` — 含语法错误的文件返回 Err，不 panic |
| 15 | `test_parse_empty_file` — 空文件返回空 symbols/edges/imports/exports |

### 🟢 绿 — 实现

- `Parser` 结构体：持有 `tree_sitter::Parser`，构造时预加载语法库
- `Parser::new() -> Result<Self>` — 初始化 parser + 加载 typescript 语法
- `Parser::parse_file(path: &str, content: &str) -> Result<ParseResult>`
- `ParseResult`：`symbols: Vec<Symbol>`, `edges: Vec<Edge>`, `imports: Vec<(String, String)>`, `exports: Vec<(String, Option<u64>, Option<String>)>`
- 解析器内部维护 (file_path, name) 去重集合
- 遍历 AST 树，按符号提取规则和关系提取规则创建符号和边

### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p vbw-codegraph && cargo clippy -p vbw-codegraph -- -D warnings
```

### 📦 提交

```bash
git add crates/vbw-codegraph/src/parser.rs
git commit -m "feat(vbw-codegraph): tree-sitter parser for TypeScript symbol and edge extraction"
```

---

## 步骤 5：索引构建（index.rs）

### 5a：全量构建

#### 🔴 红 — 测试

创建临时 TypeScript 项目目录（含多个 .ts 文件），执行全量构建后验证：

| # | 测试用例 |
|---|---|
| 1 | `test_full_build_basic` — 3 个 .ts 文件，构建后 symbols 总数正确 |
| 2 | `test_full_build_excludes_dirs` — node_modules 中的文件不被索引 |
| 3 | `test_full_build_cross_file_resolution` — 文件 A 导出 foo，文件 B 导入并调用 foo，验证 B 中的 Edge 被解析为指向 A 的 foo |
| 4 | `test_full_build_skip_parse_error` — 包含语法错误的文件被跳过，其他文件正常索引 |

#### 🟢 绿 — 实现

- `Indexer` 结构体：持有 `Arc<Store>`, `Parser`
- `build_full(project_path: &Path, config: &CodeGraphConfig) -> Result<()>`
  - 使用 walkdir 遍历项目目录
  - 收集支持的文件，排除配置的目录
  - 逐文件解析 + 写入 store
  - 跨文件解析：遍历未解析边 → exports 表匹配 → resolve
- `CodeGraphConfig`：`exclude_dirs: Vec<String>`, `supported_extensions: Vec<String>`

#### 📦 提交

```bash
git add crates/vbw-codegraph/src/index.rs crates/vbw-codegraph/src/lib.rs
git commit -m "feat(vbw-codegraph): full index build with cross-file resolution"
```

### 5b：增量更新

#### 🔴 红 — 测试

| # | 测试用例 |
|---|---|
| 1 | `test_incremental_add_file` — 新增文件后索引追加，不影响已有符号 |
| 2 | `test_incremental_modify_file` — 修改文件后旧符号替换为新符号（UNIQUE 保证边不断裂） |
| 3 | `test_incremental_delete_file` — 删除文件后 symbols + edges + imports + exports 清除 |

#### 🟢 绿 — 实现

- `update_file(project_path: &Path, file_path: &Path, event: FileEvent) -> Result<()>`
- `FileEvent` 枚举：Created, Modified, Removed
- Created/Modified：事务内 delete_by_file → parse → 写入 → 全量跨文件解析
- Removed：事务内 delete_by_file + delete_file_record

#### 📦 提交

```bash
git add crates/vbw-codegraph/src/index.rs
git commit -m "feat(vbw-codegraph): incremental index update for file create/modify/delete"
```

---

## 步骤 6：查询引擎（query.rs）

### 🔴 红 — 测试

| # | 测试用例 |
|---|---|
| 1 | `test_search_prefix` — 搜索 "get" 返回 "getUser", "getName" 等 |
| 2 | `test_search_no_match` — 搜索 "zzz" 返回空列表 |
| 3 | `test_search_case_sensitive` — 搜索 "Get" 不匹配 "get" |
| 4 | `test_get_details_single` — 查询单个符号返回正确 callers/callees |
| 5 | `test_get_details_multiple` — 同名符号在不同文件，每个都有 file_path |
| 6 | `test_get_details_caller_format` — callers 返回格式 `"bar (src/utils.ts)"` |
| 7 | `test_search_during_build` — 构建中查询返回错误消息 |

### 🟢 绿 — 实现

- `QueryEngine` 结构体：持有 `Arc<Store>`
- `search(query: &str, limit: usize) -> Result<Vec<SymbolInfo>>`
- `get_details(name: &str) -> Result<Vec<SymbolDetails>>`
- 查询时过滤未解析边（`target_id IS NOT NULL`）
- 构建中标志位检查

### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p vbw-codegraph && cargo clippy -p vbw-codegraph -- -D warnings
```

### 📦 提交

```bash
git add crates/vbw-codegraph/src/query.rs crates/vbw-codegraph/src/lib.rs
git commit -m "feat(vbw-codegraph): query engine with prefix search and symbol details"
```

---

## 步骤 7：文件监听（watcher.rs）

### 🔴 红 — 测试

| # | 测试用例 |
|---|---|
| 1 | `test_watcher_file_create` — 创建临时 .ts 文件，验证增量索引被触发 |
| 2 | `test_watcher_file_modify` — 修改文件内容，验证增量索引被触发 |
| 3 | `test_watcher_debounce` — 快速连续两次修改，只触发一次索引更新 |
| 4 | `test_watcher_excludes_dirs` — node_modules 变更不触发索引 |
| 5 | `test_watcher_unsupported_extension` — 修改 .json 文件不触发索引 |

### 🟢 绿 — 实现

- `Watcher` 结构体：持有 `notify` watcher 句柄
- `Watcher::start(project_path, indexer: Arc<Indexer>, config) -> Result<Self>`
  - 递归监听项目目录
  - 防抖 500ms
  - 文件类型过滤
  - 目录排除
  - 事件映射（Created/Modified → indexer.update_file, Removed → indexer.update_file）
- `Watcher::stop(self)` — 停止监听

### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p vbw-codegraph && cargo clippy -p vbw-codegraph -- -D warnings
```

### 📦 提交

```bash
git add crates/vbw-codegraph/src/watcher.rs crates/vbw-codegraph/src/lib.rs
git commit -m "feat(vbw-codegraph): file watcher with debounce and incremental index triggers"
```

---

## 步骤 8：CodeGraph 主结构体（lib.rs）

### 🔴 红 — 测试

| # | 测试用例 |
|---|---|
| 1 | `test_codegraph_open` — open 创建数据库，目录自动创建 |
| 2 | `test_codegraph_build_and_search` — build_full → search 返回结果 |
| 3 | `test_codegraph_start_watching` — 调用 start_watching 不报错 |
| 4 | `test_codegraph_shutdown` — shutdown 清理资源 |

### 🟢 绿 — 实现

- `CodeGraph` 结构体：`store: Arc<Store>`, `watcher: Option<Watcher>`, `is_building: AtomicBool`
- `CodeGraph::open(project_path: &Path) -> Result<Self>`
- `CodeGraph::build_full(&self) -> Result<()>`
- `CodeGraph::start_watching(&mut self, indexer: Arc<Indexer>, config) -> Result<()>`
- `CodeGraph::search(&self, query, limit) -> Result<Vec<SymbolInfo>>`
- `CodeGraph::get_details(&self, name) -> Result<Vec<SymbolDetails>>`
- `CodeGraph::shutdown(self) -> Result<()>`

### 📦 提交

```bash
git add crates/vbw-codegraph/src/lib.rs
git commit -m "feat(vbw-codegraph): CodeGraph main struct with lifecycle management"
```

---

## 步骤 9：vbw-daemon 集成

### 🔴 红 — 测试

| # | 测试用例 |
|---|---|
| 1 | `test_search_symbols_unimplemented_when_disabled` — CodeGraph 未启用时返回 unimplemented |
| 2 | `test_search_symbols_returns_results` — 使用预构建测试数据库，SearchSymbols 返回正确结果 |
| 3 | `test_get_symbol_details_returns_details` — GetSymbolDetails 返回正确的 callers/callees |

### 🟢 绿 — 实现

- vbw-daemon Cargo.toml 添加 vbw-codegraph 依赖
- `CoderDaemonService` 新增 `codegraphs: Arc<RwLock<HashMap<String, Arc<CodeGraph>>>>` 字段
- `SearchSymbols` 实现：根据 project_path 懒加载获取/创建 CodeGraph → `codegraph.search(query, limit)`
- `GetSymbolDetails` 实现：同上 → `codegraph.get_details(name)`
- daemon config 新增 `[codegraph]` section
- daemon shutdown 遍历所有 CodeGraph 实例逐个关闭

### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p vbw-daemon && cargo clippy -p vbw-daemon -- -D warnings
```

### 📦 提交

```bash
git add crates/vbw-daemon/
git commit -m "feat(vbw-daemon): CodeGraph integration for SearchSymbols and GetSymbolDetails"
```

---

## 步骤 10：全 Workspace 质量门

```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --check --all
```

全部通过后 Phase 5 完成。

---

## Wave 并行策略

### Wave 1：项目骨架 + 核心类型（1 个 Agent，串行）

```
Agent A: 步骤 1 → 2a → 2b → 2c
```

创建 crate 骨架 + 定义所有核心数据类型。后续所有模块依赖这些类型。

### Wave 2：SQLite 存储 + 解析器（2 个 Agent，并行）

```
Agent A: 3a → 3b (store.rs)
Agent B: 步骤 4 (parser.rs)
```

两个模块独立开发，仅依赖 graph.rs 的类型定义。

### Wave 3：索引构建 + 查询引擎（2 个 Agent，并行）

```
Agent A: 5a → 5b (index.rs)
Agent B: 步骤 6 (query.rs)
```

Index 依赖 store + parser，Query 依赖 store。两者互不依赖，可并行。

### Wave 4：文件监听 + 主结构体（2 个 Agent，并行）

```
Agent A: 步骤 7 (watcher.rs)
Agent B: 步骤 8 (lib.rs)
```

Watcher 依赖 index，lib.rs 依赖所有模块。两者可并行编写。

### Wave 5：vbw-daemon 集成（1 个 Agent）

```
Agent A: 步骤 9
```

### Wave 6：质量门

```
cargo test --workspace && cargo clippy --workspace -- -D warnings && cargo fmt --check --all
```

---

## 依赖关系总览

```
Wave 1:  步骤1 → 2a → 2b → 2c                        [1 Agent, 串行]
                │
        ┌───────┴───────┐
Wave 2:  3a→3b            步骤4                         [2 Agent, 并行]
        │                 │
        └───┬─────────────┘
            │
    ┌───────┴───────┐
Wave 3: 5a→5b        步骤6                             [2 Agent, 并行]
    │                 │
    └───┬─────────────┘
        │
    ┌───┴───┐
Wave 4: 步骤7  步骤8                                    [2 Agent, 并行]
    │       │
    └───┬───┘
        │
Wave 5: 步骤9                                          [1 Agent]
        │
Wave 6: 质量门                                         [全 workspace]
```

---

## 测试覆盖汇总

| Wave | 并行数 | Crate | 步骤 | 测试用例 |
|---|---|---|---|---|
| 1 | 1 | vbw-codegraph | 步骤 1~2c (4) | 5 |
| 2 | 2 | vbw-codegraph | 步骤 3~4 (3) | 26 |
| 3 | 2 | vbw-codegraph | 步骤 5~6 (3) | 11 |
| 4 | 2 | vbw-codegraph | 步骤 7~8 (2) | 9 |
| 5 | 1 | vbw-daemon | 步骤 9 (1) | 3 |
| 6 | — | 全 workspace | 质量门 | — |

总计：**10 个主步骤，17 个子步骤，54 个测试用例，最多 2 Agent 并行**。

## 备注

- Parser 测试需要真实的 TypeScript 代码片段（用字符串常量嵌入测试文件）
- Store 测试使用临时 SQLite 文件（`:memory:` 或 tempfile）
- Index 测试需要创建临时目录结构（用 tempfile 或测试 fixtures）
- Watcher 测试依赖 notify 的实际文件系统事件（需要短暂 sleep 等待）
- 跨文件解析集成测试需要多文件临时项目
- tree-sitter-typescript 语法库首次编译可能较慢
- vbw-codegraph 不依赖 vbw-llm 或 vbw-tools，可完全独立开发
