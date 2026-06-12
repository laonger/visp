# CodeGraph 搜索召回优化 — 实施计划

## 概述

对 visp-codegraph 的搜索层和解析层做系统性优化，覆盖搜索召回提升（FTS5 + LIKE + 评分排序）和索引质量修复（dedup bug + 多语言 node kind）。

涉及 3 个 crate 文件，分 3 个 Wave。

## Wave 并行策略

```
Wave 1（搜索层·可并行）
├─ 1a: FTS5 schema + 辅助索引            ← store.rs
├─ 1b: search_fts — FTS5 BM25 查询       ← store.rs
├─ 1c: search_like — LIKE 子串查询       ← store.rs
└─ 1d: FTS5 存量回填                     ← store.rs

Wave 2（查询引擎·依赖 Wave 1）
├─ 2a: 搜索编排 + project_name_tokens    ← query.rs + lib.rs
└─ 2b: 评分排序（score_and_sort）        ← query.rs

Wave 3（解析器修复·可并行于 Wave 1/2）
├─ 3a: dedup 去重修复                    ← parser.rs
└─ 3b: 多语言 node kind 覆盖率           ← parser.rs
```
## 步骤 1a：FTS5 schema + 辅助索引（store.rs）

### 🔴 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 1a.1 | fts_table_exists | `init_schema` 后 `symbols_fts` 虚拟表存在，可通过 `SELECT count(*) FROM sqlite_master WHERE type='virtual_table' AND name='symbols_fts'` 验证 |
| 1a.2 | fts_triggers_exist | `init_schema` 后 `symbols_ai`、`symbols_ad`、`symbols_au` 三个触发器存在 |
| 1a.3 | fts_backfill_on_insert | 插入 symbol 后，`symbols_fts` 中能查到对应行（触发器自动同步） |
| 1a.4 | fts_backfill_on_update | 更新 symbol 后，`symbols_fts` 中对应行同步更新 |
| 1a.5 | fts_backfill_on_delete | 删除 symbol 后，`symbols_fts` 中对应行同步删除 |
| 1a.6 | idx_kind_exists | `init_schema` 后 `idx_symbols_kind` 索引存在 |
| 1a.7 | idx_lower_name_exists | `init_schema` 后 `idx_symbols_lower_name` 索引存在 |
| 1a.8 | schema_idempotent | 多次调用 `init_schema` 不报错 |

### 🟢 绿 — 实现

在 `Store::init_schema` 的 `execute_batch` 中的末尾追加：

```sql
CREATE VIRTUAL TABLE IF NOT EXISTS symbols_fts USING fts5(
    id UNINDEXED,
    name,
    kind UNINDEXED,
    signature,
    docstring,
    content='symbols',
    content_rowid='rowid'
);

CREATE TRIGGER IF NOT EXISTS symbols_ai AFTER INSERT ON symbols BEGIN
    INSERT INTO symbols_fts(rowid, id, name, kind, signature, docstring)
    VALUES (NEW.rowid, NEW.id, NEW.name, NEW.kind, NEW.signature, NEW.docstring);
END;

CREATE TRIGGER IF NOT EXISTS symbols_ad AFTER DELETE ON symbols BEGIN
    INSERT INTO symbols_fts(symbols_fts, rowid, id, name, kind, signature, docstring)
    VALUES ('delete', OLD.rowid, OLD.id, OLD.name, OLD.kind, OLD.signature, OLD.docstring);
END;

CREATE TRIGGER IF NOT EXISTS symbols_au AFTER UPDATE ON symbols BEGIN
    INSERT INTO symbols_fts(symbols_fts, rowid, id, name, kind, signature, docstring)
    VALUES ('delete', OLD.rowid, OLD.id, OLD.name, OLD.kind, OLD.signature, OLD.docstring);
    INSERT INTO symbols_fts(rowid, id, name, kind, signature, docstring)
    VALUES (NEW.rowid, NEW.id, NEW.name, NEW.kind, NEW.signature, NEW.docstring);
END;

CREATE INDEX IF NOT EXISTS idx_symbols_kind ON symbols(kind);
CREATE INDEX IF NOT EXISTS idx_symbols_lower_name ON symbols(LOWER(name));
```

### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-codegraph
cargo clippy -p visp-codegraph -- -D warnings
```

### 📦 提交

```
feat(codegraph): add FTS5 virtual table and triggers for full-text search
```
---
## 步骤 1b：search_fts — FTS5 BM25 查询（store.rs）

### 🔴 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 1b.1 | fts_query_basic | `search_fts("get", 10)` 返回 `getUser`，`getData`（需先插入 symbol 并回填 FTS） |
| 1b.2 | fts_query_ranking | `"get"` 搜索：`getUser`（精确匹配）排名高于 `getUserProfile`（前缀匹配） |
| 1b.3 | fts_query_no_match | 无匹配时返回空 Vec |
| 1b.4 | fts_sanitize_special_chars | `"fn*()"` 不 panic，安全处理后返回结果 |
| 1b.5 | fts_sanitize_boolean_ops | `"NOT AND OR NEAR bar"` 安全处理，返回 `bar` 相关结果 |
| 1b.6 | fts_query_limit | `limit=2` 时返回最多 2 条 |

### 🟢 绿 — 实现

在 `Store` 上新增方法：

```rust
/// 返回带 BM25 分数的 symbol。fts_query 需已转义（由调用方处理）。
pub fn search_fts(&self, fts_query: &str, limit: usize) -> rusqlite::Result<Vec<ScoredSymbol>> {
    let conn = self.conn.lock().unwrap();
    let mut stmt = conn.prepare(
        "SELECT symbols.*, bm25(symbols_fts, 0, 20, 0, 5, 1) as score
         FROM symbols_fts
         JOIN symbols ON symbols_fts.rowid = symbols.rowid
         WHERE symbols_fts MATCH ?1
         ORDER BY score DESC
         LIMIT ?2"
    )?;
    // ... 映射到 ScoredSymbol
}
```

同时新增 `sanitize_fts_query` 模块级函数（Rust 函数，非方法）：

```rust
pub fn sanitize_fts_query(query: &str) -> String {
    query
        .replace("::", " ")
        .replace(['\'', '"', '*', '(', ')', ':', '^'], "")
        .split_whitespace()
        .filter(|t| !["AND", "OR", "NOT", "NEAR"].contains(&t.to_uppercase().as_str()))
        .map(|t| format!("\"{}\"*", t))
        .collect::<Vec<_>>()
        .join(" OR ")
}
```

新增 `ScoredSymbol` 类型：

```rust
#[derive(Debug, Clone)]
pub struct ScoredSymbol {
    pub symbol: Symbol,
    pub score: f64,
}
```

### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-codegraph
cargo clippy -p visp-codegraph -- -D warnings
```

### 📦 提交

```
feat(codegraph): add search_fts method with BM25 scoring
```
---
## 步骤 1c：search_like — LIKE 子串查询（store.rs）

### 🔴 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 1c.1 | like_substring | `"get"` 返回 `my_getter`、`getter`、`getUser` |
| 1c.2 | like_case_insensitive | `"getuser"` 返回 `getUser` |
| 1c.3 | like_search_signature | `"i32"` 返回签名含 `-> i32` 的 symbol |
| 1c.4 | like_escape_wildcard | `"foo_bar"` 不匹配 `fooXbar`（`_` 被转义） |
| 1c.5 | like_escape_percent | `"100%"` 不匹配 `100anything`（`%` 被转义） |
| 1c.6 | like_empty_query | 空字符串匹配所有行 |
| 1c.7 | like_no_match | 无匹配时返回空 Vec |
| 1c.8 | like_limit | `limit=2` 时返回最多 2 条 |

### 🟢 绿 — 实现

