# visp Phase 5 阶段设计：CodeGraph 代码智能

## 1. 阶段目标

实现基于 tree-sitter 的代码解析、索引和查询引擎，为 visp 提供代码智能能力（符号搜索、调用关系查询）。

**一句话总结**：对 TypeScript/JavaScript 项目完成全量索引后，支持按名称搜索符号和查询符号的调用者/被调用者。

**独立性**：CodeGraph 是独立模块，与 Phase 3（Agent 核心）和 Phase 4（CLI 前端）可并行开发。最终由 visp-daemon 导入，为已定义的 gRPC RPC（SearchSymbols、GetSymbolDetails）提供实现。

## 2. 模块划分

Phase 5 涉及一个新 crate（visp-codegraph）和一个已有 crate 的扩展（visp-daemon 集成）。

| Crate | 职责 | 类型 |
|---|---|---|
| **visp-codegraph** | tree-sitter 解析、符号/关系提取、索引构建、查询引擎、SQLite 持久化、文件监听 | 新建 |
| **visp-daemon** | 集成 CodeGraph，实现 SearchSymbols / GetSymbolDetails RPC | 扩展 |

### 2.1 visp-codegraph crate

#### 2.1.1 模块结构

```
visp-codegraph/
├── Cargo.toml
└── src/
    ├── lib.rs          # 模块声明 + CodeGraph 主结构体 + re-export
    ├── parser.rs       # tree-sitter 集成（解析单文件，提取符号和关系）
    ├── graph.rs        # 核心数据类型（Symbol, Edge, SymbolKind, EdgeKind）
    ├── store.rs        # SQLite 持久化（schema 管理、CRUD 操作）
    ├── index.rs        # 索引构建编排（全量 + 增量）
    ├── query.rs        # 查询引擎（搜索、详情查询）
    └── watcher.rs      # 文件监听（notify + 防抖 + 触发增量索引）
```

**新增依赖**：
- `tree-sitter`：核心解析引擎
- `tree-sitter-typescript`：TypeScript/TSX 语法库
- `tree-sitter-javascript`：JavaScript 语法库（MVP 可仅 TypeScript）
- `rusqlite`：SQLite 绑定（feature: `bundled`）
- `walkdir`：递归遍历项目目录
- `notify`：workspace 已有，用于文件监听
- `tokio`：workspace 已有，异步运行时
- `visp-core`：项目内依赖

#### 2.1.2 核心数据类型（`graph.rs`）

定义符号图的核心数据结构，纯数据模块，无 IO 依赖。

**Symbol**：代码中的一个可识别实体。

| 字段 | 类型 | 说明 |
|---|---|---|
| `id` | u64 | 唯一标识（SQLite 自增主键） |
| `name` | String | 符号名称 |
| `kind` | SymbolKind | 符号类型 |
| `file_path` | String | 所在文件路径（相对于项目根目录） |
| `line` | u32 | 起始行号 |
| `column` | u32 | 起始列号 |
| `signature` | Option\<String\> | 函数签名字符串（函数/方法有，变量/类可能无） |
| `docstring` | Option\<String\> | 文档注释文本 |

**SymbolKind**：枚举，当前支持的符号类型。

- `Function` — 函数声明 / 箭头函数 / 函数表达式
- `Method` — 类/对象方法
- `Class` — 类声明
- `Interface` — 接口声明
- `TypeAlias` — 类型别名
- `Variable` — 变量声明（const/let/var）
- `Enum` — 枚举声明

**Edge**：两个符号之间的关系。支持跨文件延迟解析——parser 先创建未解析边（用名称标记目标），跨文件解析阶段转为已解析边（用 ID 标记目标）。

| 字段 | 类型 | 说明 |
|---|---|---|
| `source_id` | u64 | 源符号 ID |
| `target_id` | Option\<u64\> | 目标符号 ID（已解析时有值） |
| `target_name` | Option\<String\> | 目标符号名称（未解析时用名称标记，已解析后清空） |
| `kind` | EdgeKind | 关系类型 |

