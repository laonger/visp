# CodeGraph 搜索召回优化设计

## 1. 背景与问题

### 当前状态

visp-codegraph 的搜索召回率远低于预期。用户输入 `codegraph_search("getUser")` 经常返回空结果，但实际代码中存在同名或相关 symbol。

| 指标 | 当前值 | 目标值 |
|------|--------|--------|
| 精确名称匹配召回 | ~70% | 100% |
| 子串匹配召回 | ~20% | ≥90% |
| **综合召回率** | **~30-40%** | **≥90%** |

### 根因

当前 `search_symbols` 只有 **单一 LIKE 前缀匹配**：

```rust
// store.rs:173
let pattern = format!("{}%", prefix);
"SELECT ... FROM symbols WHERE name LIKE ?1 LIMIT ?2"
```

对比 TypeScript 版 codegraph 的多层搜索（FTS5 BM25 → LIKE 子串补充 → 多信号排序），visp 缺少：

1. **全文检索引擎** — 无 FTS5，无法对 name/signature/docstring 做 BM25 加权搜索
2. **子串匹配** — `signIn` 搜不到 `signInWithGoogle`
3. **精确名保底** — 精确匹配不保证出现在结果中
4. **多信号排序** — 结果按 SQL 原生顺序返回，不区分 function 与 variable
5. **查询语法** — 不支持 `kind:`, `lang:`, `path:`, `name:` 字段过滤

---

## 2. 设计方案（经讨论确认）

### 2.1 搜索模型：方案 B（FTS5 + LIKE 按需补充）

```
FTS5 ─────→ 候选集 A ──┐
                        ├─── merge + 去重 + 评分排序 → 输出
LIKE ─────→ 候选集 B ──┘
(仅当 A 未达 limit 时)

Exact 注入 ──始终执行
```

**选择的理由**：

FTS5 前缀通配 `"term"*` 是按 token 前缀匹配的，对 camelCase 名称搜索能力有限。例如搜索 `"get_user"`：

| 名称 | FTS5 `"get_user"*` | FTS5 分词 `"get" "user"`* | LIKE 子串 |
|------|-------------------|--------------------------|----------|
| `getUser` | ❌（不同 token） | ✅ | ❌（无子串 `get_user`） |
| `get_user_info` | ✅ | ✅ | ✅ |
| `getUserProfile` | ❌ | ✅ | ❌ |

FTS5 和 LIKE 互补而非互斥，**两者结合能覆盖更多场景**。而 LIKE 只在 FTS5 不足 limit 时才跑，常见情况仍只有一次 SQL。

### 2.2 排序模型：统一多信号评分

**核心原则**：不区分结果来自 FTS5 还是 LIKE，所有结果走同一套评分公式。

```
final_score = base_score + kind_bonus + name_match_bonus + path_score
```

#### base_score

| 来源 | 取值 |
|------|------|
| **FTS5 结果** | `bm25(symbols_fts, 0, 20, 0, 5, 1)` — 列权重：id=0 (UNINDEXED), name=20, kind=0 (UNINDEXED), signature=5, docstring=1 |
| **LIKE 结果 / 精确名注入** | 借用当前查询中 FTS5 结果的最高 BM25 分（`max_fts_score`） |

LIKE 结果借用 BM25 分的理由：如果不给一个基础分，纯靠 bonus 排序的 LIKE 结果和带 BM25 的 FTS5 结果不在同一量级，混合排序会乱。借用 `max_fts_score` 保证 "至少和最相关的 FTS5 结果同一梯队"，然后靠 nameMatchBonus 做内部差异化排序。

#### name_match_bonus（最重要的信号）

| 匹配类型 | Bonus | 示例 |
|----------|-------|------|
| 精确匹配 `name == query` | 80 | `getUser` → `getUser` |
| 名称以查询词开头，按长度比例 | 10~40 | `"get"`→`getUser`(40) vs `"get"`→`getUserLongName`(12) |
| 子串包含 | 10 | `"ser"` → `UserService` |

> 注：曾考虑 `token 精确匹配（60分，多词查询中精确匹配某 token）` 和 `全 camelCase 分词匹配（15分）`，经讨论确认 FTS5 的 BM25 已天然覆盖这两种场景，无需在 name_match_bonus 中重复加分。

#### kind_bonus

