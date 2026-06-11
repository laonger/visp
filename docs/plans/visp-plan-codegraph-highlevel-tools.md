# 工作计划：CodeGraph 高层次工具

## 概述

基于设计文档 `docs/design/visp-design-codegraph-highlevel-tools.md`，实施 3 个新 CodeGraph 工具、3 个现有工具改进、以及 prompt 引导调整。

## Wave 并行策略

```
Wave 1: 基础设施（3 个并行）
├── A: visp-codegraph 新增公有方法 (trace/impact/context 底层)
├── B: visp-tools — grep 加参数 (include/context/max_matches)
└── C: visp-tools — read_file 加 paths 参数

Wave 2: 工具实现（1 个串行步骤，依赖 Wave 1A）
└── A: visp-tools — 实现 codegraph_context/trace/impact + 更新 codegraph_search 描述

Wave 3: Prompt 调整（依赖 Wave 2 — 需知道确切工具名）
└── A: visp-core — render_tool_guide 分组 + 描述更新
```

## 步骤 1：visp-codegraph 新增公有方法

### 1a：`CodeGraph` 新增 `trace` / `impact` 方法

#### 🔴 红 — 测试

在 `crates/visp-codegraph/src/lib.rs` 的 `mod tests` 中新增：

| # | 测试用例 | 说明 |
|---|---------|------|
| 1 | `test_trace_direct_call` | A 直接调用 B，trace(A,B) 返回 A→B |
| 2 | `test_trace_indirect_path` | A→B→C，trace(A,C) 返回 A→B→C |
| 3 | `test_trace_no_path` | A 和 B 无调用关系，返回空 |
| 4 | `test_trace_with_cycle` | A→B→A，trace(A,B) 返回 A→B（去重，标记环） |
| 5 | `test_impact_depth_1` | 只有直接调用者/被调用者 |
| 6 | `test_impact_depth_2` | 递归一层间接调用 |
| 7 | `test_impact_no_results` | 孤立符号，返回空 |

需要先在测试数据库中插入符号和边。

#### 🟢 绿 — 实现

在 `visp-codegraph/src/query.rs` 中新增 `trace()` 和 `impact()` 方法：

- `trace(from, to)`：从 from 符号出发 BFS，找到 to 后返回路径（SymbolDetails 列表）
- `impact(symbol, depth)`：递归获取 callers 和 callees，depth 层

在 `visp-codegraph/src/lib.rs` 中 `CodeGraph` 结构体上暴露这两个方法。

---

## 步骤 2：grep 加参数（与步骤 1 并行）

### 2a：grep 新增 include / context / max_matches

#### 🔴 红 — 测试

在 `crates/visp-tools/src/search.rs` 的 `mod tests` 中新增：

| # | 测试用例 | 说明 |
|---|---------|------|
| 1 | `test_grep_with_context` | 传 context=2，输出包含上下 2 行 |
| 2 | `test_grep_with_include` | 传 include="*.rs"，只搜 .rs 文件 |
| 3 | `test_grep_with_max_matches` | 传 max_matches=5，最多 5 条结果 |
| 4 | `test_grep_context_clamped` | context=100 被截断为 50 |
| 5 | `test_grep_max_matches_clamped` | max_matches=0 被修正为 1 |

#### 🟢 绿 — 实现

- `parameters()` 的 JSON schema 新增三个可选参数
- `execute()` 中将新参数映射到 `rg` 命令行参数（`-C`、`-g`、`-m`）

---

## 步骤 3：read_file 加 paths 参数（与步骤 1、2 并行）

### 3a：read_file 新增可选 paths 参数

#### 🔴 红 — 测试

在 `crates/visp-tools/src/file.rs` 的 `mod tests` 中新增：

| # | 测试用例 | 说明 |
|---|---------|------|
| 1 | `test_read_file_single_path` | 传 path 参数，向后兼容 |
| 2 | `test_read_file_paths` | 传 paths=["a.rs","b.rs"]，两个文件内容 |
| 3 | `test_read_file_paths_partial_fail` | 一个文件存在一个不存在，返回存在的内容+错误标注 |
| 4 | `test_read_file_path_and_paths_conflict` | 同时传 path 和 paths，path 优先 |

#### 🟢 绿 — 实现

- `parameters()` 新增可选 `paths: array` 参数
- `execute()` 逻辑：先检查 `path`，再检查 `paths`，循环读取每个文件

---

## 步骤 4：实现 3 个新 CodeGraph 工具（依赖步骤 1）

### 4a：`codegraph_context` 工具

#### 🔴 红 — 测试

