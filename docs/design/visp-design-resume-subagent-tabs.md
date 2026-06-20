# Resume Sub-Agent Tabs — 恢复 session 时一并恢复子 agent tab

## 背景

当前 `visp -s <session-id>` 或 TUI 内 `/sessions <id>` 切换会话时，daemon 只回放主 session 的对话历史。该 session 在历史上派生过的所有子 agent（通过 `task` 工具创建的 sub-session）的 tab 完全消失，用户看不到子 agent 曾经的输入输出。

需求：恢复主 session 时，递归地把所有后代子 agent 的 tab 一并恢复，每个 tab 内回放完整对话历史，统一标记为只读（View Only）。

## 现状（关键事实）

1. DB 已有完整数据：`session.parent_id` 存父 session ID，`agent_name` 存 agent 类型。`parent_id IS NULL` 即主 session。子 session 的 message 表完整保留消息。
2. CLI 已具备多 tab 能力：`TabBar` 通过 `route_frame()` 按 `session_id` 路由消息，未知 session_id 自动调用 `insert_sub_agent()` 创建新 tab。
3. 回放机制已存在：`JoinSession` handler 把主 session 的 history 转换成 `TextDelta` / `ToolCall` / `ToolResult` / `Done` 序列推给 CLI。
4. `StatusUpdate` 帧已有所需字段：`session_id`、`agent_name`、`user_inputs` 已存在，daemon 只需正确填充。
5. 唯一缺失：`JoinSession` handler 不查询 `parent_id = <main_session_id>` 的子 session，更不递归。

## 设计决策

| 维度 | 选择 |
|------|------|
| 恢复粒度 | 完整恢复历史（含 User / Assistant / Tool 三类消息） |
| 嵌套层级 | 递归全量：BFS 遍历整棵 parent-child 树，带 `visited` 防环 |
| 回放顺序 | 先主后子；兄弟按 `created_at` 升序 |
| 子 agent tab 状态 | 统一 `ViewOnly`，不区分 Completed / Interrupted / Failed |
| 用户在恢复 tab 中输入 | daemon 返回明确错误帧 `SessionNotActive`，CLI 视觉上禁用输入提示 |
| 失败容错 | 单个子 session 加载失败 → 记录日志 + 跳过 |
| 协议层方案 | 复用 `StatusUpdate`，新增 optional 字段 `view_only`；task prompt 复用 `StatusUpdate.user_inputs` 字段；`SessionNotActive` 复用现有 `Error` 帧 |
| 后代收集 | 应用层 BFS + 轻量元信息查询，元信息与 history 加载分离 |
| 回放执行模型 | 本期同步执行（已知限制），下一期改后台 task |

## 关键架构决策的取舍说明

### 状态统一 ViewOnly 而非区分 Completed / Interrupted

DB 里的 `session.status = Running` 既可能是 daemon 重启后的僵尸，也可能是 daemon 正在跑这个子 agent。仅凭 DB 字段无法可靠区分；维护活跃 loop 集合或用时间戳启发式都有误判和复杂度成本。

本需求的本质是查看历史，用户预期不是接着跑。统一 ViewOnly 最简单可靠，对话内容本身已经能让用户看出子 agent 跑到哪一步。未来如要区分运行中 vs 已中断是独立需求。

### 不引入新 proto 帧

`StatusUpdate` 已有 `session_id` / `agent_name` / `user_inputs` 三字段，CLI 的 `route_frame()` 已能据 `session_id` 路由并自动 `insert_sub_agent()`。daemon 只需在子 session 回放前发一帧 `StatusUpdate(session_id=child, agent_name=..., view_only=true)`，CLI 端即可建好 tab 并标注状态。新增一个 optional 字段 `view_only` 比新增一对 `SubAgentResumeBegin/End` 帧成本低、向后兼容性更好。

### 本期 JoinSession 同步回放

后台 spawn task 推送回放是更优解，但本期需求核心是功能补全。在典型场景下（< 5 个子 agent，总消息 < 1000 条）同步回放 1-2 秒内完成，可接受。该限制写入"已知限制"章节，留给下一期独立优化。

## 架构变更

### 改动范围

| Crate | 职责 | 改动概要 |
|-------|------|---------|
| visp-core | `SessionStore` trait | 新增 `list_child_sessions(parent_id) -> Vec<Session>` 接口（复用现有 Session 类型，不引入 SessionMeta） |
| visp-db | SQLite 实现 | 新增 `SessionRepo::list_child_sessions` SQL 查询；为 `parent_id` 加索引（`CREATE INDEX IF NOT EXISTS`） |
| visp-daemon | `JoinSession` / `UserInput` handler | 1) 主 session 回放完成后，BFS 遍历后代并依次回放；2) 为每个子 agent 推送一帧 `StatusUpdate` 带 `agent_name` 和 `view_only=true`，task prompt 塞入 `user_inputs`；3) `UserInput` 对非活跃 session 返回 `SessionNotActive` 错误帧（判断标准：daemon 内存活跃 loop 集合不包含该 session_id） |
| visp-cli | `AgentStatus` 枚举 + `TabEntry` | 1) `AgentStatus` 新增 `ViewOnly` 变体；2) 新增 `TabEntry::new_view_only()` 构造器，不改 `new()` 签名；3) ViewOnly tab 视觉上标注 (View Only) 并禁用输入回车；4) 收到 `SessionNotActive` 错误帧时给用户清晰提示 |
| visp-proto | 协议扩展 | 1) `StatusUpdate` 新增 optional 字段 `view_only: bool`；2) `SessionNotActive` 复用现有 `Error { code, message, session_id, agent_name }` 帧（`code="SessionNotActive"`），不新增 proto。task prompt 复用 `StatusUpdate.user_inputs` 字段传输，不新增 `UserMessage` 帧 |