```
function = 10, method = 10, interface = 9, class = 8
type_alias = 6, enum = 5, variable = 2
```

#### path_score

查询词的 token 在文件路径中出现时加分。项目名 token（如 `visp`）自动降权，避免项目名匹配所有 `visp-*` 文件导致误排。

### 2.3 中间类型

FTS5 搜索需要返回 BM25 分数，但当前 `Symbol` 没有 score 字段。新增内部中间类型：

```rust
/// 带 BM25 分数的 symbol，仅在 QueryEngine 内部使用
struct ScoredSymbol {
    symbol: Symbol,
    score: f64,
}
```

Store 的 `search_fts` 返回 `Vec<ScoredSymbol>`，`search_like` 返回 `Vec<Symbol>`（LIKE 本身无分数，评分在 QueryEngine 侧做）。

### 2.3.1 QueryEngine 新增：项目名 token 集合

`QueryEngine` 新增 `project_name_tokens` 字段，用于 `path_score` 中过滤掉项目名匹配项，避免项目名（如 `visp`）导致所有 `visp-*` 路径下的文件被误加分。

```rust
pub struct QueryEngine {
    store: Arc<Store>,
    is_building: Arc<AtomicBool>,
    /// 项目名 token 集合（如 ["visp"]），用于 path_score 降权。
    /// 从 Cargo.toml workspace members / 目录名 推导，仅保留 ≥5 字符的 token。
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

派生逻辑（在 `CodeGraph::open()` 中执行）：

```rust
fn derive_project_name_tokens(project_path: &Path) -> HashSet<String> {
    let mut tokens = HashSet::new();

    // 1. 从 Cargo.toml 的 workspace 或 package name 提取
    //    仅取最后一个 segment（如 "visp" 从 "visp-core" 也是取 "visp"）
    if let Ok(content) = std::fs::read_to_string(project_path.join("Cargo.toml")) {
        // 尝试 workspace members 前缀（如 members = ["crates/*"] → 用项目根目录名）
        // 尝试 package name（单 crate 项目）
        if let Some(name) = extract_package_or_workspace_name(&content) {
            let norm = name.to_lowercase().replace(|c: char| !c.is_alphanumeric(), "");
            if norm.len() >= 5 {
                tokens.insert(norm);
            }
        }
    }

    // 2. 回退：使用项目根目录名
    if tokens.is_empty() {
        if let Some(dir_name) = project_path.file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.to_lowercase().replace(|c: char| !c.is_alphanumeric(), ""))
        {
            if dir_name.len() >= 5 {
                tokens.insert(dir_name);
            }
        }
    }

    tokens
}
```

### 2.4 完整搜索流程

```rust
/// Phase 1 搜索入口——使用原始 query 直接搜索，无 filter 语法支持。
/// Phase 2 将升级为 parse_query(query) 以支持 kind:/lang:/path: 过滤器。
pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SymbolInfo>, String> {
    let mut results: Vec<ScoredSymbol> = Vec::new();

    // Step 1: FTS5 始终跑（BM25 加权搜索 name + signature + docstring）
    // 注意：需要对用户输入做 FTS5 特殊字符转义（详见转义说明），
    // 防止 " 、*、AND/OR/NOT 等导致语法错误
    // Phase 1 限制：不识别 kind:/lang:/path: 前缀，所有文本原样送入 FTS5。
    // "kind:function foo" 中的 ":" 会被 sanitize_fts_query 剥离，变为
    // "kindfunction foo" 送入 FTS5——这是可接受的 Phase 1 行为。
    if !query.is_empty() {
        let fts_query = sanitize_fts_query(query);
        results = self.store.search_fts(&fts_query, limit * 5)?;
    }

    // Step 2: 如果 FTS5 结果不足 limit，用 LIKE 子串补充
    // 当 query 为空时，search_like 会匹配所有行（LIKE '%%'）
    // 计算一次 max_fts_score，供 Step 2 的 merge_dedup 和 Step 3 的 inject_exact 共用
    let max_fts = results.iter().map(|r| r.score).fold(0.0, f64::max);
    if results.len() < limit {
        let like_results = self.store.search_like(query, limit)?;
        merge_dedup(&mut results, like_results, max_fts);
    }

    // Step 3: 精确名始终注入（保底）
    inject_exact(&mut results, query, &self.store, max_fts);

    // Step 4: 评分排序 → 截断
    score_and_sort(&mut results, query, &self.project_name_tokens);
    results.truncate(limit);

    Ok(results.into_iter().map(|rs| sym_to_info(rs.symbol)).collect())
}

