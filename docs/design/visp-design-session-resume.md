# Session Resume — `-s` 参数恢复历史会话

## 背景

用户希望像 OpenCode 那样，通过 `-s <session_key>` 参数恢复一个历史会话。当前每次启动 CLI 都会创建一个新会话（UUID v4），历史会话只能被查看但无法重新进入交互模式。

## 当前状态分析

### 现有基础设施

| 组件 | 当前能力 |
|------|----------|
| `SessionManager.get(id)` | 已实现 — 通过 ID 检索会话 |
| `SessionManager.start_loop(id)` | 已实现 — 加载 session.history 作为初始上下文 |
| `SessionManager.list()` | 已实现 — 列出所有会话 |
| `SessionStore` | 内存中已完整的 CRUD |
| gRPC `Chat` | 已支持通过 session_id 绑定会话 |

关键发现：**daemon 端的 agent 循环已经支持恢复**。`start_loop()` 会从 store 加载 `session.history` 作为 `AgentLoopContext.history`，再传给 `run_agent_loop()`。这意味着只要跳过创建新会话、直接使用已有 session_id 启动 chat，agent 就能继续之前的对话。

### 当前缺失的环节

1. **gRPC**: 没有 `GetSession` RPC — CLI 无法查询已有会话的信息
2. **CLI**: 没有 `-s` 参数 — 永远只调用 `create_session`
3. **Launcher**: 没有 `-s` 参数 — 无法透传给 CLI
4. **会话列表**: 用户需要一种方式查看可用的历史会话
5. **agent 异常终止恢复**: agent loop panic（进程未退出）后 session 状态卡在 `Running`，无法 resume

## Session ID 方案选择

### 方案对比

| 维度 | Option A: nanoid | **Option B: UUID 前缀匹配** |
|------|------------------|---------------------------|
| 新依赖 | `nanoid` crate | **无** |
| 迁移成本 | 旧 UUID session 难找回 | **零** |
| 前缀歧义 | 按设计不存在 | 需要定义处理逻辑 |
| Daemon 改动 | 改 `generate_session_id` | `get_session` 添加前缀搜索 |
| 与现有 TUI 一致性 | 不一致（8→12 char） | **一致（TUI 已用 8-char 截断）** |

**关键发现：** TUI 状态栏已在用 `app.session_id.chars().take(8)` 截取 UUID 前 8 字符作为短 ID 显示（`ui.rs`）。Option B 与此天然一致。

**结论：选择 Option B（UUID 前缀匹配）**。

### 前缀匹配规则

`GetSession` 的查找策略（daemon 端）：
1. 先做精确匹配（完整 session_id）
2. 精确匹配不到，做前缀搜索（输入字符前缀匹配）
3. 前缀匹配结果：
   - 0 个 → NotFound（CLI 收到后引导用户）
   - 1 个 → 返回该 Session
   - 多个 → 也返回 NotFound（因 Proto 返回类型为单个 `Session`，无法承载列表）

多匹配时 CLI 收到 NotFound 后，调用 `ListSessions` 获取全量列表，按项目路径和输入的前缀做二次筛选，展示给用户选择。

UUID v4 的前 4 个 hex 字符有 65,536 种组合，前 6 个有 16.7M 种，在单个 daemon 实例上前缀冲突概率接近零。但规则仍需覆盖多匹配情况。

## 架构变更

只新增、不改原有逻辑，所有变更向后兼容。

### 1. gRPC Proto

新增 `GetSession` RPC：

- **新增 RPC**：`rpc GetSession(GetSessionRequest) returns (Session);`
- 直接返回 `Session` 类型（与 `CreateSession` 返回类型一致），不引入多余的 `GetSessionResponse` 包装层
- `Session` 结构体不变（已有 `session_id`、`status`、`project_path`、`created_at`、`model` 字段）

### 2. visp-core — 无变更

Session ID 保持使用 UUID v4。`SessionStore` 基于字符串 key 的 HashMap，格式不敏感，无需修改。

### 3. visp-daemon

新增 `get_session` handler：
- 接收 `GetSessionRequest`（含 `session_id`）
- 调用 `session_mgr.get(id)` 做精确匹配
- 精确匹配失败后执行前缀搜索：遍历 `session_mgr.list()` 按前缀过滤
- 匹配到 1 个 → 返回 Session
- 匹配到 0 个或多个 → 返回 NotFound（多匹配时 CLI 调用 `ListSessions` 二次筛选）