```rust
/// 支持大小写不敏感的子串匹配，内部自动转义 LIKE 通配符。
pub fn search_like(&self, query: &str, limit: usize) -> rusqlite::Result<Vec<Symbol>> {
    let conn = self.conn.lock().unwrap();
    let safe = query.replace("%", "\\%").replace("_", "\\_");
    let pattern = format!("%{}%", safe);
    let mut stmt = conn.prepare(
        "SELECT id, name, kind, file_path, line, column, signature, docstring
         FROM symbols
         WHERE LOWER(name) LIKE ?1 OR LOWER(signature) LIKE ?1
         LIMIT ?2"
    )?;
    // ...
}
```

### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-codegraph
cargo clippy -p visp-codegraph -- -D warnings
```

### 📦 提交

```
feat(codegraph): add search_like method for substring fallback
```
---
## 步骤 1d：FTS5 存量回填（store.rs）

### 🔴 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 1d.1 | backfill_existing_data | 先插入一批 symbol 再回填，FTS5 能搜到它们 |
| 1d.2 | backfill_idempotent | 回填后再次回填不报错，不产生重复行 |

### 🟢 绿 — 实现

```rust
pub fn backfill_fts(&self) -> rusqlite::Result<()> {
    let conn = self.conn.lock().unwrap();
    conn.execute_batch(
        "INSERT OR IGNORE INTO symbols_fts(rowid, id, name, kind, signature, docstring)
         SELECT rowid, id, name, kind, signature, docstring FROM symbols;"
    )?;
    Ok(())
}
```

### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-codegraph
cargo clippy -p visp-codegraph -- -D warnings
```

### 📦 提交

```
feat(codegraph): add backfill_fts for existing symbol data
```
---
## 步骤 2a：搜索编排 + project_name_tokens（query.rs + lib.rs）

### 🔴 红 — 测试

#### QueryEngine 新增 project_name_tokens 字段

| # | 测试用例 | 描述 |
|---|---------|------|
| 2a.1 | new_accepts_3params | `QueryEngine::new(store, is_building, HashSet::new())` 编译通过 |
| 2a.2 | empty_tokens_unchanged | 空 `project_name_tokens` 时，已有搜索行为完全不变 |
| 2a.3 | tokens_affect_path_score | 设置 `{"visp"}` 后，路径 `visp-core/src/lib.rs` 中的 `visp` 不贡献 path_score |

#### 搜索编排

| # | 测试用例 | 描述 |
|---|---------|------|
| 2a.4 | search_empty_query | `search("", 10)` 返回最多 10 条结果（LIKE `%%`） |
| 2a.5 | search_fts_only | FTS5 结果 ≥ limit 时，不触发 LIKE |
| 2a.6 | search_fts_fallback_to_like | FTS5 结果 < limit 时，触发 LIKE 补充 |
| 2a.7 | search_merge_dedup | FTS5 和 LIKE 结果按 `(name, file_path)` 去重合并 |
| 2a.8 | search_inject_exact | 精确名 `getUser` 始终在结果中，即使 FTS5/LIKE 未命中 |
| 2a.9 | search_truncate | 最终结果不超过 `limit` |
| 2a.10 | search_building | 索引构建中返回 `Err("codegraph: index is building...")` |

#### inject_exact

| # | 测试用例 | 描述 |
|---|---------|------|
| 2a.11 | inject_missing_exact | 精确名不在结果中时被注入 |
| 2a.12 | inject_skip_existing | 精确名已在结果中时不重复注入 |
| 2a.13 | inject_borrows_max_fts | 注入的 symbol 借用传入的 `max_fts_score` |

#### derive_project_name_tokens（lib.rs）

| # | 测试用例 | 描述 |
|---|---------|------|
| 2a.14 | from_cargo_toml | 有 Cargo.toml 且含 package name `visp`，token 集合含 `"visp"` |
| 2a.15 | from_dir_name | 无 Cargo.toml 时回退到目录名 |
| 2a.16 | short_name_skipped | 目录名 `"api"`（4 字符）不加入集合 |
| 2a.17 | open_injects_tokens | `CodeGraph::open` 调用 `derive_project_name_tokens` 并传入 `QueryEngine` |

### 🟢 绿 — 实现

**QueryEngine 结构变更**：

