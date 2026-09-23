# visp 工作计划：visp-acp（ACP agent 接入 M1）

> 输入：[`docs/design/visp-design-acp.md`](../design/visp-design-acp.md)（已定稿，§13 十项 2026-09-23 逐项确认）
> 范围：§13.10 core 前置修复 + M1 全量（`initialize`/`session/new`/`session/prompt`/`session/cancel` + 审批桥接 + elicitation）+ 集成测试
> 不含：`session/load`、`fs/*`、`terminal/*`、`mcpServers[]` 转发、`usage_update`（M2/M3）
> 质量门：`cargo test && cargo clippy -- -D warnings && cargo fmt -- --check`（与 CI 一致）
> 状态：**待用户审核**

---

## 概述

新增独立二进制 `visp-acp`：ACP over stdio（官方 crate `agent-client-protocol` 2.2.0，锁版本），内部以 gRPC 连接（默认自行拉起）`visp-daemon`。M1 前先修 core 取消收尾缺陷（§13.10 方案 A）。事件翻译为纯函数便于单测；端到端测试以「真 daemon + MockProvider」为主，未就绪前用 gRPC stub 驱动适配器级测试。

## 步骤 1：core 取消收尾缺陷修复（前置，§13.10）

### 1a：LLM 提问等待中取消的收尾补齐（visp-core）
#### 🔴 红 — 测试
| # | 测试用例 |
|---|---|
| 1 | LLM 提问等待中触发 cancel → 收到 `Error{Cancelled}` 事件 |
| 2 | 同场景 → `finish_loop` 被调用，会话状态最终为 `Error` |
| 3 | 回归：审批等待中取消（V5 路径）行为不变 |
| 4 | 回归：流式/重试路径取消行为不变 |
#### 🟢 绿 — 实现
`agent_loop.rs:1835-1841` 的 `select!` 取消分支补发 `Error{Cancelled}` + `finish_loop(Error)`（约两行，与其他三条取消路径对齐）。
#### 🧪 测试 → 🔍 类型检查
`cargo test -p visp-core && cargo test -p visp-daemon && cargo clippy -p visp-core -- -D warnings`
#### 📦 提交
`fix(core): 补齐 UserQuery 等待中取消的收尾事件与会话状态`

## 步骤 2：crate 骨架与 SDK 冒烟（visp-acp）

### 2a：workspace 成员 + 参数解析 + stderr 日志
#### 🔴 红 — 测试
| # | 测试用例 |
|---|---|
| 1 | `--project` 默认当前目录 |
| 2 | `--addr` 缺省 `[::1]:50051`；非法地址报错退出 |
| 3 | `--config-dir` 透传 daemon 并导出 `VISP_CONFIG_DIR` |
| 4 | `--shutdown-on-exit` 默认 false |
#### 🟢 绿 — 实现
workspace 加入 `crates/visp-acp`；`Cargo.toml` 锁 `agent-client-protocol = "2.2.0"`；clap 参数解析；日志全走 stderr。
#### 🧪 测试 → 🔍 类型检查
`cargo test -p visp-acp && cargo clippy -p visp-acp -- -D warnings`
#### 📦 提交
`feat(acp): visp-acp crate 骨架与参数解析`

### 2b：SDK 冒烟（最小 agent）
#### 🔴 红 — 测试
| # | 测试用例 |
|---|---|
| 1 | initialize 请求 → 返回 protocolVersion v1 + agentInfo + `authMethods: []` |
| 2 | stdout 输出逐行均为合法 JSON-RPC（日志零污染） |
| 3 | 未 initialize 前收到 `session/new` → JSON-RPC error |
#### 🟢 绿 — 实现
最小 ACP agent 事件循环（SDK role + handler API），仅应答 initialize——落地 §12「先冒烟」风险缓解。
#### 🧪 测试 → 🔍 类型检查
`cargo test -p visp-acp`
#### 📦 提交
`feat(acp): SDK 冒烟——最小 agent 与 stdout 纪律基线`