### 数据流（恢复一个含子 agent 的主 session）

```
visp -s <main-id>
  -> CLI: get_session(main-id) -> 校验 project_path
  -> CLI: chat(main-id) + send_join()
       -> daemon: JoinSession 处理
            Step 1  发 StatusUpdate (main, user_inputs)
            Step 2  回放 main.history -> TextDelta/ToolCall/ToolResult/Done
            Step 3  list_descendants(main-id)              [新增]
                    BFS 收集后代 SessionMeta（visited 防环、created_at 升序）
            Step 4  for each descendant:                    [新增]
                      发 StatusUpdate(session_id=child, agent_name, view_only=true, user_inputs)
                      -> CLI: route_frame() 自动 insert_sub_agent()，状态置为 ViewOnly
                      加载 child messages（按需调用 get_messages）
                      回放 messages -> User/TextDelta/ToolCall/ToolResult
                      发 Done(session_id=child)
```

CLI 端 `route_frame()` 按 `session_id` 路由，方案天然兼容嵌套子 agent（孙级 session_id 形如 `parent/sub/uuid/sub/uuid`，CLI 仅做精确字符串匹配，不解析层级）。

### 子 session 消息回放范围

与现有主 session 回放（只覆盖 `Role::Assistant` + `Role::Tool`）不同，子 session 回放需**让用户看到 task prompt**（即首条 `Role::User` 消息），对理解子 agent 行为至关重要。

回放策略：
- `Role::User` 消息**不走独立帧**。首条 User 消息（即 task prompt）放入该子 session 的 `StatusUpdate.user_inputs` 字段随首帧下发；后续 `Role::User`（理论上不存在，task 模型单 prompt）若出现也追加进 `user_inputs` 数组。
- `Role::Assistant` -> TextDelta + 可能的 ToolCall
- `Role::Tool` -> ToolResult

### user_inputs 字段在 ViewOnly tab 的语义

`user_inputs` 当前用途是主 session 恢复后让用户按 ↑↓ 翻找历史输入。对 ViewOnly tab 复用该字段，行为定义如下：

- **展示路径**：tab 首帧 StatusUpdate 收到后，把 `user_inputs[0]` 作为 task prompt 渲染到 tab 顶部（视觉上等价于用户消息气泡，可标注 `[task prompt]` 前缀以区别于真正的用户输入）。
- **↑↓ 行为**：ViewOnly tab 输入框已禁用回车提交，但 ↑↓ 翻看 `user_inputs` 历史仍可用，让用户能回看 task prompt 原文。
- **不冲突的理由**：ViewOnly tab 的输入框即便能 ↑↓ 也无法发送，语义上"只读历史输入"与"禁用提交"一致，不构成体验冲突。

### 后代收集策略：应用层 BFS + 轻量元信息查询

daemon 持有一个待处理队列：
1. 初始放入 `main_session_id`
2. 每次 pop 调用 `list_child_sessions(parent_id)` 返回 `Vec<SessionMeta>`（仅含 id / agent_name / status / created_at，不含 history）
3. 结果按 `created_at` 升序入队，加入 `visited` 集合
4. 直到队列空

回放阶段，逐个 session 调用 `get_messages(session_id)` 加载消息后再生成帧。元信息查询和历史加载分离，避免 BFS 阶段一次性把所有 history 装入内存。

未来如需性能优化，可在不改上层逻辑的前提下替换为 `WITH RECURSIVE` 单条 SQL。

### tab 初始状态修正

当前 `TabEntry::new()` 硬编码 `AgentStatus::Running`，对回放历史场景语义错误。修正方式：

- 新增 `AgentStatus::ViewOnly` 变体
- **新增 `TabEntry::new_view_only(session_id, agent_name)` 构造器**，不改 `new()` 签名（`new()` 共 ~28 处调用，含 ~22 处测试；新增构造器只影响 `route_frame()` 中 `view_only=true` 分支的 1-2 处生产代码）
- `route_frame()` 在因 StatusUpdate 自动 `insert_sub_agent()` 时，按 `view_only` 字段决定走 `new()` 还是 `new_view_only()`
- tab UI（spinner / 标签）按状态分别渲染

### tab 数量软上限