**EdgeKind**：枚举。

- `Call` — 源符号调用了目标符号（函数调用）
- `Reference` — 源符号引用了目标符号（变量使用、类型引用）
- `Implementation` — 源符号实现了目标符号（类实现接口）
- `Inheritance` — 源符号继承/扩展了目标符号（类继承）

**FileInfo**：已索引文件的元数据。

| 字段 | 类型 | 说明 |
|---|---|---|
| `path` | String | 文件路径（主键） |
| `language` | String | 语言类型（"typescript"） |
| `symbol_count` | u32 | 该文件包含的符号数量 |
| `last_indexed_at` | u64 | 最后索引时间（Unix 时间戳） |

#### 2.1.3 解析器（`parser.rs`）

**职责**：使用 tree-sitter 解析单个文件，遍历 AST，提取符号节点和关系边。

**输入**：文件路径 + 文件内容（源代码文本）

**输出**：该文件的所有 `Symbol` 列表 + 该文件内发现的 `Edge` 列表（含未解析边）+ `imports` 映射 + `exports` 映射

**解析流程**：

```
读取文件内容
    │
    ▼
选择语言（按扩展名：.ts/.tsx → TypeScript）
    │
    ▼
tree-sitter 解析为 AST
    │
    ▼
遍历 AST 树：
  1. 识别符号节点（function_declaration, class_declaration, ...）
     为每个符号创建 Symbol（含名称、类型、位置）
  2. 识别关系节点（call_expression, new_expression, ...）
     解析被调用的名称 → 创建 Edge(Call) 或 Edge(Reference)
  3. 识别导入语句（import_statement）
     记录"本地名称 → 导入源"映射，用于跨文件关系解析
  4. 识别继承/实现（class_heritage, implements_clause）
     创建 Edge(Inheritance) 或 Edge(Implementation)
```

**符号提取规则**：

解析器内部维护以 `(file_path, name)` 为 key 的去重集合。同一文件内同名符号只提取一次（先到先得），避免 `var_declaration` + `arrow_function` 等复合同步提取产生重复符号。

| tree-sitter 节点类型 | SymbolKind | 名称来源 |
|---|---|---|
| `function_declaration` | Function | 子节点 `identifier` |
| `arrow_function` (赋值给变量) | Function | 父节点的变量名 |
| `method_definition` | Method | 子节点 `property_identifier` |
| `class_declaration` | Class | 子节点 `identifier` |
| `interface_declaration` | Interface | 子节点 `type_identifier` |
| `type_alias_declaration` | TypeAlias | 子节点 `type_identifier` |
| `variable_declaration` (const/let/var) | Variable | 子节点 `identifier` |
| `enum_declaration` | Enum | 子节点 `identifier` |

**导出清单**（不创建符号，仅用于跨文件解析）：

解析过程中，当遇到 `export` 关键字（`export_statement`、`export default` 等），记录 `(导出名 → 被导出的本地符号名)` 映射。此映射随 symbols/edges/imports 一并返回，由 index 模块写入 Store 的 `exports` 表。

对于 `export { foo } from './other'` 形式的 re-export：当前文件不包含 `foo` 的本地符号，记录为 `(导出名 → re_export_source)` 映射，存入 exports 表的 `re_export_source` 字段。跨文件解析时沿 `re_export_source` 链式查找最终符号。

**关系提取规则**：

| AST 节点/模式 | EdgeKind | 说明 |
|---|---|---|
| `call_expression` 中函数名为本地符号 | Call | 函数调用 |
| `new_expression` 中类名为本地符号 | Call | new 实例化 |
| `identifier` 引用（非调用位置） | Reference | 变量/类型引用 |
| `type_annotation` 中的类型引用 | Reference | 类型标注引用 |
| `class_heritage` → `extends_clause` | Inheritance | 类继承 |
| `class_heritage` → `implements_clause` | Implementation | 接口实现 |