```rust
pub struct QueryEngine {
    store: Arc<Store>,
    is_building: Arc<AtomicBool>,
    project_name_tokens: HashSet<String>,
}

impl QueryEngine {
    pub fn new(
        store: Arc<Store>,
        is_building: Arc<AtomicBool>,
        project_name_tokens: HashSet<String>,
    ) -> Self {
        QueryEngine { store, is_building, project_name_tokens }
    }
}
```

**搜索编排**（`search` 方法重写，设计文档 2.4 节伪代码）：

```rust
pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SymbolInfo>, String> {
    if self.is_building.load(Ordering::Relaxed) {
        return Err("codegraph: index is building, please retry later".into());
    }
    let mut results: Vec<ScoredSymbol> = Vec::new();

    if !query.is_empty() {
        let fts_query = sanitize_fts_query(query);
        results = self.store.search_fts(&fts_query, limit * 5)?;
    }

    let max_fts = results.iter().map(|r| r.score).fold(0.0, f64::max);
    if results.len() < limit {
        let like_results = self.store.search_like(query, limit)?;
        merge_dedup(&mut results, like_results, max_fts);
    }

    inject_exact(&mut results, query, &self.store, max_fts);
    score_and_sort(&mut results, query, &self.project_name_tokens);
    results.truncate(limit);

    Ok(results.into_iter().map(|rs| sym_to_info(rs.symbol)).collect())
}
```

**merge_dedup**：

```rust
fn merge_dedup(results: &mut Vec<ScoredSymbol>, incoming: Vec<Symbol>, max_fts_score: f64) {
    let existing: HashSet<(String, String)> = results
        .iter()
        .map(|r| (r.symbol.name.clone(), r.symbol.file_path.clone()))
        .collect();
    for sym in incoming {
        let key = (sym.name.clone(), sym.file_path.clone());
        if !existing.contains(&key) {
            results.push(ScoredSymbol { symbol: sym, score: max_fts_score });
        }
    }
}
```

**inject_exact**：

```rust
fn inject_exact(
    results: &mut Vec<ScoredSymbol>,
    query: &str,
    store: &Store,
    max_fts_score: f64,
) {
    if let Ok(exacts) = store.get_symbols_by_name(query) {
        for sym in exacts {
            if !results.iter().any(|r| r.symbol.name == sym.name && r.symbol.file_path == sym.file_path) {
                results.push(ScoredSymbol { symbol: sym, score: max_fts_score });
            }
        }
    }
}
```

**lib.rs — `CodeGraph::open` 和 `derive_project_name_tokens`**：

```rust
impl CodeGraph {
    pub fn open(project_path: &Path) -> Result<Self, String> {
        let db_path = project_path.join(".visp").join("codegraph.db");
        let store = Arc::new(Store::open(&db_path).map_err(|e| e.to_string())?);
        let is_building = Arc::new(AtomicBool::new(false));
        let project_name_tokens = derive_project_name_tokens(project_path);
        let query_engine = QueryEngine::new(
            store.clone(),
            is_building.clone(),
            project_name_tokens,
        );
        // ...
    }
}

fn derive_project_name_tokens(project_path: &Path) -> HashSet<String> {
    let mut tokens = HashSet::new();
    if let Ok(content) = std::fs::read_to_string(project_path.join("Cargo.toml")) {
        // 尝试提取 package name
        if let Some(name) = content.lines()
            .find(|l| l.trim().starts_with("name"))
            .and_then(|l| l.split('=').nth(1))
            .map(|v| v.trim().trim_matches('"').to_lowercase())
        {
            let norm: String = name.chars().filter(|c| c.is_alphanumeric()).collect();
            if norm.len() >= 5 { tokens.insert(norm); }
        }
    }
    if tokens.is_empty() {
        if let Some(dir) = project_path.file_name().and_then(|n| n.to_str()) {
            let norm: String = dir.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect();
            if norm.len() >= 5 { tokens.insert(norm); }
        }
    }
    tokens
}
```

### 🧪 测试 → 🔍 类型检查

需要更新 10 个已有测试的 `QueryEngine::new(store, is_building)` → `QueryEngine::new(store, is_building, HashSet::new())`。

