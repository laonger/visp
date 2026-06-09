# UserQuery 选择式确认栏设计

## 1. 目标

将当前 TUI 中工具审批的 `[y/N]` 文本输入方式，改造为**横向可选项选择栏**。选项来源有两种：
- **工具审批**（hardcode）：Approve / Deny / Always Allow
- **LLM 发起的用户选择**（动态）：由 LLM 通过系统 prompt 提供选项列表

支持用户通过方向键导航选中项、按 Enter 确认。额外支持 `Other` 按钮，选中后复用底部输入区输入自定义文本。

## 2. 改动范围

涉及 4 个 crate，按依赖顺序：

```
vbw-proto ──→ vbw-core ──→ vbw-daemon ──→ vbw-cli
  (协议定义)    (核心逻辑)    (消息桥接)      (TUI)
```

| 层次 | 改动内容 |
|------|---------|
| **vbw-proto** | UserQuery 增加 `options` / `allow_other`；UserResponse 改为 `selected_index` + `text` |
| **vbw-core** | AgentEvent::UserQuery 的 respond 从 `Sender<bool>` 改为支持索引和文本；新增 Always Allow 机制 |
| **vbw-core** | Agent 系统 prompt 增加 LLM 发起用户选择的指令 |
| **vbw-core** | Agent 循环新增解析 LLM 特殊输出 → UserQuery 的路径 |
| **vbw-daemon** | 适配新协议；UserResponse 到 respond 的桥接逻辑 |
| **vbw-cli** | 确认栏 UI 重写；事件处理改为方向键导航 + Enter 确认 |

## 3. 模块详细设计

### 3.1 vbw-proto — 协议定义

**UserQuery**（服务端 → 客户端）：

当前 proto 只有 `query_id` / `message` / `session_id`。扩展后：
- `options`：字符串列表。**空列表 = 工具审批模式**（客户端 fallback 为 [Approve, Deny, Always Allow]）；**非空 = LLM 提供选项**。
- `allow_other`：布尔值，是否显示 `Other` 按钮。仅当 true 时出现。

**UserResponse**（客户端 → 服务端）：

当前为 `{ query_id, approved: bool }`。改造为：
- `selected_index`：选中选项的索引。**0 起计数**；**-1 表示用户选择了 Other**。
- `text`：自定义文本。当 `selected_index == -1` 时携带用户输入内容；其他情况可为空。

工具审批场景下，approve = index 0，deny = index 1，always allow = index 2。

### 3.2 vbw-core — 核心逻辑

#### 3.2.1 UserQueryResult 结构体

AgentEvent::UserQuery 的 `respond` 需从 `Sender<bool>` 改为 `Sender<UserQueryResult>`，其中 UserQueryResult 包含：
- `selected_index`：已确认的选项索引
- `text`：自定义文本（非 Other 时可为空）

#### 3.2.2 Always Allow 机制

- 在 `Session` 结构体中增加一个 `approved_tools: HashSet<String>` 字段。
- 当用户选择 "Always Allow" 时，将工具名加入集合。
- 后续同一会话中（跨多次 agent 循环调用），相同工具名再次调用 `requires_approval_for()` 时，直接跳过审批（视为已批准）。
- 存储方式：内存集合，**仅当前会话有效**。不持久化（与 Session 的持久化策略相同）。

#### 3.2.3 Agent 循环改动

当前流程（工具审批）：
```
ToolCall → requires_approval? → UserQuery [respond: Sender<bool>]
  → 等待 respond → 若 false → User Denied；若 true → 执行
```

改造后（在 spawn task 内部，tc.name 可靠可用）：
```
ToolCall → 检查 Session.approved_tools 是否包含 tc.name
  ├── 已存在 → 直接执行（跳过审批）
  └── 不存在
      └── requires_approval_for()?
          ├── false → 直接执行
          └── true → UserQuery [options=[], allow_other=false, respond: Sender<UserQueryResult>]
              ├── index 0 (Approve)      → 执行
              ├── index 1 (Deny)         → User Denied
              └── index 2 (Always Allow) → 写回 Session.approved_tools → 执行
```

#### 3.2.4 LLM 发起的用户选择

**方式**：后处理检测。不在流中实时处理，在 LLM 输出流全部到达后（Done 事件后）对 `text_buffer` 做后处理。