## 步骤 3：daemon 编排器（visp-acp，模块 supervisor）

### 3a：端口探测
#### 🔴 红 — 测试
| # | 测试用例 |
|---|---|
| 1 | base 端口被占 → 递增探测成功（移植 launcher `find_available_addr`） |
| 2 | 1000 个端口全占 → 报错退出 |
#### 🟢 绿 — 实现
地址解析/格式化/探测函数移植（`visp/src/main.rs:207-262`）。
#### 📦 提交
`feat(acp): 端口探测`

### 3b：spawn 与日志重定向
#### 🔴 红 — 测试
| # | 测试用例 |
|---|---|
| 1 | spawn 注入 `VISP_LISTEN_ADDR` |
| 2 | daemon stdout/stderr 重定向到日志文件（不污染 ACP stdout） |
| 3 | `--config-dir` 透传 |
#### 🟢 绿 — 实现
`Command` 组装（对齐 launcher 做法）。
#### 📦 提交
`feat(acp): daemon spawn 与输出重定向`

### 3c：health check + 子进程存活监控
#### 🔴 红 — 测试
| # | 测试用例 |
|---|---|
| 1 | 轮询 HealthCheck 至 `alive` 为止（500ms 间隔，15s 超时） |
| 2 | 轮询期间子进程已退出（模拟绑定失败）→ 立即报错退出并读 `startup_error_file()`，**不**继续对探测端口 health check |
| 3 | 15s 超时 → 报错退出并杀子进程 |
#### 🟢 绿 — 实现
`wait_for_health` + 每轮 `try_wait()`（§6.1 步骤 3，TOCTOU 防御）。
#### 📦 提交
`feat(acp): health check 与子进程存活监控`

### 3d：退出收尾（按模式区分）
#### 🔴 红 — 测试
| # | 测试用例 |
|---|---|
| 1 | 自拉起退出 → 发 `Shutdown`，超时强杀子进程 |
| 2 | 直连退出（无 flag）→ 不发 `Shutdown`、不强杀 |
| 3 | 直连 + `--shutdown-on-exit` → 发 `Shutdown` |
#### 🟢 绿 — 实现
§6.1 步骤 6 的模式区分收尾。
#### 📦 提交
`feat(acp): 按启动模式区分的退出收尾`

## 步骤 4：gRPC 会话传输层（visp-acp，模块 grpc）

### 4a：唯一 Chat 流
#### 🔴 红 — 测试
| # | 测试用例 |
|---|---|
| 1 | 建流成功；同进程重复建流被拒（单例语义，V1） |
| 2 | daemon 流断开 → 入站通道关闭事件上抛 |
#### 🟢 绿 — 实现
`CoderDaemonClient` 连接封装、出站 mpsc sender、入站转发任务。
#### 📦 提交
`feat(acp): Chat 流传输层`

### 4b：出站消息封装
#### 🔴 红 — 测试
| # | 测试用例 |
|---|---|
| 1 | `UserInput`/`Cancel`/`UserResponse`/`Ack` 编码与发送正确 |
| 2 | 流已断开时发送 → 明确错误上抛 |
#### 🟢 绿 — 实现
出站 API 薄封装。
#### 📦 提交
`feat(acp): 出站消息封装`

## 步骤 5：事件翻译器（visp-acp，模块 translate，纯函数）