```bash
cargo test -p visp-codegraph
cargo clippy -p visp-codegraph -- -D warnings
```

### 📦 提交

```
feat(codegraph): add search orchestration with FTS5/LIKE/exact pipeline
```
---
## 步骤 2b：评分排序（score_and_sort — query.rs）

### 🔴 红 — 测试

#### kind_bonus

| # | 测试用例 | 描述 |
|---|---------|------|
| 2b.1 | kind_fn_method | `Kind(Function)` = 10, `Kind(Method)` = 10 |
| 2b.2 | kind_interface | `Kind(Interface)` = 9 |
| 2b.3 | kind_class | `Kind(Class)` = 8 |
| 2b.4 | kind_typealias | `Kind(TypeAlias)` = 6 |
| 2b.5 | kind_enum | `Kind(Enum)` = 5 |
| 2b.6 | kind_variable | `Kind(Variable)` = 2 |

#### name_match_bonus

| # | 测试用例 | 描述 |
|---|---------|------|
| 2b.7 | exact_match | `"getUser"` vs `getUser` = 80 |
| 2b.8 | starts_with_ratio | `"get"` vs `getUser` = 40（4/7 × 30 + 10），`"get"` vs `getUserLongName` ≈ 12（3/13 × 30 + 10） |
| 2b.9 | substring | `"ser"` vs `UserService` = 10 |
| 2b.10 | no_match | `"xyz"` vs `foo` = 0 |
| 2b.11 | short_query | 查询词 < 2 字符 → 0 |

#### path_score

| # | 测试用例 | 描述 |
|---|---------|------|
| 2b.12 | path_match | `"auth"` vs `src/auth/login.rs` = 2 |
| 2b.13 | path_no_match | `"db"` vs `src/auth/login.rs` = 0 |
| 2b.14 | short_term | 查询词 < 2 字符 → 不加分 |
| 2b.15 | project_name_skip | 项目名 token `"visp"` 在路径 `visp-core/src/lib.rs` → 不加分 |

#### score_and_sort 整合

| # | 测试用例 | 描述 |
|---|---------|------|
| 2b.16 | function_before_variable | 相同 base_score 时 Function(10) 排在 Variable(2) 前面 |
| 2b.17 | exact_match_always_top | 精确匹配（+80）排在所有模糊匹配前面 |
| 2b.18 | path_score_boosts | 路径匹配的 symbol 在同类中排名更高 |
| 2b.19 | stable_comparison | 分数相同者保持相对顺序（或 ID 序兜底） |
| 2b.20 | project_name_skip_in_sort | 路径含项目名的 symbol 不被额外提升 |

### 🟢 绿 — 实现

```rust
fn score_and_sort(
    results: &mut Vec<ScoredSymbol>,
    query: &str,
    project_name_tokens: &HashSet<String>,
) {
    for scored in results.iter_mut() {
        scored.score += kind_bonus(&scored.symbol.kind);
        scored.score += name_match_bonus(&scored.symbol.name, query);
        scored.score += path_score(&scored.symbol.file_path, query, project_name_tokens);
    }
    results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
}

fn kind_bonus(kind: &SymbolKind) -> f64 {
    match kind {
        SymbolKind::Function | SymbolKind::Method => 10.0,
        SymbolKind::Interface => 9.0,
        SymbolKind::Class => 8.0,
        SymbolKind::TypeAlias => 6.0,
        SymbolKind::Enum => 5.0,
        SymbolKind::Variable => 2.0,
    }
}

fn name_match_bonus(name: &str, query: &str) -> f64 {
    let name_lower = name.to_lowercase();
    let query_lower = query.to_lowercase();

    // 精确匹配
    if name_lower == query_lower { return 80.0; }

    // 前缀匹配（按长度比例）
    if name_lower.starts_with(&query_lower) {
        let ratio = query_lower.len() as f64 / name_lower.len() as f64;
        return 10.0 + 30.0 * ratio;
    }

    // 子串匹配
    if name_lower.contains(&query_lower) { return 10.0; }

    0.0
}

fn path_score(
    file_path: &str,
    query: &str,
    project_name_tokens: &HashSet<String>,
) -> f64 {
    let path_lower = file_path.to_lowercase();
    let mut score = 0.0;
    for term in query.split_whitespace().filter(|t| t.len() >= 2) {
        let term_lower = term.to_lowercase();
        if project_name_tokens.contains(&term_lower) { continue; }
        if path_lower.contains(&term_lower) { score += 2.0; }
    }
    score
}
```

### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-codegraph
cargo clippy -p visp-codegraph -- -D warnings
```

### 📦 提交

```
feat(codegraph): add multi-signal scoring (kind + name + path)
```
---
## 步骤 3a：dedup 去重修复（parser.rs）

### 🔴 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 3a.1 | dedup_same_file_same_name | 同一文件中同名 symbol 只保留一个（原有行为不变） |
| 3a.2 | dedup_diff_file_same_name | 不同文件中同名 symbol 各自保留（如 `a.rs` 和 `b.rs` 都有 `parse_config`）→ 返回 2 个 |
| 3a.3 | existing_tests_pass | 已有 19 个测试全部通过 |

### 🟢 绿 — 实现

```rust
// Before:
let mut dedup: HashSet<String> = HashSet::new();

fn add_symbol(name, kind, file_path, node, symbols, dedup, next_id) -> u64 {
    if dedup.contains(name) {
        symbols.iter().find(|s| s.name == name).map(|s| s.id).unwrap_or(*next_id)
    } else {
        dedup.insert(name.to_string());
        // ... add symbol ...
    }
}

// After:
let mut dedup: HashSet<(String, String)> = HashSet::new();

fn add_symbol(name, kind, file_path, node, symbols, dedup, next_id) -> u64 {
    let key = (name.to_string(), file_path.to_string());
    if dedup.contains(&key) {
        symbols.iter().find(|s| s.name == name && s.file_path == file_path).map(|s| s.id).unwrap_or(*next_id)
    } else {
        dedup.insert(key);
        // ... add symbol ...
    }
}
```

**波及范围**：`HashSet<String>` → `HashSet<(String, String)>`，涉及 `walk_children`、`walk_node`、`handle_variable_declaration`、`handle_export` 共约 5~6 个调用点。

### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-codegraph
cargo clippy -p visp-codegraph -- -D warnings
```

### 📦 提交

```
fix(codegraph): fix dedup to allow same-named symbols across files
```
---
## 步骤 3b：多语言 node kind 覆盖率（parser.rs）

### 🔴 红 — 测试

#### Go 新增

| # | 测试用例 | 描述 |
|---|---------|------|
| 3b.1 | go_method | `"test.go"` 解析 `func (r *R) Foo() {}` → name=`Foo`, kind=Method |
| 3b.2 | go_const | `"test.go"` 解析 `const X = 1` → name=`X`, kind=Variable |
| 3b.3 | go_import | `"test.go"` 解析 `import "fmt"` → imports 表含 `("test.go", "fmt", "fmt")` 或等价记录 |
| 3b.4 | go_function | `"test.go"` 解析 `func Bar() {}` → name=`Bar`, kind=Function |
| 3b.5 | go_function_no_mistake_as_method | 模块级 `func Bar()` 不被标为 Method（仅接收器前缀的才是 Method） |

#### Rust 新增

| # | 测试用例 | 描述 |
|---|---------|------|
| 3b.6 | rust_const | `"test.rs"` 解析 `const MAX: usize = 100;` → name=`MAX`, kind=Variable |
| 3b.7 | rust_static | `"test.rs"` 解析 `static NAME: &str = "hello";` → name=`NAME`, kind=Variable |

#### Python 改善

| # | 测试用例 | 描述 |
|---|---------|------|
| 3b.8 | python_function | `"test.py"` 解析模块级 `def foo():` → name=`foo`, kind=Function |
| 3b.9 | python_method_in_class | `"test.py"` 解析 `class A: def bar(self):` → `bar` 的 kind=Method, `A` 的 kind=Class |
| 3b.10 | python_class | `"test.py"` 解析 `class Foo:` → name=`Foo`, kind=Class |
| 3b.11 | python_method_outside_class | 嵌套函数 `def outer(): def inner():` → `inner` 不被标为 Method |