**系统 prompt 新增指令**：告知 LLM 可以在输出的**末尾**附加特殊标记格式向用户提问。LLM 输出类似：
```
...分析过程...
[USER_QUERY allow_other=true]
问题描述
- 选项A描述
- 选项B描述
- 选项C描述
[/USER_QUERY]
```

**后处理流程**（在 Done 之后、决定"无 tool call → done"之前插入）：

1. 检测 `text_buffer` 末尾是否匹配 `[USER_QUERY]...[/USER_QUERY]` 模式
2. 如果匹配：
   - 从 `text_buffer` 中剥离标记部分，将剩余文本作为 Assistant Message 发送
   - 解析出 message（问题描述）、options（`- 选项描述` 行列表）、allow_other（从 `[USER_QUERY allow_other=...]` 提取）
   - 转换为 `AgentEvent::UserQuery`，触发确认栏
   - **只暂停当前迭代的用户查询响应**，不启动新的 LLM 调用
3. 用户响应回来后：
   - 选中选项（非 Other）→ 将选项文本作为 User Message 插入对话历史
   - Other + 自定义文本 → 将自定义文本作为 User Message 插入历史
4. 插入 User Message 后，**重新进入下一轮迭代**（而非继续当前迭代），让 LLM 看到用户的选择继续输出

**不匹配**：按原有逻辑结束当前迭代。

#### 3.2.5 边界情况

- **用户取消（Ctrl+C/Ctrl+D）**：在 confirm 状态下，Ctrl 快捷键应能正常退出或取消。Ctrl+D 应视为退出程序；Ctrl+C 应取消当前查询（相当于 Deny）。
- **空选项列表 + allow_other=true**：工具审批模式不支持 Other，此时客户端应忽略 allow_other。
- **Other 输入为空**：用户选中 Other 但未输入内容直接确认 → 视为取消（Deny）。

### 3.3 vbw-daemon — 消息桥接

当前桥接逻辑：
```
AgentEvent::UserQuery → proto::UserQuery
proto::UserResponse → respond.send(approved)
```

改造后需将 UserQueryResult 中的 `selected_index` 和 `text` 从 proto 映射回 core 类型。

主要改动在 `service.rs` 中 `UserResponse` 处理分支和 `AgentEvent::UserQuery` 转换分支。

### 3.4 vbw-cli — TUI 改造

#### 3.4.1 确认栏 UI

当前渲染（一行文字）：
```
❓ Allow tool: Bash(rm -rf /tmp)? [y/N]
```

改造后（两行）：
```
┌──────────────────────────────────────────────────────────────┐
│ ❓ Allow tool: Bash(rm -rf /tmp)?                            │
│  [A] Approve  [B] Deny  [C] Always Allow                    │  ← 高亮当前选项
└──────────────────────────────────────────────────────────────┘
```

- 第一行：消息文字（左侧 `❓` 前缀 + 白色文字）
- 第二行：选项行。每个选项格式 `[字母] 选项名`，当前选中选项反色/高亮
- ←/→ 方向键在选项间移动高亮
- Enter 确认选中项
- Other 选项（如有）是最后一个，高亮时底部输入区变为可编辑

#### 3.4.2 交互状态

引入 `ConfirmState` 的扩展：

```rust
pub struct ConfirmState {
    pub query_id: String,
    pub message: String,
    pub options: Vec<String>,       // LLM 提供的选项；空=工具审批模式
    pub allow_other: bool,          // 是否显示 Other
    pub selected_index: usize,      // 当前高亮的选项索引
    pub other_active: bool,         // 是否处于 Other 输入模式
}
```

**Other 输入模式**：
- 当 `other_active = true` 时，输入区从不可编辑变为可编辑状态，**覆盖 `generating` 状态的锁定**（即使 `generating==true`，渲染时也按可编辑处理）
- 用户输入文本后按 Enter → 发送 `UserResponse { selected_index: -1, text: "自定义内容" }`
- Other 模式下按 Esc → 退出 Other 模式，回到导航，清空输入区

#### 3.4.3 键盘事件

| 状态 | 按键 | 行为 |
|------|------|------|
| 导航中 | ← / → | 移动 selected_index（选项超宽时同步滚动可见区域） |
| 导航中 | Enter | 确认当前选中项 |
| 导航中 | Esc | 相当于 Deny |
| Other 模式 | 任意文字键 | 输入到输入区 |
| Other 模式 | Enter | 提交自定义文本 |
| Other 模式 | Esc | 退出 Other 模式，回到导航 |

#### 3.4.4 底部区域布局

当 `confirm.is_some()` 时，底部区域布局变为：