**导入源路径解析**：

跨文件关系解析时，`import { foo } from './bar'` 中的 `./bar` 需要解析为实际文件路径。解析规则（按优先级尝试）：

1. 精确匹配：`./bar` → 若文件 `./bar` 存在且扩展名支持，直接使用
2. 追加扩展名：`./bar.ts`、`./bar.tsx` → 按 supported_extensions 顺序尝试
3. 目录导入：`./bar/index.ts`、`./bar/index.tsx`
4. 若以上都无法匹配，该导入源标记为不可解析，相关边保持未解析状态

解析时以导入文件所在目录为基准拼接相对路径，跨项目边界（如 `../outside/` 跳出项目根目录）的导入不解析。

对于 `import { foo } from './bar'` 形式的导入：
1. 记录当前文件引入的符号清单（本地名 → 导入源）
2. 在当前文件中，凡是调用/引用 `foo` 的地方，**照常创建 Edge**，但标记为未解析：`target_id = NULL, target_name = "foo"`
3. 延后到索引构建阶段解析——待所有文件索引完成后，根据导入源和导出符号名，将未解析边的 `target_name` 匹配到远程符号，填充 `target_id`

**错误处理**：
- 解析失败（语法错误）：跳过该文件，记录 warning，不中断整体索引
- 不支持的扩展名：静默跳过
- 超大文件（> 5MB）：跳过，记录 warning

#### 2.1.4 存储层（`store.rs`）

**职责**：管理 SQLite 数据库，提供符号/边/文件的 CRUD 操作。

**SQLite Schema**：

五张核心表：

- **`symbols`**：符号表，存储所有已提取的符号
  - 字段：`id INTEGER PRIMARY KEY AUTOINCREMENT`, `name TEXT NOT NULL`, `kind TEXT NOT NULL`, `file_path TEXT NOT NULL`, `line INTEGER NOT NULL`, `column INTEGER NOT NULL`, `signature TEXT`, `docstring TEXT`
  - 约束：`UNIQUE(file_path, name)` — 同文件同名符号视为同一符号，增量更新时 `INSERT OR REPLACE` 保留原 rowid，指向它的边不会断裂
  - 索引：`name`（前缀搜索）、`file_path`（按文件删除/更新）

- **`edges`**：边表，存储符号间的关系（支持未解析边）
  - 字段：`id INTEGER PRIMARY KEY AUTOINCREMENT`, `source_id INTEGER NOT NULL REFERENCES symbols(id) ON DELETE CASCADE`, `target_id INTEGER REFERENCES symbols(id) ON DELETE SET NULL`, `target_name TEXT`, `kind TEXT NOT NULL`
  - `target_id` 为 NULL 且 `target_name` 非 NULL 表示未解析边（待跨文件解析）
  - `target_id` 非 NULL 表示已解析边
  - 索引：`source_id`、`target_id`（调用者/被调用者查询）

- **`files`**：文件元数据表
  - 字段：`path TEXT PRIMARY KEY`, `language TEXT NOT NULL`, `symbol_count INTEGER DEFAULT 0`, `last_indexed_at INTEGER NOT NULL`

- **`imports`**：导入映射表（跨文件关系解析用）
  - 字段：`file_path TEXT NOT NULL`, `local_name TEXT NOT NULL`, `import_source TEXT NOT NULL`
  - 复合索引：`(file_path, local_name)`

- **`exports`**：导出映射表（跨文件关系解析用）
  - 字段：`file_path TEXT NOT NULL`, `export_name TEXT NOT NULL`, `symbol_id INTEGER REFERENCES symbols(id) ON DELETE CASCADE`, `re_export_source TEXT`
  - 常规导出：`symbol_id` 填本地符号 ID，`re_export_source` 为 NULL
  - Re-export（`export { foo } from './bar'`）：`symbol_id` 为 NULL，`re_export_source` 填导入源路径
  - 跨文件解析处理 re-export 时，沿 `re_export_source` 继续查找最终符号
  - 复合索引：`(file_path, export_name)`