#### TypeScript 新增

| # | 测试用例 | 描述 |
|---|---------|------|
| 3b.12 | ts_interface_method_signature | `"test.ts"` 解析 `interface I { foo(): void }` → `foo` 被提取（Function 或 Method） |
| 3b.13 | ts_generator | `"test.ts"` 解析 `function* gen() {}` → name=`gen`, kind=Function |

#### C/C++ 回归确认

| # | 测试用例 | 描述 |
|---|---------|------|
| 3b.14 | c_struct | `"test.c"` 解析 `struct S { int x; }` → name=`S`, kind=Class |
| 3b.15 | cpp_class | `"test.cpp"` 解析 `class C {}` → name=`C`, kind=Class |
| 3b.16 | c_function | `"test.c"` 解析 `void f() {}` → name=`f`, kind=Function |

### 🟢 绿 — 实现

**核心改动**：`walk_node` 的 match 从 `node.kind()` 升维为 `(node.kind(), lang)` 元组匹配。

**修改 `parse_file` 签名**，衍生出 language 参数：

```rust
pub fn parse_file(&mut self, file_path: &str, content: &str) -> Result<ParseResult, Box<dyn Error>> {
    let ext = Path::new(file_path).extension()
        .and_then(|e| e.to_str())
        .map(|e| format!(".{e}"))
        .unwrap_or_default();
    let lang = language_for_ext(&ext).unwrap_or("unknown");
    // ... 解析 ...
    walk_node(root, source, file_path, &mut symbols, &mut edges,
              &mut imports, &mut exports, &mut dedup, &mut next_id,
              None, lang);
    // ...
}
```

**`walk_node`/`walk_children` 加 `lang` 参数**，所有递归调用透传：

```rust
fn walk_node(node: Node, source: &[u8], file_path: &str,
    symbols: &mut Vec<Symbol>, edges: &mut Vec<Edge>,
    imports: &mut Vec<(String, String)>, exports: &mut Vec<(String, Option<u64>, Option<String>)>,
    dedup: &mut HashSet<(String, String)>, next_id: &mut u64, current_sym_id: Option<u64>,
    lang: &str)  // 新增参数
{
    match (node.kind(), lang) {
        // === 语言专属规则（必须在通用规则之前）===

        // Python 专属：类内 method 判断
        ("function_definition", "python") => {
            if current_sym_id.is_some() {
                let id = add_symbol(name, SymbolKind::Method, file_path, node, symbols, dedup, next_id);
                walk_children(..., Some(id), lang);
            } else {
                let id = add_symbol(name, SymbolKind::Function, file_path, node, symbols, dedup, next_id);
                walk_children(..., Some(id), lang);
            }
        }

        // === 跨语言通用规则 ===

        // 通用函数声明（排除 python，已在上面处理）
        ("function_declaration" | "function_item", _) |
        ("function_definition", _) => {
            let id = add_symbol(name, SymbolKind::Function, file_path, node, symbols, dedup, next_id);
            walk_children(..., Some(id), lang);
        }

        // 通用类/结构体声明
        ("class_declaration" | "class_definition" | "class_specifier"
         | "struct_specifier" | "struct_item", _) => {
            let id = add_symbol(name, SymbolKind::Class, file_path, node, symbols, dedup, next_id);
            // 处理 extends_clause / implements_clause ...
            walk_children(..., Some(id), lang);
        }

        ("method_definition", _) => {
            let id = add_symbol(name, SymbolKind::Method, file_path, node, symbols, dedup, next_id);
            walk_children(..., Some(id), lang);
        }

        // Go 专属
        ("method_declaration", "go") => {
            add_symbol(name, SymbolKind::Method, file_path, node, symbols, dedup, next_id);
        }
        ("const_declaration", "go") => {
            add_symbol(name, SymbolKind::Variable, file_path, node, symbols, dedup, next_id);
        }
        ("import_declaration", "go") => { handle_import(...); }

        // Rust 专属
        ("const_item", "rust") => { add_symbol(name, SymbolKind::Variable, ...); }
        ("static_item", "rust") => { add_symbol(name, SymbolKind::Variable, ...); }

        // 通用变量/常量声明
        ("lexical_declaration" | "variable_declaration" | "let_declaration", _)
        | ("const_declaration", "go") | ("const_item", "rust") | ("static_item", "rust") => {
            handle_variable_declaration(..., lang);
        }

        // 接口/枚举/类型别名
        ("interface_declaration" | "trait_item" | "interface_type", _) => { ... }
        ("type_alias_declaration" | "type_item" | "type_alias_statement" | "type_declaration", _) => { ... }
        ("enum_declaration" | "enum_item" | "enum_specifier", _) => { ... }

        // 导入
        ("import_statement" | "use_declaration" | "import_declaration", _)
        | ("import_declaration", "go") => { handle_import(...); }

        // 导出
        ("export_statement", _) => { handle_export(..., lang); }

        // 箭头函数（JS/TS）
        ("arrow_function", _) => { ... }

        // 调用表达式
        ("call_expression", _) => { ... }
        ("new_expression", _) => { ... }

        // extends / implements
        ("extends_clause", _) => { ... }
        ("implements_clause", _) => { ... }

        // 未识别的节点：继续遍历子节点
        _ => { walk_children(..., current_sym_id, lang); }
    }
}
```