/// FTS5 查询转义：防止特殊字符破坏 FTS5 语法
/// Phase 1 限制：`:` 被从文本中剥离（包括 `kind:function` 中的冒号）。
/// 这是可接受的——Phase 2 加入 parse_query 后，filter 前缀在送入 FTS5 前就被
/// 提取走了，剩余文本中的 `:` 本就不该出现。
fn sanitize_fts_query(query: &str) -> String {
    query
        .replace("::", " ")                // Rust/C++ 限定符分隔
        .replace(['\'', '"', '*', '(', ')', ':', '^'], "")  // 剥离 FTS5 特殊字符
        .split_whitespace()
        .filter(|t| !["AND", "OR", "NOT", "NEAR"].contains(&t.to_uppercase().as_str()))
        .map(|t| format!("\"{}\"*", t))     // 每个 term 加前缀通配
        .collect::<Vec<_>>()
        .join(" OR ")
}

/// FTS5 + LIKE 结果合并去重：按 (name, file_path) 判重，保留得分高的
///
/// 注意：用 `(String, String)` 而非 `(&str, &str)` 判重，防止后续
/// `results.push()` 触发 Vec 扩容导致引用悬垂。
fn merge_dedup(results: &mut Vec<ScoredSymbol>, incoming: Vec<Symbol>, max_fts_score: f64) {
    let existing: HashSet<(String, String)> = results
        .iter()
        .map(|r| (r.symbol.name.clone(), r.symbol.file_path.clone()))
        .collect();
    for sym in incoming {
        let key = (sym.name.as_str(), sym.file_path.as_str());
        if !existing.contains(&key) {
            results.push(ScoredSymbol {
                symbol: sym,
                score: max_fts_score,  // 借用最高 FTS5 分
            });
        }
    }
}

/// 精确名保底注入
fn inject_exact(results: &mut Vec<ScoredSymbol>, query: &str, store: &Store, max_fts_score: f64) {
    if let Ok(exacts) = store.get_symbols_by_name(query) {
        for sym in exacts {
            if !results.iter().any(|r| r.symbol.name == sym.name && r.symbol.file_path == sym.file_path) {
                results.push(ScoredSymbol {
                    symbol: sym,
                    score: max_fts_score,
                });
            }
        }
    }
}

/// 统一评分排序：所有结果（FTS5 / LIKE / 注入）重新评分后排序
///
/// 评分公式：final_score = base_score + kind_bonus + name_match_bonus + path_score
/// - base_score：FTS5 结果用 BM25 分，LIKE/注入结果已在合并时借用了 max_fts_score
/// - kind_bonus：根据 symbol 类型加分（function=10, class=8, variable=2...）
/// - name_match_bonus：根据名匹配质量加分（精确=80, 前缀=10~40, 子串=10）
/// - path_score：查询词 token 在文件路径中出现时加分（项目名 token 自动跳过）
fn score_and_sort(results: &mut Vec<ScoredSymbol>, query: &str, project_name_tokens: &HashSet<String>) {
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
        SymbolKind::Class => 8.0,
        SymbolKind::Interface => 9.0,
        SymbolKind::TypeAlias => 6.0,
        SymbolKind::Enum => 5.0,
        SymbolKind::Variable => 2.0,
    }
}

fn name_match_bonus(name: &str, query: &str) -> f64 {
    let name_lower = name.to_lowercase();
    let query_lower = query.to_lowercase().replace(|c: char| !c.is_alphanumeric(), "");

    // 精确匹配
    if name_lower == query_lower { return 80.0; }

    // 名称以查询词开头 → 按长度比例给分
    if name_lower.starts_with(&query_lower) {
        let ratio = query_lower.len() as f64 / name_lower.len() as f64;
        return 10.0 + 30.0 * ratio;
    }

    // 子串包含
    if name_lower.contains(&query_lower) { return 10.0; }

    0.0
}