**关键操作**：

| 操作 | 说明 | SQL 策略 |
|---|---|---|
| 初始化数据库 | 建表 + 索引 | `CREATE TABLE IF NOT EXISTS` |
| 批量插入符号 | 一个文件的所有符号一次事务写入 | 事务 + `INSERT INTO` |
| 按文件删除符号 | 增量更新时清除旧数据 | 事务中先 `DELETE` 再 `INSERT` |
| 按前缀搜索符号 | 按 `name LIKE 'prefix%'` 查询 | `SELECT ... WHERE name LIKE ?` |
| 查询调用者 | 某符号被谁调用 | JOIN edges + symbols on `source_id` |
| 查询被调用者 | 某符号调用了谁 | JOIN edges + symbols on `target_id` |
| 列出已索引文件 | 获取所有已索引文件列表 | `SELECT path FROM files` |
| 获取文件符号数 | 用于增量更新判断 | `SELECT symbol_count FROM files WHERE path = ?` |

**设计决策**：
- 每个项目一个独立的 SQLite 数据库文件，路径为 `<project_path>/.visp/codegraph.db`
- `open` 时若 `.visp` 目录不存在则自动创建
- daemon 根据会话的 `project_path` 拼接 db 路径，无需额外配置
- 使用 `rusqlite` 的 WAL 模式（Write-Ahead Logging），支持并发读 + 写
- 每个连接必须执行 `PRAGMA foreign_keys = ON`（`ON DELETE CASCADE` 依赖此开关）
- 批量操作使用 SQLite 事务（大幅提升写入性能）
- 跨文件关系解析在存储层之上完成（由 index 模块编排）

#### 2.1.5 索引构建（`index.rs`）

**职责**：编排全量索引构建和增量索引更新。协调 parser、store 和 watcher 三者的工作。

**全量构建流程**：

```
1. 初始化数据库（建表）
2. 递归遍历项目目录，收集所有支持的文件（.ts, .tsx）
   - 排除 .git, node_modules, dist, build, .next 等目录
3. 对每个文件：
   a. parser 解析 → 获得 symbols + edges + imports
   b. store 批量写入 symbols（获得 id）
   c. store 写入 edges（使用 id）
   d. store 写入 imports（本地名 → 导入源映射）
   e. store 更新 files 表
 4. 跨文件关系解析：
    遍历 edges 表，筛选 `target_id IS NULL AND target_name IS NOT NULL` 的未解析边：
    - 根据边的 `target_name` 和 imports 表，查找导出源文件中的对应符号
    - 匹配成功后，UPDATE edge SET target_id = <远程符号id>, target_name = NULL
    匹配失败的边（无法解析的导入）保留未解析状态，查询时忽略此类边
5. 完成
```

**增量更新流程**（由 watcher 触发）：

```
收到文件变更事件（防抖后）
  │
  ├─ 文件新增 → 步骤 A
  ├─ 文件修改 → 步骤 A
  └─ 文件删除 → 删除该文件的所有 symbols + edges + imports + files 记录

步骤 A（新增/修改）:
  1. Store 事务：先删除该文件的旧 symbols + edges + imports + exports
  2. Parser 解析该文件 → 获得 symbols + edges + imports + exports
  3. Store 写入新数据
  4. 跨文件关系解析：遍历整个 imports 表，全量重匹配跨文件边
     - 处理其他文件对此文件新符号的导入
     - 处理此文件对其他文件的导入
```

**设计决策**：
- 全量构建是同步/阻塞操作（可放在后台 tokio task 中），增量是轻量级事务
- 跨文件关系采用两阶段方案：先提取所有本地关系，再通过 imports 表解析跨文件关系
- 排除目录列表可配置，有合理默认值
- 索引构建过程中不阻塞查询（WAL 模式保证）
- 全量构建结果写入后通知 watcher 启动文件监听