**`handle_variable_declaration`/`handle_export` 等辅助函数**全部新增 `lang: &str` 参数并透传。

**`language_for_ext` 返回 `&str`**：

```rust
fn language_for_ext(ext: &str) -> &'static str {
    match ext {
        ".ts" | ".tsx" => "typescript",
        ".rs" => "rust",
        ".py" => "python",
        ".go" => "go",
        ".c" | ".h" => "c",
        ".cpp" | ".hpp" | ".cc" => "cpp",
        _ => "unknown",
    }
}
```

### 🧪 测试 → 🔍 类型检查

已有 19 个 parser 测试适配（`walk_children` 签名变化需补 `lang` 参数）。

```bash
cargo test -p visp-codegraph
cargo clippy -p visp-codegraph -- -D warnings
```

### 📦 提交

```
feat(codegraph): add lang-aware node kind matching for Go/Rust/Python/TS/C/C++
```
---
## 验证标准

### 每步骤提交前

```bash
cargo test -p visp-codegraph
cargo clippy -p visp-codegraph -- -D warnings
```

### 全 Wave 完成后

```bash
cargo test && cargo clippy -- -D warnings && cargo fmt -- --check
```

### 召回验收

在 visp 项目上全量重建索引后验证：

| 查询 | 预期至少返回 | 依赖 |
|------|------------|------|
| `"search"` | `search_symbols`, `search_fts`, `search_like`, `QueryEngine::search` | Wave 1+2 |
| `"searchNodes"` | FTS（camelCase 子串） | Wave 1c |
| `"query_engine"` | `QueryEngine`（snake_case） | Wave 1b |
| `"parse_config"` | 跨文件同名（存在多个） | Wave 3a |
| `"const MAX"` | `MAX`（Rust const） | Wave 3b |
| `"func Foo"` | `Foo`（Go method） | Wave 3b |
| `"func Bar"` | `Bar`（Go function） | Wave 3b |
| `"def foo"` | `foo`（Python function） | Wave 3b |
| `"def bar"` | `bar`（Python method） | Wave 3b |

## 提交历史预期

```
feat(codegraph): add FTS5 virtual table and triggers for full-text search
feat(codegraph): add search_fts method with BM25 scoring
feat(codegraph): add search_like method for substring fallback
feat(codegraph): add backfill_fts for existing symbol data
feat(codegraph): add search orchestration with FTS5/LIKE/exact pipeline
feat(codegraph): add multi-signal scoring (kind + name + path)
fix(codegraph): fix dedup to allow same-named symbols across files
feat(codegraph): add lang-aware node kind matching for Go/Rust/Python/TS/C/C++
```
