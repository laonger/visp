# CodeGraph 高层次工具设计

## 背景

当前 visp 的信息收集（搜索、读取、理解）效率远低于预期。LLM 经常需要 30-50 轮工具调用来理解一个跨越多个文件的特性，而类似工具（opencode）用 1-3 轮就能完成同等任务。

## 问题分析

### 1. 工具粒度过细

当前 visp 的 CodeGraph 只有 3 个工具：

| 工具 | 作用 | 每次返回 |
|------|------|----------|
| `codegraph_search` | 搜索符号 | 仅位置列表（file:line），不带源码 |
| `codegraph_get_details` | 获取符号详情 | 单符号的调用者/被调用者列表 |
| `codegraph_rebuild` | 重建索引 | 无 |

要理解一个特性的全貌，LLM 被迫走这样的路径：

```
codegraph_search("auth")              → 找到 AuthService、loginHandler 等
codegraph_get_details("AuthService")  → 看定义 + 调用者
codegraph_get_details("loginHandler") → 再看另一个
read_file("auth/types.ts")            → 读类型定义
read_file("auth/service.ts")          → 读实现
→ 5～10 轮，还不知道关键路径
```

### 2. 缺少面向"理解"的高层次工具

当前工具的查询原语是代码库结构中的**单个节点**（一个符号）。但如果 LLM 的问题是"这个授权流程怎么工作的？"，它需要的不是单个符号，而是**一组相关符号 + 它们的调用关系 + 源码**。

缺少以下能力：

- **一次调用获取任务级上下文**：不是搜一个符号、读一个细节，而是"给一个描述，返回所有相关符号、入口点、关键代码"
- **批量获取相关符号源码**：一次读几个相关文件/符号的源码
- **调用链追踪**：从 A 函数到 B 函数路径上经过的所有调用
- **变更影响分析**：改一个符号会波及哪些地方

### 3. 文件读取工具也缺乏批量能力

`read_file` 一次只读一个文件。如果 LLM 需要理解包含 5 个文件的模块，必须 5 次调用。

`grep` 工具没有 `-C`（上下文行数）参数。搜到匹配行后，LLM 必须再调 `read_file` 才能看到周围上下文。

### 4. Prompt 引导方向不一致

`codegraph_search` 的描述主动劝退使用：

```
Slower than Grep for simple text search — prefer Grep for literal text patterns.
```

而 Grep 不带上下文行又没有 `include` 过滤，反倒导致更多后续调用。

`render_tool_guide` 推荐了不存在的 `codegraph_context` 工具名。

## 设计目标

1. **减少理解代码所需的工具调用次数**：从 5-15 轮降至 1-3 轮
2. **面向任务而非面向节点**：查询原语从"符号"升级到"上下文/关系"
3. **保持 CodeGraph 索引作为核心**：新工具基于现有索引能力扩展，不重复造轮子
4. **向后兼容**：不删除或破坏现有工具的调用接口

## 新增工具设计

### 工具一：`codegraph_context`

角色：「给我理解 X 所需的全部背景」

```
作用：
  给定一组关键词（符号名、文件名、代码术语），返回所有相关的：
  - 入口点（关键函数/类定义的位置和签名）
  - 相关符号（相关类型、辅助函数）
  - 调用关系（符号间的调用链）
  - 关键代码（默认代码片段，detail="full" 时返回完整源码）

  注意：task 参数接受的是空格分隔的搜索关键词，而非自由自然语言。
  LLM 负责将需求转化为具体的关键词（如符号名、文件名）。

参数：
  - task: string (必填) — 空格分隔的搜索关键词，如 "AuthService login verify"
  - detail: string (选填，默认 "overview") — 输出深度：
    - "overview"：入口点 + 调用关系 + 代码片段（默认）
    - "full"：额外返回匹配符号的完整源码（按文件分组）
  - max_nodes: integer (选填，默认 20) — 最多包含的符号数

返回：
  - 入口点列表（location + signature）
  - 相关符号列表（file:line + kind + name）
  - 调用关系说明
  - detail=overview：关键符号的源码片段
  - detail=full：匹配符号所属文件的完整源码（行号标记）

用法示例：
  codegraph_context(task="ErrorBoundary handleError")
  → 返回入口点、调用关系、代码片段

  codegraph_context(task="ErrorBoundary handleError", detail="full")
  → 同上面，但额外返回完整源码
```

设计决策：第一版 `task` 参数接受关键词而非自由自然语言。LLM 负责将需求转化为具体符号名。
`detail` 参数合并了原 `codegraph_explore` 的功能：需要批量阅读完整源码时设 `detail="full"`。
**后续 TODO**：引入模糊匹配（编辑距离 + token 重叠），支持自然语言→符号名的自动映射。

ToolDefinition description：
```
Get comprehensive context about a topic in the codebase. Returns entry points,
call relationships, and source code. Use this when you need to understand how
a module works or find related code. Set detail="full" to also get complete
source files (equivalent to reading multiple files at once).
```

### 工具二：`codegraph_trace`

角色：「从 A 到 B 是怎么走到的」