fn path_score(file_path: &str, query: &str, project_name_tokens: &HashSet<String>) -> f64 {
    let path_lower = file_path.to_lowercase();
    let terms: Vec<&str> = query.split_whitespace()
        .filter(|t| t.len() >= 2)
        .collect();
    let mut score = 0.0;
    for term in &terms {
        let term_lower = term.to_lowercase();
        // 跳过项目名 token（如 "visp"，否则所有 visp-* 路径都会被加分）
        if project_name_tokens.contains(&term_lower) {
            continue;
        }
        if path_lower.contains(&term_lower) {
            score += 2.0;
        }
    }
    score
}
```

**关于 `max_fts_score` 的兜底**：当 FTS5 返回 0 条结果时（少见，但可能发生在冷门查询），LIKE 和精确注入的结果没有 BM25 分可借用。此时 `max_fts_score = 0.0`，排序纯靠 `kind_bonus + name_match_bonus + path_score`，仍能正确排序。

### 2.5 Schema 变更

#### 2.5.1 FTS5 虚拟表，通过触发器与 `symbols` 表保持同步：

```sql
-- FTS5 全文索引
CREATE VIRTUAL TABLE IF NOT EXISTS symbols_fts USING fts5(
    id UNINDEXED,
    name,
    kind UNINDEXED,
    signature,
    docstring,
    content='symbols',
    content_rowid='rowid'
);

-- 保持同步的触发器
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
```

#### 2.5.2 辅助索引（提升 LIKE/过滤性能）

```sql
CREATE INDEX IF NOT EXISTS idx_symbols_kind ON symbols(kind);
CREATE INDEX IF NOT EXISTS idx_symbols_lower_name ON symbols(LOWER(name));
```

#### 2.5.3 LIKE 子串的 `LOWER()` 注意事项

当前 schema 有 `PRAGMA case_sensitive_like = ON;`，这意味着 `WHERE name LIKE '%Term%'` 不会匹配 `getTerm`。因此 `search_like` 必须显式使用 `LOWER()`：

```sql
SELECT s.* FROM symbols s
WHERE LOWER(s.name) LIKE '%' || LOWER(?) || '%'
   OR LOWER(s.signature) LIKE '%' || LOWER(?) || '%'
```

此外，LIKE 的通配符 `%` 和 `_` 需要在 `Store::search_like` 内部转义，防止用户查询中的 `_`（如 `foo_bar`）被解释为"匹配任意单个字符"：

```rust
fn escape_like(s: &str) -> String {
    s.replace("%", "\\%").replace("_", "\\_")
}
```

> 注：转义发生在 `Store::search_like` 内部，搜索流程（2.4 节）无需感知。

#### 2.5.4 FTS5 支持说明

`libsqlite3-sys` 的 `build.rs` 已经包含 `-DSQLITE_ENABLE_FTS5` 编译标志（由 `bundled` feature 触发），FTS5 开箱可用。只需在 `Cargo.toml` 中确认 `rusqlite` 的 `bundled` feature 已启用即可，无需额外配置。

#### 2.5.5 解析器去重修复：跨文件同名 symbol 丢失

**问题**：`add_symbol()` 使用全局 `HashSet<String>` 按 name 去重，导致不同文件中同名 symbol 被丢弃（如 `src/auth/config.rs` 和 `src/db/config.rs` 都有 `parse_config` 函数，后者被丢弃）。

**修复**：`HashSet<String>` → `HashSet<(String, String)>`，以 `(name, file_path)` 为 key 判重。

```rust
// parser.rs
// Before:
let mut dedup: HashSet<String> = HashSet::new();
// ...
if dedup.contains(name) { return existing_id; }
dedup.insert(name.to_string());

// After:
let mut dedup: HashSet<(String, String)> = HashSet::new();
// ...
if dedup.contains(&(name.to_string(), file_path.to_string())) { return existing_id; }
dedup.insert((name.to_string(), file_path.to_string()));
```

改动量：约 5~6 个调用点，均为机械适配。

#### 2.5.6 多语言 node kind 覆盖率修复

**问题**：当前 `walk_node` 用一组 node kind 字符串硬匹配所有语言，导致多语言 node kind 漏提取。

| 语言 | 漏提取项 | 影响 |
|------|---------|------|
| **Go** | `method_declaration` / `const_declaration` / `import_declaration` / `field_declaration` | Go 方法、常量、导入、字段完全不索引 |
| **Rust** | `const_item` / `static_item` | Rust 常量、静态变量漏提取 |
| **Python** | 类内 `function_definition` 未标为 Method | 方法被标为 Function |
| **TS/TSX** | `function_signature` / `generator_function_declaration` | 接口方法签名、生成器漏提取 |
| **C/C++** | `field_declaration` | struct/class 字段不索引 |

**修复方案**：给 `walk_node` 传入 `lang: &str` 上下文，将 match 升维为 `(kind, lang)` 元组匹配。

```rust
// 之前：按 node.kind() 全局匹配
match node.kind() {
    "function_declaration" | "function_item" | "function_definition" => { ... }
    "class_declaration" | "struct_item" | ... => { ... SymbolKind::Class }
    "lexical_declaration" | "variable_declaration" | "let_declaration" => { ... }
}