#### 2.1.6 查询引擎（`query.rs`）

**职责**：提供符号搜索和详情查询的高层 API。

**search 方法**：

- 输入：查询字符串（前缀）、返回上限
- 流程：
  1. SQLite 前缀匹配：`SELECT * FROM symbols WHERE name LIKE '<query>%' LIMIT <limit>`
  2. 结果转为 `SymbolInfo` 列表（name, kind, file_path, line, column, signature）
- 排序：按名称字典序（SQLite 默认）

**get_details 方法**：

- 输入：符号名称
- 流程：
  1. 按名称精确匹配查找所有同名符号
   2. 对每个符号：查询调用者 + 被调用者 + 源码片段
      - 调用者/被调用者查询返回格式 `"符号名 (相对路径)"`（如 `"bar (src/utils.ts)"`），仅统计已解析边（`target_id IS NOT NULL`）
  3. 返回所有匹配的 `Vec<SymbolDetails>`
- 无匹配时返回空列表

**source 字段获取**：

由于 SQLite 不存储完整源码，`SymbolDetails.source` 的获取方式为：根据 `file_path` 和 `line` 返回该符号所在行的源码片段（截取合理长度，如前 500 字符）。

**设计决策**：
- 搜索仅支持前缀匹配（`LIKE 'prefix%'`），不支持模糊搜索
- 搜索区分大小写（`LIKE` 默认行为），与编程语言标识符规则一致
- 相同名称但不同文件的符号都会返回（调用方通过 file_path 区分）
- 查询是同步操作（SQLite 读很快，不需要 async）
- 无结果时返回空列表而非错误
- 全量构建进行中时，search / get_details 返回错误 `"codegraph: index is building, please retry later"`，不返回部分结果

#### 2.1.7 文件监听（`watcher.rs`）

**职责**：使用 `notify` crate 监听项目文件变更，经过防抖后触发增量索引更新。

**监听逻辑**：

```
notify 事件流
  │
  ▼
防抖队列（500ms 窗口）
  - 合并同一文件的多次变更
  - 合并创建后立即删除的文件（取消索引）
  │
  ▼
按文件类型过滤（仅 .ts, .tsx）
  │
  ▼
分类处理：
  ├─ Created / Modified → 触发增量索引更新
  └─ Removed → 触发删除操作
```

**目录排除**：自动排除 `.git`、`node_modules`、`dist`、`build`、`.next`、`target` 等目录的变更事件。

**设计决策**：
- 防抖窗口 500ms（与现有设计文档一致）
- 监听在独立的 tokio task 中运行，不阻塞主线程
- 增量索引更新使用异步任务提交
- 全量构建成功后启动 watcher；构建失败则不启动，记录 error 日志
- 下次启动时如检测到数据库不完整则重新全量构建

#### 2.1.8 CodeGraph 主结构体（`lib.rs`）

**职责**：对外暴露统一的 API，组装各子模块。

**生命周期管理**：

- `open(project_path)` → 初始化 SQLite 连接，创建 `CodeGraph` 实例
- `build_full()` → 全量索引构建（阻塞，可在后台 task 中调用）
- `start_watching()` → 启动文件监听（spawn 独立 task）
- `search(query, limit)` → 符号搜索
- `get_details(name)` → 符号详情查询
- `shutdown()` → 停止文件监听，关闭 SQLite 连接

**内部组成**：
- 持有 `Store` 实例（SQLite 连接）
- 持有 `Watcher` 实例（文件监听任务句柄）
- `Parser` 是无状态的，构造时预加载语法库，全量构建和增量更新复用同一实例

### 2.2 visp-daemon 集成

Phase 3 中 visp-daemon 的 `SearchSymbols` 和 `GetSymbolDetails` RPC 返回 `unimplemented`。Phase 5 提供真实实现。

**集成方式**：