Chat handler 无需修改 — 已有的 `session_mgr.get() + session_mgr.start_loop()` 路径已支持恢复。当 CLI 传入已有 session_id 时，chat handler 走同一条代码路径，加载历史消息继续对话。

### 4. visp-cli

新增 `-s/--session` 可选参数。当提供时：
- 调用 `GetSession` 查询该 session
- 存在则用其 `session_id` 启动 chat
- 不存在则打印可用的最近会话列表并退出

当会话未找到时，不简单 `exit(1)`，而是调用 `ListSessions` 展示最近 session 供用户参考。

### 5. visp launcher

新增 `-s/--session` 参数，透传给 CLI 子进程，模式与现有的 `-p/--project` 透传一致。

### 数据流

```
# 创建新会话（无变化）
visp -p /my-project
  → CLI create_session → daemon 生成 UUID → 开始对话

# 恢复会话
visp -p /my-project -s 550e84
  → launcher 透传 --session 550e84 给 CLI
  → CLI get_session("550e84") → daemon 前缀匹配 → 返回 Session
  → CLI chat(session_id="550e84") → daemon start_loop → 加载历史消息 → 继续对话

# 查看可用会话
visp --list
  → CLI list_sessions → 显示最近会话列表（含前缀可复制的短 ID）
  或 TUI 内 /sessions 命令
```

## 会话浏览机制

会话 resume 的前提是用户知道有哪些会话可恢复。为此需要：

### 入口 1：`visp --list`
- 调用 `ListSessions` 展示最近 N 个会话
- 每行显示：短 ID（前 8 位）| 项目路径 | 创建时间 | 状态
- 用户可以复制短 ID 后执行 `visp -s <short-id>`

### 入口 2：TUI 内 `/sessions` 命令（后续迭代）
- 在聊天界面内列出当前项目的会话
- 选中后直接切换到该会话

## 边界情况处理

### 1. Agent 异常终止恢复

当前 `SessionStore` 是纯内存存储，进程级崩溃后 session 随 daemon 一起消失，不存在"卡在 Running"的问题。但 agent loop 内 panic（进程未退出）会导致 session 状态停留在 `Running`，无法被 resume（`start_loop` 拒绝非 `Idle` 的 session）。

处理方式：`run_agent_loop` 在 `finally`/`catch_unwind` 中确保 session 状态重置为 `Idle`。这是一个健壮性加固，不依赖持久化。

如果将来引入持久化，daemon 启动时可一并清理"僵尸"Running session。

### 2. 项目路径匹配

`visp -p /other-project -s abc` 时：
- CLI 的 `-p` 与 session 存储的 `project_path` 不一致
- CLI 应校验路径，不匹配时拒绝恢复并打印提示，要求使用正确路径重试
- 这个限制的原因是 resume 场景下 `-p` 的含义不清（`-p` 应匹配 session 所在的项目）
- 如果用户需要查看某个项目下的 session，应使用 `visp --list -p /correct-project`

### 3. 恢复时 system prompt 过期

恢复的 session 使用创建时加载的 `system_prompt_template`，不会反映 `.visp/system-prompt.md` 的后续修改。这是合理的设计选择（保持对话上下文一致），但在文档中需显式说明。

### 4. 会话未找到时的引导

`-s` 参数指定 session 找不到时，CLI 应：
1. 打印错误信息
2. 调用 `ListSessions` 列出当前项目下的最近会话（短 ID + 创建时间 + 状态）
3. 提示用户可以复制短 ID 重试

### 5. 多 daemon 实例

`-s` 与 `-a`（daemon 地址）配合使用。不同端口的 daemon 实例各自的 session 空间独立，自然隔离。

## 向后兼容性

- 所有新增代码对现有功能零影响
- Session ID 格式不变（仍是 UUID v4）
- 已有 session 完全可以通过完整 UUID 或前缀匹配恢复
- 未传 `-s` 时行为完全不变
- gRPC proto 新增 RPC，不影响已有客户端
- `GetSession` 直接返回 `Session`，与现有 `CreateSession` 模式一致

## Session ID 显示

TUI 在标题栏或状态栏显示当前 session_id 的前 8 位（已实现），方便用户记住以在后续 `-s` 中使用：

```
visp — /my-project [550e8400]
```

`ui.rs` 中已有 `app.session_id.chars().take(8)` 截断逻辑，无需额外改动。

## 待讨论问题

1. **排序与数量限制**：`ListSessions` 在 `--list` 场景下应该按时间倒序排列，最多显示多少个会话？
2. **会话过期清理**：引入持久化后，是否需要按时间或数量自动清理历史会话？