// 之后：按 (node.kind(), lang) 元组匹配
match (node.kind(), lang) {
    // === 语言专属规则（必须在通用规则之前，否则会被通用 _ 通配先拦截）===

    // Python 专属：需要感知是否在 class body 内
    ("function_definition", "python") => {
        if current_sym_id.is_some() {
            add_symbol(..., SymbolKind::Method)  // 类内 → Method
        } else {
            add_symbol(..., SymbolKind::Function)  // 模块级 → Function
        }
    }

    // === 跨语言通用规则 ===

    // 通用函数声明（排除 python，已在上面处理）
    ("function_declaration" | "function_item", _)
    | ("function_definition", _) => {  // _ 只会匹配非 python 语言
        add_symbol(..., SymbolKind::Function)
    }

    ("class_declaration" | "class_definition" | "class_specifier" | "struct_specifier" | "struct_item", _) => {
        ... SymbolKind::Class
    }
    ("method_definition", _) => { ... SymbolKind::Method }

    // Go 专属
    ("method_declaration", "go") => { ... add_symbol(..., SymbolKind::Method) }
    ("const_declaration", "go") => { ... add_symbol(..., SymbolKind::Variable) }
    ("import_declaration", "go") => { ... handle_import(...) }

    // Rust 专属
    ("const_item", "rust") => { ... add_symbol(..., SymbolKind::Variable) }
    ("static_item", "rust") => { ... add_symbol(..., SymbolKind::Variable) }

    // 通用变量/常量声明
    ("lexical_declaration" | "variable_declaration" | "let_declaration", _)
    | ("const_declaration", "go")
    | ("const_item", "rust")
    | ("static_item", "rust") => { ... handle_variable_declaration(...) }

    // 已知的通用模式
    ("interface_declaration" | "trait_item" | "interface_type", _) => { ... }
    ("type_alias_declaration" | "type_item" | "type_alias_statement" | "type_declaration", _) => { ... }
    ("import_statement" | "use_declaration" | "import_declaration", _)
    | ("import_declaration", "go") => { ... handle_import(...) }
}
```

`lang` 信息在 `parse_file()` 时从文件扩展名推导，通过 `walk_node` → `walk_children` 透传：

```rust
fn language_for_ext(ext: &str) -> Option<&'static str> {
    match ext {
        ".ts" | ".tsx" => Some("typescript"),
        ".rs" => Some("rust"),
        ".py" => Some("python"),
        ".go" => Some("go"),
        ".c" | ".h" => Some("c"),
        ".cpp" | ".hpp" | ".cc" => Some("cpp"),
        _ => None,
    }
}
```

改动量：`walk_node` 及其所有递归调用（`walk_children`、`handle_variable_declaration`、`handle_export` 等）新增 `lang: &str` 参数，重写 match 分支。约 30~40 行有变化。

### 2.6 查询语法解析（Phase 2）

支持字段过滤语法：

```
codegraph_search("kind:function lang:rust auth")
                  ↓ 解析后
    kinds: ["function"]
    languages: ["rust"]
    text: "auth"