```
作用：
  追踪两个符号之间的完整调用路径，返回从起点到终点经过的
  所有函数/方法调用链。自动跨越文件边界。

参数：
  - from: string (必填) — 起始符号名（支持限定名如 `module::handleError`）
  - to: string (必填) — 终点符号名（支持限定名）

同名消歧义：如果匹配到多个符号，返回消歧义列表并提示 LLM 使用限定名重试。

返回：
  - 路径上的每个跳转（file:line → 函数名 → 调用了 → file:line）
  - 每个跳转的源码片段
  - 如果找不到静态路径，指明断裂处（如回调、动态分发）

循环处理：全局去重（visited set），已访问的符号不再展开，
在结果中标注"（已访问，跳过）"。

ToolDefinition description：
```
Trace the call path from one symbol to another across the codebase.
Returns each function call in the chain with file:line locations and
source code snippets. Handles cross-file calls automatically. If no
static path exists (e.g. callbacks, dynamic dispatch), the result
indicates where the chain breaks.
```

用法示例：
  codegraph_trace(from="handleRequest", to="sendResponse")
  → 返回完整调用链；如果经过回调，指出断裂处
```

### 工具三：`codegraph_impact`

角色：「改这个会炸什么」

```
作用：
  分析修改某个符号的影响半径。展示被该符号直接或间接调用的
  所有代码，以及调用它的所有代码。

参数：
  - symbol: string (必填) — 要修改的符号名
  - depth: integer (选填，默认 1) — 影响深度（1=直接，2=间接一层）

返回：
  - 直接调用者列表（depth=1，默认）
  - 直接被调用者列表（depth=1，默认）
  - 深度递归调用者列表（depth≥2 时递归展开）
  - 深度递归被调用者列表（depth≥2 时递归展开）

ToolDefinition description：
```
Analyze what would be affected if you change a symbol. Returns all
functions that call it (callers) and all functions it calls (callees).
Use this before refactoring to understand the blast radius. depth
controls recursion: 1 = direct only (default), 2 = one level indirect.
```

用法示例：
  codegraph_impact(symbol="AuthService.verify")
  → 返回直接调用和直接调用 verify 的所有函数
```

## 现有工具改进

### `codegraph_search` 改进

- 移除劝阻性描述
- 返回结构从纯文本改为按符号组织

### `read_file` 改进

- 新增可选参数 `paths: string[]` — 一次读多个文件
- 保留现有 `path: string` 参数，两者互斥。同时传时 `path` 优先（向后兼容）
- 现有 `start_line` / `end_line` 保留不变，作用于所有文件

### `grep` 改进

新增三个参数：

| 参数 | 类型 | 范围 | 默认值 | 极端值行为 |
|------|------|------|--------|-----------|
| `include` | string | glob 模式（如 `*.rs`） | 无（不过滤） | — |
| `context` | integer | 0～50 | 0 | `>50` 时截断为 50 |
| `max_matches` | integer | 1～500 | 50 | `≤0` 或 `>500` 时截断为边界值 |

## 与 Prompt 的配合

所有新工具均归类为 `"analyze"`（与现有 CodeGraph 工具相同）。

`render_tool_guide` 中新增独立的分组 `## Code Understanding`，按名称白名单列出 3 个新工具：

```
## Code Understanding (prefer these first)
codegraph_context  — Get comprehensive context for a topic (use detail="full" for complete source)
codegraph_trace    — Trace the call path between two symbols
codegraph_impact   — Analyze what would break if you change a symbol
```

其余工具保持按 category 分组的现有顺序（Common → Analyze → Network → MCP）。
`codegraph_search` 和 `codegraph_get_details` 留存在 Analyze 分类中。
移除 `render_tool_guide` 中对不存在的 `codegraph_context` 的引用。

## 实现要点

### 与现有索引的兼容

高层次的 `context` / `explore` / `trace` / `impact` 工具不应重复实现 CodeGraph 索引逻辑，而是基于现有索引原语（搜索、查询调用者、查询被调用者）的组合。它们是索引查询的编排者，而非新的索引构建者。

### 空匹配行为

三个新工具统一行为：搜索/查询无结果时返回空列表并附说明"未找到匹配"，不返回错误。空匹配是正常搜索行为，非异常。

### 返回结果大小控制

- 所有返回结果应控制在 100KB 以内（与现有工具一致）
- `context` 的 `max_nodes` 参数限制搜索空间
- `context` 的 `detail=full` 时通过 `max_nodes` 间接控制源码读取量
- `trace` 最多返回 50 跳（防止无限循环）
- `impact` 的 `depth` 参数控制递归深度

### 工具注册

新工具在 `visp-tools/src/codegraph.rs` 中实现（与现有 CodeGraph 工具同文件），通过现有 `ToolRegistry` 注册，无需修改 `visp-core`。

### `render_tool_guide` 渲染调整

3 个新工具的 `category()` 返回 `"analyze"`，但 `render_tool_guide` 渲染时需要将它们从 `## Analyze` 分组中排除，转而放到新增的 `## Code Understanding` 分组。现有 `codegraph_search` 和 `codegraph_get_details` 仍保留在 `## Analyze` 分组中。

## 预期效果

| 场景 | 当前 | 预期 |
|------|------|------|
| 理解一个模块 | 10-15 轮 | 1-2 轮 |
| 追踪调用链 | 5-8 轮 | 1 轮 |
| 分析变更影响 | 8-12 轮 | 1 轮 |
| 阅读多个相关文件 | 5 轮（逐个 read_file） | 1 轮（context detail="full"） |

## 不做的事情

- **不修改 `visp-core` 的核心结构**：`ToolContext`、`AgentLoopContext` 等不变
- **不重建索引**：新工具只查询现有索引
- **不改变现有工具的调用接口**：向后兼容
- **不引入新的数据存储**：CodeGraph SQLite 数据库已包含足够信息