- visp-daemon 的 `Cargo.toml` 添加 `visp-codegraph` 依赖
- daemon 首次收到某项目的 SearchSymbols/GetSymbolDetails 请求时，懒加载创建 CodeGraph 实例并触发后台全量构建。后续请求复用已有实例。
- `CoderDaemonService` 新增 `codegraphs: HashMap<String, Arc<CodeGraph>>` 字段（key=project_path），懒加载创建实例
- `SearchSymbols` 实现：根据 project_path 查找或创建 CodeGraph 实例 → `codegraph.search(query, limit)`。若对应项目未启用 CodeGraph 则返回 unimplemented。
- `GetSymbolDetails` 实现：同上，按 project_path 选择 CodeGraph 实例，调用 `codegraph.get_details(symbol_name)`
- daemon shutdown 时遍历所有 CodeGraph 实例，逐个调用 `shutdown()`

**daemon 配置扩展**：新增 `[codegraph]` section：

| 配置项 | 说明 | 默认值 |
|---|---|---|
| `enabled` | 是否启用 CodeGraph | `true` |
| `exclude_dirs` | 排除的目录列表 | `.git`, `node_modules`, `dist`, `build`, `.next`, `target` |
| `supported_extensions` | 支持的文件扩展名 | `.ts`, `.tsx` |

注：数据库文件固定为 `<project_path>/.visp/codegraph.db`，无需配置。

## 3. 依赖关系

```
visp-codegraph (新 crate)
    ├──→ tree-sitter (C 库 FFI)
    ├──→ tree-sitter-typescript (语法库)
    ├──→ rusqlite (SQLite)
    ├──→ notify (workspace 已有)
    ├──→ tokio (workspace 已有)
    └──→ visp-core (项目内，错误类型)

visp-daemon (扩展)
    └──→ visp-codegraph (新增依赖)
```

## 4. 核心数据流

### 4.1 全量索引构建

```
CodeGraph::build_full()
    │
    ├─ 1. 遍历项目目录 (walkdir)
    │     收集所有 .ts/.tsx 文件
    │     (排除 .git, node_modules, ...)
    │
    ├─ 2. 对每个文件：
    │     Parser::parse_file(path, content)
    │       │
    │       ├─ tree-sitter 解析 → AST
    │       ├─ 遍历 AST 提取：
    │       │   ├─ symbols: Vec<Symbol>
    │       │   ├─ edges: Vec<Edge> (本地关系)
    │       │   └─ imports: Vec<Import> (导入映射)
    │       │
    │       └─ 返回解析结果
    │     │
    │     Store 事务写入：
    │       ├─ insert_symbols(symbols) → id 回填
    │       ├─ insert_edges(edges)       -- 含未解析边
    │       ├─ insert_imports(imports)
    │       ├─ insert_exports(exports)   -- 导出映射
    │       └─ upsert_file(path, language, count)
    │
    ├─ 3. 跨文件关系解析：
    │     遍历 edges 表筛选未解析边 (target_id IS NULL AND target_name IS NOT NULL)：
    │       - 根据 target_name + imports 表 + exports 表匹配远程符号
    │       - 匹配成功 → UPDATE edge SET target_id = <远程id>, target_name = NULL
    │       - 匹配失败 → 保留未解析状态（查询时忽略）
    │
    └─ 4. 完成，通知 watcher 可启动
```

### 4.2 增量索引更新

```
Watcher 事件 (debounced)
    │
    ├─ FileCreated / FileModified:
    │   1. Store.begin_transaction()
    │   2. Store.delete_by_file(path)  -- 清除旧数据 (symbols, edges, imports, exports)
    │   3. Parser::parse_file(path)     -- 重新解析
    │   4. Store 写入新数据 (symbols, edges, imports, exports)
    │   5. Store.commit_transaction()
    │   6. 跨文件关系解析：遍历未解析边，全量重匹配
    │      (处理此文件与其他文件之间的导入/导出关系)
    │
    └─ FileRemoved:
        1. Store.begin_transaction()
        2. Store.delete_by_file(path)  -- 清除所有关联数据
        3. Store.delete_file_record(path)
        4. Store.commit_transaction()
```