```

| 字段 | 别名 | 值示例 |
|------|------|--------|
| `kind:` | — | function, method, class, struct, trait, interface, enum, variable, type_alias |
| `lang:` | `language:` | rust, typescript, python, go, c, cpp |
| `path:` | — | src/api, lib/ |

未知字段（如 `TODO:`）透传为纯文本搜索。

### 2.7 迁移方案

向后兼容：
- 新增 FTS5 表作为增量，不修改现有 `symbols` 表结构
- 现有 `search_symbols` API 签名不变（`query: &str, limit: usize`）
- 旧索引无需重建——FTS5 表会在下次插入/更新时通过触发器自动填充
- 存量数据需一次性回填：

```sql
INSERT INTO symbols_fts(rowid, id, name, kind, signature, docstring)
SELECT rowid, id, name, kind, signature, docstring FROM symbols;
```

---

## 3. 模块职责与边界

### 3.1 变更范围

| 文件 | 变更类型 | 说明 |
|------|----------|------|
| `crates/visp-codegraph/src/store.rs` | **修改** | 新增 FTS5 schema 初始化、`search_fts`、`search_like` 方法 |
| `crates/visp-codegraph/src/query.rs` | **修改/新增** | 新增 `search` 编排、`ScoredSymbol`、`inject_exact`、评分排序函数 |
| `crates/visp-codegraph/src/parser.rs` | **修改** | dedup 去重修复 + 语言感知 node kind 匹配（`(kind, lang)` 元组 match），透传 `lang` 参数到所有递归调用 |
| `crates/visp-tools/src/codegraph.rs` | **不改** | 参数文档可选更新 |
| `crates/visp-core/` | **不改** | 核心结构不动 |
| `crates/visp-proto/` | **不改** | 搜索接口签名不变 |
| `crates/visp-daemon/` | **不改** | 中间层无变更 |

### 3.2 禁止变更

- `visp-core` 的 `ToolContext`、`Tool` trait 等核心结构不做任何修改
- gRPC proto 不修改
- 现有测试不破坏

---

## 4. 测试策略

| 测试类型 | 覆盖范围 | 示例 |
|----------|----------|------|
| **FTS5 搜索** | BM25 查询构造、结果排序 | `"getUser"` → getUser 排名高于 getUserData |
| **降级 LIKE** | 子串匹配，仅在 FTS5 不足时触发 | `"ser"` → `UserService` |
| **精确注入** | 精确名一定出现在结果中 | `"get"` 包含 `get` 函数 |
| **评分排序** | 排序正确性，LIKE 结果借用 BM25 分 | function 在 variable 之前 |
| **FTS5 回填** | 存量数据迁移 | 批量插入 FTS5 表 |
| **空结果** | 无匹配场景 | 返回空 Vec，不 panic |

---

## 5. TODO（后续 Phase）

这些是讨论中确认暂不纳入 Phase 1、留待后续解决的问题：

- [ ] **Levenshtein 模糊匹配**：当 FTS5 + LIKE 都无结果时，编辑距离 ≤2 或相对距离 ≤30% 的 symbol 应被召回
- [ ] **查询语法解析**：`kind:` / `lang:` / `path:` / `name:` 字段过滤语法
- [ ] **SymbolKind 扩展**：添加 Trait / Struct / EnumMember / Field / Constant / Module 等种类
- [ ] **LanguageExtractor 模块拆分**：将 parser.rs 的单一 `walk_node` 拆分为每种语言独立的 extractor 模块（对标 TS codegraph 架构），便于后续新增语言和定制化提取逻辑
- [ ] **去重策略改进**：`UNIQUE(file_path, name)` → `UNIQUE(file_path, name, kind)`，解决同名不同类型 symbol 的冲突

---

## 6. 实施计划

### Phase 1（本次）

| 步骤 | 内容 | 涉及文件 |
|------|------|----------|
| 1 | FTS5 schema 初始化 + 辅助索引 | `store.rs` |
| 2 | `search_fts` — FTS5 BM25 查询 | `store.rs` |
| 3 | `search_like` — LIKE 子串查询 | `store.rs` |
| 4 | 搜索编排 + 精确名注入 | `query.rs` |
| 5 | 评分排序（kind_bonus + name_match_bonus + path_score + project_name_tokens） | `query.rs` |
| 6 | dedup 去重修复：`HashSet<String>` → `HashSet<(String, String)>` | `parser.rs` |
| 7 | 多语言 node kind 覆盖率修复：`walk_node` 升维为 `(kind, lang)` 元组匹配，修复 Go method/const/import、Rust const/static、Python method、TS function_signature/generator | `parser.rs` |
| 8 | FTS5 回填逻辑（存量数据迁移） | `store.rs` |
| 9 | 现有测试适配 + 新增测试 | `store.rs`, `query.rs`, `parser.rs` |