### 5a：ServerMessage 全变体映射
#### 🔴 红 — 测试
| # | 测试用例 |
|---|---|
| 1 | `TextDelta`（父）→ `agent_message_chunk`，messageId 稳定 |
| 2 | `TextDelta`（子）→ None（M1 抑制） |
| 3 | `ThinkingBlock`（父）→ `agent_thought_chunk`；（子）→ None |
| 4 | `ToolCall` → `tool_call` pending + kind 映射（read/search/edit/execute/fetch/other）+ locations 绝对路径 1-based |
| 5 | `ToolResult` → `tool_call_update` completed/failed + 超长内容截断（保留首尾） |
| 6 | `UserQuery`（options 空）→ 审批桥接事件；（非空）→ 提问桥接事件 |
| 7 | `Done`（父 session）→ end_turn 终止；（子 session）→ None |
| 8 | `Error` code=`Operation cancelled`（父）→ cancelled 终止 |
| 9 | `Error` 其他（父）→ refusal 终止 + 错误文本下发；（子）→ None（不终止父 turn） |
| 10 | `UsageInfo`/`UsageDelta` → M1 忽略；`StatusUpdate` → 文本 chunk；`ImageBlock`/`ImageError`/`UserMessage` → M1 忽略（M2 再启用） |
| 11 | messageId 切换：`agent_name` 变化或工具帧插入时更换（近似策略） |
#### 🟢 绿 — 实现
纯函数 `fn translate(msg, ctx) -> Vec<Outbound>`；终止判定只用 `session_id`（V9：主 agent `agent_name` 实为 `"default"` 非空）；取消判定双向匹配 code/message（§6.6）。
#### 🧪 测试 → 🔍 类型检查
`cargo test -p visp-acp`（表驱动用例）
#### 📦 提交
`feat(acp): 事件翻译器——ServerMessage 全变体映射`

## 步骤 6：会话注册表与归属路由（visp-acp，模块 sessions）

### 6a：注册表与在途 prompt 状态机
#### 🔴 红 — 测试
| # | 测试用例 |
|---|---|
| 1 | `session/new` 登记 sessionId ↔ visp session_id |
| 2 | 同会话并发 prompt → 第二个被拒（JSON-RPC error） |
| 3 | 不同会话并发 prompt → M1 拒绝（单在途父约束） |
| 4 | turn 收尾后状态复位；`session/delete`（M3 占位）清理条目 |
#### 🟢 绿 — 实现
注册表 + 每 session 在途状态机。
#### 📦 提交
`feat(acp): 会话注册表与在途状态机`

### 6b：归属路由
#### 🔴 红 — 测试
| # | 测试用例 |
|---|---|
| 1 | 在途期间 `session_id == 父` → 父事件；`≠ 父` → 归入当前父（子事件） |
| 2 | 无在途 prompt 的迟到事件（子晚于父 Done / 兜底后迟到父 Done/Error）→ 丢弃 + stderr 警告 |
| 3 | 未知 session → 丢弃 + 警告 |
| 4 | **`UserQuery` 任何归属 → 旁路直接进审批/提问桥接**（§8.4 特例） |
#### 🟢 绿 — 实现
按 §8.4 规则 2（三轮修正：主判据只有 `session_id`）。
#### 📦 提交
`feat(acp): 事件归属路由与 UserQuery 旁路`

## 步骤 7：审批与提问桥接（visp-acp，模块 approval）

### 7a：工具审批（request_permission）
#### 🔴 红 — 测试
| # | 测试用例 |
|---|---|
| 1 | 发 `session/request_permission` 携带 toolCallId/kind/标题 |
| 2 | `selected` allow_once/allow_always/reject_once → 回填 0/2/1 |
| 3 | `selected` reject_always → 降级回填 1（§13.5 已拍板） |
| 4 | `cancelled` → 回填 -1；**turn 收尾按归属区分**：父审批 ≈ 取消中（看信号）、子审批仅等价拒绝（§6.5 三轮修正） |
| 5 | 迟到 UserResponse（query_id 已被 daemon 消费）→ 忽略不报错 |
#### 🟢 绿 — 实现
V4 索引映射表 + 回填通道（经 grpc 层）。
#### 📦 提交
`feat(acp): 工具审批桥接与索引映射`