不强制硬上限。建议软上限 50 个 sub tab；超过时 daemon 在 BFS 收集阶段截断，按 **BFS 层级优先 + 同层 created_at 倒序** 保留最近 50 个：直接子节点全部保留，剩余配额按层级广度遍历依次填充，同层用 created_at 倒序。理由：直接子节点是用户当初主动 `task` 派生的，重要性高于深层孙节点。

截断后 daemon 在主 session 的 StatusUpdate 之后追加一条**主 session TextDelta** 形式的提示消息：`[system] 子 agent 数量 N 超出上限 50，仅展示最近 50 个`。明确投递目标为主 session tab，避免提示丢失。

### tab frames 缓存清理（可选优化）

已完成回放的 ViewOnly tab，其 `frames` 已渲染至 `messages`，可在 Done 帧处理后清空 `frames` 释放内存。标注为已知优化点，本次需求不强制实施。

## 边界情况

1. **回放期间消息处理被阻塞**（已知限制）：JoinSession handler 在 `tokio::select!` 单分支中同步执行，回放期间用户输入的 Cancel / UserInput / 新 JoinSession 不响应。典型场景下阻塞 1-2 秒。下一期改为后台 task 推送解决。回放期间用户按 Ctrl+C 退出 CLI 会断开 gRPC 流，daemon 检测到流断开后中止回放，行为安全。
2. **用户在 ViewOnly tab 中输入**：CLI 视觉上禁用回车提示；若用户仍发送 UserInput，daemon 返回 `Error { code: "SessionNotActive", message, session_id }` 帧，CLI 渲染为提示信息。
3. **大量子 agent**：用 tab 软上限 + 截断策略保护渲染层。
4. **循环引用**：理论不存在（`spawn_sub_agent` 单向赋值），BFS 仍维护 `visited` 集合做防御。
5. **跨项目子 session**：信任写入侧（子 session 的 `project_path` 与父一致），不再额外校验。
6. **失败容错**：单个子 session 加载失败 -> 记日志、跳过、继续下一个。
7. **CLI 启动 vs `/sessions <id>` 切换**：两条路径都走 `send_join()`，行为一致。
8. **嵌套 session_id 路由**：CLI 仅做精确字符串匹配，不解析层级，孙级及以下 tab 路由天然支持。
9. **多 CLI 同时 join 同一 session**：不在本期需求范围。当前行为是各自独立回放（互不干扰），可接受。

## 已知限制

1. **回放期间不响应用户输入**：见边界情况 1。
2. **不区分子 agent 完成 / 中断 / 失败**：统一 ViewOnly。后续如有需要再做精确判断。
3. **不支持续跑子 agent**：恢复出的 ViewOnly tab 无法继续交互。如需"接着跑"是独立需求。
4. **tab 数量超 50 时截断**：BFS 阶段按 `created_at` 倒序保留最近 50 个。

## 验证策略

- DB 层：新增 `list_child_sessions` 单元测试（mock SQLite，验证只返回元信息、按 created_at 升序、`parent_id` 不匹配的不返回）。
- daemon 层：构造一个父 session + 2 层子 session 树的 fixture，断言 JoinSession 推送的帧序列正确（先主后子、子按 created_at 升序、`view_only=true` 标记正确、task prompt 通过 `user_inputs` 字段回放）。
- daemon 层：`UserInput` 对非 Running session 返回 `SessionNotActive` 错误帧的单元测试。
- CLI 层：mock 回放帧序列，断言 `TabBar` 最终包含正确数量的 tab、各 tab 内消息条数与预期一致、tab 状态为 `ViewOnly`、`SessionNotActive` 错误帧能正确渲染提示。
- 端到端：脚本化用例 - 启动 daemon -> 创建主 session -> 触发 task 工具派生子 agent -> 子 agent 完成 -> 退出 CLI -> `visp -s <main-id>` -> 断言子 agent tab 出现且内容完整、状态为 ViewOnly。

## 影响范围

- 仅新增逻辑，不改变现有调用路径 -> 现有 session（无子 agent）的恢复行为完全不变。
- DB schema 不变（`parent_id` 列 V4 已存在）；本次新增 `parent_id` 索引，幂等 `CREATE INDEX IF NOT EXISTS`。
- 协议层新增：仅 `StatusUpdate.view_only: bool` 一个 optional 字段。task prompt 复用现有 `user_inputs` 字段，`SessionNotActive` 复用现有 `Error` 帧，均不新增 proto 帧。
- 兼容性：daemon 与 CLI **同步升级**，不保证旧 CLI 与新 daemon 的兼容性（旧 CLI 不识别 `view_only` 会把回放 tab 误显示为 Running，已知不兼容，无需处理）。

## 待工作计划阶段确认

1. CLI 端"禁用回车提示"的具体交互（弹气泡？输入框变灰？）由 @designer 在工作计划阶段评估定稿。
2. task prompt（来自 `StatusUpdate.user_inputs[0]`）的渲染样式：是否复用主 session 用户消息气泡、是否加 `[task prompt]` 前缀以区分，由 @designer 评估。