在 `crates/visp-tools/src/codegraph.rs` 的 `mod tests` 中新增（需要 mock 或真实索引）：

| # | 测试用例 | 说明 |
|---|---------|------|
| 1 | `test_context_overview` | detail=overview，返回入口点+调用关系+代码片段 |
| 2 | `test_context_full` | detail=full，额外返回完整源码 |
| 3 | `test_context_no_results` | 关键词无匹配，返回空结果说明 |
| 4 | `test_context_multi_keyword` | 多个关键词，合并搜索 |
| 5 | `test_context_parameters_schema` | 参数 schema 正确 |

#### 🟢 绿 — 实现

- 新建 `CodeGraphContext` struct + Tool trait 实现
- `execute()`：用关键词搜索符号 → 对每个符号 `get_details` → 整理入口点/调用关系 → 按 `detail` 决定是否读源码
- `parameters()` 含 `task`、`detail`、`max_nodes`

### 4b：`codegraph_trace` 工具

#### 🔴 红 — 测试

| # | 测试用例 | 说明 |
|---|---------|------|
| 1 | `test_trace_direct` | 直接调用路径 |
| 2 | `test_trace_multi_hop` | 间接调用路径 |
| 3 | `test_trace_not_found` | 起点或终点不存在 |
| 4 | `test_trace_disambiguation` | 同名符号，返回消歧义列表 |

#### 🟢 绿 — 实现

- 新建 `CodeGraphTrace` struct + Tool trait 实现
- `execute()`：搜索 from/to → 用 `trace()` 方法找路径 → 格式化输出

### 4c：`codegraph_impact` 工具

#### 🔴 红 — 测试

| # | 测试用例 | 说明 |
|---|---------|------|
| 1 | `test_impact_depth_1_default` | depth=1，返回直接调用 |
| 2 | `test_impact_depth_2` | depth=2，递归展开 |
| 3 | `test_impact_not_found` | 符号不存在 |

#### 🟢 绿 — 实现

- 新建 `CodeGraphImpact` struct + Tool trait 实现
- `execute()`：搜索符号 → 用 `impact()` 方法按 depth 递归获取

### 4d：更新 `codegraph_search` 描述

#### 🔴 红 — 测试

| # | 测试用例 | 说明 |
|---|---------|------|
| 1 | `test_codegraph_search_description` | 描述不再包含 "Slower than Grep" |

#### 🟢 绿 — 实现

- 修改 `description()` 的返回值

---

## 步骤 5：Prompt 调整（依赖步骤 4 — 需知道最终工具名）

### 5a：`render_tool_guide` 新增 Code Understanding 分组

#### 🔴 红 — 测试

在 `crates/visp-core/src/agent.rs` 的 `mod tests` 中：

| # | 测试用例 | 说明 |
|---|---------|------|
| 1 | `test_render_tool_guide_has_code_understanding` | 输出包含 "## Code Understanding" |
| 2 | `test_render_tool_guide_context_first` | codegraph_context 排在最前 |
| 3 | `test_render_tool_guide_analyze_no_new_tools` | Analyze 分组中不含 3 个新工具 |
| 4 | `test_render_tool_guide_fix_broken_ref` | 不再引用不存在的 codegraph_context |

#### 🟢 绿 — 实现

- 修改 `render_tool_guide()`：在 category 分组之前，先硬编码输出 `## Code Understanding` 分组
- 在渲染 `## Analyze` 分组时，跳过 3 个新工具的名称

---

## 测试覆盖汇总

| Wave | 并行数 | 模块 | 步骤 | 测试用例数 |
|------|--------|------|------|-----------|
| 1 | 3 | visp-codegraph | 1a | 7 |
| 1 | 3 | grep | 2a | 5 |
| 1 | 3 | read_file | 3a | 4 |
| 2 | 1 | codegraph_context | 4a | 5 |
| 2 | 1 | codegraph_trace | 4b | 4 |
| 2 | 1 | codegraph_impact | 4c | 3 |
| 2 | 1 | description | 4d | 1 |
| 3 | 1 | render_tool_guide | 5a | 4 |
| **合计** | | | | **33** |

## 依赖关系总览

```
visp-codegraph (1a) ──→ codegraph_context (4a)
                      ├──→ codegraph_trace (4b)
                      └──→ codegraph_impact (4c)
                              │
grep (2a) ────────────────────┤
read_file (3a) ───────────────┤
                              ↓
                    render_tool_guide (5a)
```

## TODO（设计文档的后续事项）

- 引入模糊匹配（编辑距离 + token 重叠），支持自然语言→符号名自动映射