### 7b：LLM 提问（elicitation）
#### 🔴 红 — 测试
| # | 测试用例 |
|---|---|
| 1 | client 声明 `elicitation.form` → 发 `elicitation/create`（枚举值=选项原文，`allow_other` 附自由文本字段） |
| 2 | accept 选中第 i 项 → 反查回填 `selected_index = i` |
| 3 | accept 自由文本 → 回填 `-1, text`（自由文本优先于枚举字段） |
| 4 | decline/cancel → 回填 `-1` |
| 5 | client 未声明 `elicitation.form` → 降级：问题文本 chunk + 立即回填 `-1, text=说明` |
| 6 | 任何路径均回填（V12 无超时，不回填 = 子循环永久阻塞） |
#### 🟢 绿 — 实现
elicitation 桥接 + 能力检查 + 降级路径（schema 约定按 §6.4 步骤 7 三轮补充）。
#### 📦 提交
`feat(acp): LLM 提问 elicitation 桥接与降级`

## 步骤 8：ACP agent 层组装（visp-acp，模块 agent）

### 8a：initialize + session/new
#### 🔴 红 — 测试
| # | 测试用例 |
|---|---|
| 1 | caps 为 M1 集合：不声明 `loadSession`/`fs`/`terminal`/`image`；协议只 v1 |
| 2 | 记录 client `elicitation.form` 能力供 7b 使用 |
| 3 | `session/new` → `CreateSession(project_path = cwd)`（cwd 优先于 `--project`）；visp id 直接作 ACP sessionId；`mcpServers[]` 忽略 |
| 4 | `CreateSession` 失败 → JSON-RPC error 不崩溃 |
#### 🟢 绿 — 实现
两 handler + 注册表登记。
#### 📦 提交
`feat(acp): initialize 与 session/new`

### 8b：session/prompt 主流程
#### 🔴 红 — 测试
| # | 测试用例 |
|---|---|
| 1 | happy path：content blocks 拼接（多 text block 顺序拼接；非文本块报错）→ `UserInput` → chunk 流式 → `Done` → `end_turn` |
| 2 | 事件泵按 §7.2 翻译下发；审批/提问时暂停等待（经步骤 7 桥接） |
| 3 | `session/cancel` 中断 → `Error{Cancelled}` → `cancelled`（不发 JSON-RPC error，A7） |
| 4 | LLM 提问等待中取消 → 兜底 10s 超时自行 `cancelled`（core 修复落地后由 `Error{Cancelled}` 直达，兜底保留为防御层） |
| 5 | 未知 sessionId → JSON-RPC error |
#### 🟢 绿 — 实现
事件泵 + 终止状态机（组装 4/5/6/7）。
#### 📦 提交
`feat(acp): session/prompt 主流程与兜底收尾`

### 8c：session/cancel
#### 🔴 红 — 测试
| # | 测试用例 |
|---|---|
| 1 | 有在途 → 转发 `Cancel`；turn 以 `cancelled` 收尾（经 8b 状态机） |
| 2 | 无在途 → no-op（daemon 侧天然 no-op） |
#### 📦 提交
`feat(acp): session/cancel`

## 步骤 9：集成测试与收尾

### 9a：daemon 编排负路径（真 daemon 二进制）
| # | 测试用例 |
|---|---|
| 1 | 预占探测端口后启动 → 报错退出，不误连该端口既有 daemon |
| 2 | `--addr` 直连退出 → 断言未发送 `Shutdown` |
| 3 | 自拉起退出 → 断言 daemon 已关闭 |

### 9b：语义负路径（gRPC stub / MockProvider 夹具）
| # | 测试用例 |
|---|---|
| 1 | LLM 提问等待中取消 → `stopReason: cancelled`，不悬挂 |
| 2 | 子 agent 中途失败 → 父 turn 不提前终止、最终正常 `Done`、子 `Error` 仅展示 |
| 3 | 子 agent 触发审批 → 弹窗可见且回填生效 |
| 4 | `/` 前缀输入 → 命令劫持行为与 §8 记录一致 |
| 5 | 全程 stdout 仅合法 ACP 消息 |