```
┌───────────────────────────────┐  ← 确认栏（2行）
├───────────────────────────────┤  ← 输入区（可变高度，Other 时可编辑）
├───────────────────────────────┤  ← 分隔线（1行）
├───────────────────────────────┤  ← 状态栏（1行）
```

当前输入区高度从 2 调整为可变高度，以适配 Other 模式下的多行输入。

#### 3.4.5 边界情况

- **窗口 resize**：确认栏渲染应适应宽度。检测选项行总宽度是否超过可用宽度；超出时使用可滚动方案，←/→ 方向键同时滚动选项列表，首尾显示 `<` `>` 指示可滚动方向。
- **退出确认状态**：用户不应在无确认状态时看到确认栏。
- **Other 模式下的滚动/历史**：Other 输入也应支持输入历史（←/↑ 切换历史）。

## 4. 依赖关系

```
         vbw-proto
            │
            ▼
         vbw-core  ◄── 新增 AlwaysAllow 状态
            │
            ▼
       vbw-daemon  ◄── 映射 proto ↔ core 事件
            │
            ▼
         vbw-cli   ◄── TUI 渲染 + 事件处理
```

各层向后兼容策略（可选）：
- `UserResponse` 的旧版 `approved` 字段可保留为 deprecated，old 客户端仍正常工作
- 但鉴于这是开发中项目，建议直接替换，不做兼容层

## 5. 核心数据流

### 场景 A：工具审批

```
Agent 循环
  │ ToolCall (bash, requires_approval=true)
  ▼
检查 approved_tools HashSet 是否包含 "bash"
  │
  ├── 已存在 → 直接执行，跳过审批
  │
  └── 不存在
      ▼
  UserQuery { options=[], allow_other=false }
      │  gRPC ▼
      客户端渲染选择栏 [Approve] [Deny] [Always Allow]
      │  用户选择
      ▼ gRPC ▲
  UserResponse { selected_index, text="" }
      │
      ▼
  根据 index:
    · 0 (Approve)      → 执行工具
    · 1 (Deny)         → 返回 User Denied
    · 2 (Always Allow) → 加入 HashSet → 执行工具
```

### 场景 B：LLM 发起用户选择

```
Agent 循环第 N 轮迭代
  │ LLM 输出流 (TextDelta 实时转发给客户端)
  ▼
  Done 到达，收集完成 text_buffer
  │
  ▼
  检测 text_buffer 末尾是否含 [USER_QUERY]...[/USER_QUERY]
  │
  ├── 不匹配 → 正常结束本轮迭代（或继续处理 tool_calls）
  │
  └── 匹配 → 从 text_buffer 剥离标记，剩余文本作为 Assistant Message
      │
      ▼
      UserQuery { options=[...], allow_other=true/false }
      │  gRPC ▼
      客户端渲染选择栏 [options...] [Other]
      │  用户选择
      ▼ gRPC ▲
      UserResponse { selected_index, text }
      │
      ▼
      · 选中选项 → 选项文本作为 User Message 插入历史
      · Other     → 自定义文本作为 User Message 插入历史
      │
      ▼
      下一轮迭代（LLM 看到用户选择后继续）
```

## 6. 不做什么

- ❌ 不持久化 Always Allow 状态（仅当前会话内存中保留）
- ❌ 不做触控/鼠标选择确认栏选项（保持键盘交互）
- ❌ 不支持嵌套确认（一个确认完成前不会出现第二个）
- ❌ 不改动现有的 gRPC Chat 流架构
- ❌ 不添加 LLM 工具调用确认之外的审批类型

## 7. 验收标准

1. **工具审批**：选择 Approve → 工具执行；Deny → 返回错误；Always Allow → 当前会话同工具不再审批
2. **LLM 选择**：选项正确渲染，选中后以 User Message 形式回传给 LLM
3. **Other 输入**：allow_other=true 显示 Other 按钮，选中后输入区可编辑，提交后内容回传
4. **方向键导航**：←/→ 在选项间循环移动高亮
5. **Esc 退出**：导航状态下按 Esc 相当于 Deny；Other 模式下按 Esc 退回导航
6. **空选项列表**：fallback 为 [Approve, Deny, Always Allow]
7. **Ctrl+C 取消**：与当前 Ctrl+C 行为一致，取消正在进行的操作
8. **所有现有测试通过**：`cargo test` 全部 green
9. **Clippy 零警告**：`cargo clippy -- -D warnings` 通过