### 4.3 查询流程

```
gRPC: SearchSymbols(query="get", limit=20)
    │
    ▼
CodeGraph::search("get", 20)
    │
    ▼
Store: SELECT * FROM symbols 
      WHERE name LIKE 'get%' 
      LIMIT 20
    │
    ▼
返回 Vec<SymbolInfo>

---

gRPC: GetSymbolDetails(name="getUser")
    │
    ▼
CodeGraph::get_details("getUser")
    │
    ├─ Store: SELECT * FROM symbols WHERE name = 'getUser'
    │         → 匹配到 N 个同名符号
    │
    ├─ 对每个符号：
    │   ├─ Store: callers query → Vec<String>
    │   ├─ Store: callees query → Vec<String>
    │   └─ Store: 源码片段读取
    │
    └─ 返回 Vec<SymbolDetails> (N 个，各有 file_path)
```

## 5. 并发模型

```
┌───────────────────────────────────────────────────┐
│                visp-daemon 进程                      │
│                                                    │
│  ┌──────────────┐     ┌──────────────────┐        │
│  │ Agent Loop   │     │ CodeGraph        │        │
│  │ (per session)│     │                  │        │
│  │              │     │ ┌──────────────┐ │        │
│  │              │     │ │ SQLite (WAL) │ │        │
│  │              │     │ │ 读写保护      │ │        │
│  └──────────────┘     │ └──────────────┘ │        │
│                       │                  │        │
│  gRPC Search RPC ────→│ query.rs         │        │
│                       │ (只读)           │        │
│                       │                  │        │
│                       │ ┌──────────────┐ │        │
│                       │ │ Watcher Task │ │        │
│                       │ │ (独立 tokio) │ │        │
│                       │ │ 增量更新     │ │        │
│                       │ └──────────────┘ │        │
│                       └──────────────────┘        │
└───────────────────────────────────────────────────┘
```

**并发保证**：
- SQLite WAL 模式：允许多个读操作与一个写操作并发
- Store 模块内部使用 `Arc<Mutex<Connection>>` 保护连接，Connection 不暴露给 Store 以外的模块
- Store 对外暴露普通方法（search、insert、delete 等），内部自动加锁，调用方无需关心线程安全
- 全量构建期间会阻止 watcher 启动（构建完成后再监听）
- 查询操作（search/get_details）在 gRPC handler 线程上执行，通过 Store 内部锁保证安全

## 6. Phase 5 不做什么

- ❌ 不实现多语言支持（MVP 仅 TypeScript/JavaScript）
- ❌ 不实现路径追踪（两个符号间的调用路径 BFS）
- ❌ 不实现影响分析（修改某符号的多跳影响范围）
- ❌ 不实现文件树浏览
- ❌ 不实现模糊搜索（仅前缀匹配）
- ❌ 不实现 CodeGraph Context 查询（复合搜索+调用者+被调用者）
- ❌ 不支持 Go/Python/Rust 等其他语言语法
- ❌ 不做符号重命名等编辑操作
- ❌ 不做索引可视化

## 7. 验收标准

- `cargo build --workspace` 编译通过（包含 visp-codegraph）
- `cargo test --workspace` 所有测试通过
- `cargo clippy --workspace -- -D warnings` 通过
- `cargo fmt --check --all` 通过
- **单元测试**：每个模块至少 3 个测试用例
- **集成测试 1**：对一个示例 TypeScript 项目执行全量构建，搜索 "get" 返回正确结果
- **集成测试 2**：符号详情查询返回正确的 callers/callees 列表
- **集成测试 3**：修改文件后增量索引更新正确（旧符号移除，新符号出现）
- **集成测试 4**：解析包含语法错误的文件不中断整体索引构建