> 夹具策略：适配器级测试用轻量 gRPC stub（脚本化回放 `ServerMessage`）；端到端以「真 daemon + MockProvider」为主——若需跨 crate 复用 `service.rs:1495+` 的 MockProvider，给 visp-daemon 加 `test-mock` feature（实现时评估，不预建抽象）。

### 9c：全量回归 + Zed 手测
```bash
cargo test && cargo clippy --workspace -- -D warnings && cargo fmt -- --check
```
Zed 自定义 agent 手测验收：对话、看到工具调用、审批、中断、回答 LLM 提问。
#### 📦 提交
`test(acp): 集成测试与回归`

---

## Wave 并行策略

### Wave 1（2 个并行任务）
- 任务 A（visp-core）：步骤 1（1a）
- 任务 B（visp-acp）：步骤 2（2a → 2b）

### Wave 2（3 个并行任务，均依赖 Wave 1 的任务 B）
- 任务 A：步骤 3（3a → 3b → 3c → 3d，daemon 编排）
- 任务 B：步骤 4（4a → 4b，gRPC 传输层）
- 任务 C：步骤 5（5a，事件翻译器纯函数）

### Wave 3（2 个并行任务，依赖 Wave 2）
- 任务 A：步骤 6（6a → 6b，注册表与归属）
- 任务 B：步骤 7（7a → 7b，审批与提问桥接，依赖 4/5）

### Wave 4（串行，依赖 Wave 3 全部）
- 步骤 8（8a → 8b → 8c，agent 层组装）

### Wave 5（串行，依赖 Wave 4）
- 步骤 9（9a → 9b → 9c；9c 的 Zed 手测为人工验收）

## 依赖关系总览

```
Step1(core) ──────────────────────────┐（并行，互不依赖）
Step2(骨架/冒烟) ──┬─ Step3(编排) ─┬──┤
                   ├─ Step4(gRPC) ─┤  │
                   └─ Step5(翻译) ─┤  │
                                   ├── Step8(agent组装) ── Step9(集成/回归)
                   Step6(注册表) ──┤
                   Step7(桥接) ────┘
```

## 测试覆盖汇总

| Wave | 并行数 | 模块/包 | 步骤 | 测试用例数 |
|---|---|---|---|---|
| 1 | 2 | visp-core · visp-acp | 1、2 | 4+7 |
| 2 | 3 | visp-acp | 3、4、5 | 10+4+11 |
| 3 | 2 | visp-acp | 6、7 | 8+11 |
| 4 | 1 | visp-acp | 8 | 11 |
| 5 | 1 | visp-acp/tests | 9 | 8 + 全量回归 |

## 备注

1. **SDK 版本**：`agent-client-protocol` 锁 2.2.0；官网文档可能滞后，以 docs.rs/仓库 README 为准（§4.2）。
2. **判据纪律**：归属/终止/路由不得依赖 proto 注释级事实（如「空 `agent_name` = 主 agent」），以集成测试断言实际字段值为准（§11 通用原则）。
3. **`reject_always`/`max_tokens`**：本期降级，已登记 `docs/todo/TODO.md`（P2 ×2），不进 M1。
4. **M2 前置备忘**：`session/load` 需持久派生 config-dir + `visp-db` WAL/busy_timeout（§4.3 演进路径），M2 立项时先做。
5. **人工依赖**：9c 的 Zed 手测需真实 Zed 环境；自动化部分完成后即可开始。
6. **SDK API 事实（lib-1 调研，基于 2.2.0 + schema 1.9.1 发布包源码逐行核对，实现依据）**：
   - **形态**：2.x 无 `Agent` trait/`async_trait`；是 role 单元结构体 + Builder + 原生 async 闭包：`Agent.builder().name("visp-acp").on_receive_request(async move |req: T, responder, cx| {...}, agent_client_protocol::on_receive_request!()).connect_to(transport)`。Step 2b 已落地（`run_agent`）。
   - **单任务事件循环**：handler 执行期间收不到新消息；**长工作必须 `cx.spawn(async move {...})` 卸载**（session/prompt 事件泵属此类，Step 8 落地时必须遵守）。
   - **传输**：stdio = `connect_to(Stdio::new())`；测试/内存 = `ByteStreams::new(outgoing, incoming)`，其泛型是 **futures 的** AsyncWrite/AsyncRead——tokio 类型需 `tokio-util::compat` 桥（`write.compat_write()` / `read.compat()`，依赖已加）。
   - **Client→Agent 请求**：`initialize`、`session/new`(NewSessionRequest)、`session/prompt`(PromptRequest{session_id, prompt: Vec<ContentBlock>})、`session/cancel`(通知 CancelNotification)；未注册 → SDK 自动回 Method not found；SDK 内置 v1 守卫（initialize 必须首条请求）。
   - **Agent→Client**：通知 `cx.send_notification(SessionNotification::new(session_id, update))`；请求 `cx.send_request(...)` 返回 `SentRequest`——**Drop 自动发 `$/cancel_request`**，句柄必须消费或 `.detach()`；等待响应用 `.block_task().await`（勿在 dispatch 循环内 await 自己的请求 → 死锁，配合 `cx.spawn`）。
   - **SessionUpdate**（`#[non_exhaustive]`，match 需通配）：`UserMessageChunk/AgentMessageChunk/AgentThoughtChunk(ContentChunk{content, message_id: Option<MessageId>})`、`ToolCall(ToolCall)/ToolCallUpdate(ToolCallUpdate)`、`Plan`、`CurrentModeUpdate`、`UsageUpdate` 等；messageId 策略可用 `ContentChunk.message_id` 原生承载。
   - **工具调用**：`ToolCall::new(tool_call_id, title).kind(ToolKind).status(ToolCallStatus)`；`ToolCallUpdate::new(id, ToolCallUpdateFields)`；`ToolKind{Read,Edit,Delete,Move,Search,Execute,Think,Fetch,SwitchMode,Other}`；`ToolCallStatus{Pending,InProgress,Completed,Failed}`。
   - **权限**：`RequestPermissionRequest::new(session_id, ToolCallUpdate, Vec<PermissionOption>)`；`PermissionOption::new(option_id, name, kind)`，`PermissionOptionKind{AllowOnce,AllowAlways,RejectOnce,RejectAlways}`；响应 `RequestPermissionResponse.outcome: RequestPermissionOutcome{Cancelled, Selected(sel)}`，取 `sel.option_id`（与 visp 索引映射对照：AllowOnce→0、AllowAlways→2、RejectOnce/RejectAlways→1、Cancelled→-1）。
   - **Elicitation**：`CreateElicitationRequest::new(mode, message)`，`ElicitationMode::Form(ElicitationFormMode{scope, requested_schema})`；响应 `ElicitationAction{Accept{content: BTreeMap<String, ElicitationContentValue>}, Decline, Cancel}`——§6.4 步骤 7 的「选项原文反查索引、自由文本优先」作用于 content map。
   - **StopReason**：`{EndTurn, MaxTokens, MaxTurnRequests, Refusal, Cancelled}`，wire snake_case；`PromptResponse::new(reason)`。
   - **initialize**：`ProtocolVersion` 为 newtype(u16)（wire 数字 1），标准做法**回显** `req.protocol_version`；client 能力判定 `req.client_capabilities.elicitation.as_ref().is_some_and(|e| e.supports_form())`；响应 `InitializeResponse::new(pv).agent_capabilities(AgentCapabilities::new()).agent_info(Implementation::new("visp", version))`。
   - **坑**：schema 全 `#[non_exhaustive]`；docs.rs 是 all-features 构建，v2/unstable 内容勿照抄；v1 稳定 wire 协议 2.0 起未变。
