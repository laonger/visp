# 工具调用与结果的差异化显示设计

## 现状与问题

### 当前实现

1. **ToolCall 消息**：来自服务端 `ToolCall` 事件 → `add_tool_line(LineType::ToolCall, args_display, call_id)`
2. **ToolResult 消息**：来自服务端 `ToolResult` 事件 → `insert_tool_result(call_id, "Output/Error: ...")`，结果**追加**到同 `call_id` 的 ToolCall 消息中
3. `LineType::ToolResult` **枚举存在但从未使用**，所有结果都混在 `ToolCall` 里
4. 渲染截断：超过 5 行的工具内容显示 `... [truncated, NNNB]`，不管工具类型

### 核心痛点

1. **所有工具一视同仁**：read_file、bash、edit_file 的结果都显示同样的格式，没有差异化
2. **不该显示的也显示**：read_file 的结果直接在终端里刷出大量文件内容，干扰对话
3. **格式单一丑陋**：工具调用首行黄色、后续灰色，没有统一的美观设计
4. **LineType::ToolResult 被废弃**：为空泛的枚举变体，实际没人用

## 设计目标

1. **统一风格**：所有工具调用采用相同的视觉框架（统一的 BlockStyle 风格、一致的头部格式）
2. **差异化展示**：不同工具类型有不同显示策略（是否显示结果、结果折叠、结果高亮）
3. **语义清晰**：明确区分"调用中"、"已完成"、"有错误"三种状态

## 方案设计

### 1. LineType 拆分

将 `LineType::ToolCall` 拆分为两个独立类型，并添加工具名称元信息：

```rust
pub enum LineType {
    User,
    Assistant,
    Thinking,
    ToolCall { name: String },    // 包含工具名
    ToolResult { name: String },  // 包含工具名，独立于 ToolCall
    ToolError { name: String },   // 工具执行错误
    Error,
    Status,
    Usage,
}
```

**变更说明**：
- `ToolCall` 和 `ToolResult` 成为两条独立的消息，不再混在一起
- 工具名（`name`）作为类型参数，渲染时可据此差异化
- `ChatLine` 的 `call_id` 字段用于关联 Call 和 Result

### 2. 消息流变更

当前：
```
[ToolCall (call_id="tc1")]            ← 调用信息 + 结果全部追加在这里
```

变更后：
```
[ToolCall (call_id="tc1", name="read_file")]    ← 只显示调用参数
[ToolResult (call_id="tc1", name="read_file")]  ← 独立消息，显示结果
```

服务端事件处理（`event.rs`）：
- `ToolCall` 事件 → `add_tool_line(LineType::ToolCall { name }, args_display, call_id)`
- `ToolResult` 事件 → `add_tool_line(LineType::ToolResult { name }, content, call_id)`（新增独立消息）
- 不再调用 `insert_tool_result()` 追加到 ToolCall

### 3. 显示策略矩阵

不同工具类型的不同显示行为：

| 工具 | 调用显示 | 结果显示 | 截断策略 |
|------|----------|----------|----------|
| `read_file` | 显示 `read_file: <path>` | **不显示**（仅显示"OK"或行数） | N/A |
| `edit_file` | 显示 `edit_file: <path>` | 完全显示 diff 结果 | 不截断 |
| `write_file` | 显示 `write_file: <path>` | 完全显示 | 不截断 |
| `grep` | 显示 `grep: <pattern>` | 显示前 N 行 | 截断（最多 20 行） |
| `glob` | 显示 `glob: <pattern>` | 显示匹配文件列表 | 截断（最多 15 行） |
| `bash` | 显示 `bash: <command>` | 显示完整输出 | 截断（最多 30 行） |
| `fetch_web` | 显示 `fetch: <url>` | 显示提取内容 | 截断（最多 20 行） |
| `codegraph_*` | 显示 `codegraph_xxx: <query>` | 显示结果摘要 | 截断（最多 20 行） |

**默认策略**（未知工具）：
- 调用：显示 `name: <参数摘要>`
- 结果：完全显示
- 截断：最多 20 行

### 4. 视觉样式设计

统一的视觉框架，由工具类型决定细节：

```
┌──────────────────────────────────────────┐
│ 🔧 read_file: "src/main.rs"              │  ← 调用行（黄色图标 + 白色工具名）
│ ✓ Loaded 847 bytes (42 lines)            │  ← 结果行（绿色 ✓ + 灰色摘要）
└──────────────────────────────────────────┘

┌──────────────────────────────────────────┐
│ 📝 edit_file: "src/main.rs"              │  ← 调用行
│ --- a/src/main.rs                        │
│ +++ b/src/main.rs                        │  ← 结果内容完全显示
│ @@ -1,5 +1,6 @@                          │
│ ...                                      │
└──────────────────────────────────────────┘

┌──────────────────────────────────────────┐
│ 💻 bash: "cargo test"                    │  ← 调用行
│    Compiling visp-core v0.1.0            │
│    Compiling visp-tools v0.1.0           │  ← 输出（可能截断）
│    Finished test [unoptimized] ...       │
│ ... [truncated, 42 more lines]           │
└──────────────────────────────────────────┘

┌──────────────────────────────────────────┐
│ ❌ read_file: "nonexistent.rs"           │  ← 错误调用
│ Error: No such file or directory         │  ← 错误行（红色）
└──────────────────────────────────────────┘
```

**关键视觉元素**：
1. **工具图标 emoji**：每种工具一个唯一图标，快速区分
2. **工具名 + 参数摘要**：始终显示在第一行
3. **结果状态行**：结果的第一行显示成功/失败状态 + 摘要
4. **结果内容**：差异化显示（包含/不包含/截断）
5. **错误信息**：红色标记，始终完整显示

### 5. 图标映射

| 工具 | 图标 | 含义 |
|------|------|------|
| `read_file` | 📖 | 打开文件 |
| `write_file` | 📝 | 写入文件 |
| `edit_file` | ✏️ | 编辑文件 |
| `grep` | 🔍 | 搜索 |
| `glob` | 📂 | 文件浏览 |
| `bash` | 💻 | 命令执行 |
| `fetch_web` | 🌐 | 网络请求 |
| `codegraph_*` | 🔎 | 代码分析 |
| 未知工具 | 🔧 | 通用工具 |

### 6. BlockStyle 统一

所有工具使用同一套 `BlockStyle`，不再区分 `ToolCall` 和 `ToolResult` 的样式：

```rust
pub const TOOL_STYLE: BlockStyle = BlockStyle {
    margin_vertical: 1,
    margin_horizontal: 1,
    bg_fill: Some(TOOL_BG),        // 深灰色底色
    shadow: true,                  // 阴影
    bottom_pad: 1,                 // 底部留白
};
```

**不再需要** `TOOL_RESULT_STYLE`（与 `TOOL_STYLE` 完全一致）。

### 7. 调用与结果缩进对齐

为了实现视觉上的关联性，ToolResult 可以采用**缩进+无阴影**的风格：

```rust
// ToolCall 的样式（完整）
pub const TOOL_CALL_STYLE: BlockStyle = BlockStyle {
    margin_vertical: 1,
    margin_horizontal: 1,
    bg_fill: Some(TOOL_BG),
    shadow: true,
    bottom_pad: 0,
};

// ToolResult 的样式（缩进 2 格，无阴影，表示从属关系）
pub const TOOL_RESULT_STYLE: BlockStyle = BlockStyle {
    margin_vertical: 0,
    margin_horizontal: 3,    // 缩进 2 格
    bg_fill: Some(TOOL_BG),
    shadow: false,            // 无阴影，视觉上附属于调用
    bottom_pad: 1,
};
```

这样 ToolCall 和 ToolResult 在视觉上形成"主-从"关系。

### 8. 截断策略实现

在 `MessageCache::from_message` 中根据 `LineType` 中的工具名决定截断策略：

```rust
fn max_lines_for_tool(name: &str) -> Option<usize> {
    match name {
        "read_file" => Some(0),       // 不显示内容
        "edit_file" | "write_file" => None,  // 不截断
        "bash" => Some(30),
        "grep" => Some(20),
        "glob" => Some(15),
        "fetch_web" => Some(20),
        _ if name.starts_with("codegraph_") => Some(20),
        _ => Some(20),  // 默认截断
    }
}
```

## 实施计划

### Phase 1: 数据结构变更

1. 修改 `LineType` 枚举，为 `ToolCall` 和 `ToolResult` 添加 `name` 字段
2. 恢复 `ToolResult` 的实际使用（当前被废弃）
3. 修改 `ChatLine` 以支持工具名

### Phase 2: 消息流改造

1. 修改 `event.rs` 中的 `ToolResult` 处理：不再追加到 ToolCall，而是创建独立消息
2. 修改 `AppState::insert_tool_result()` → `add_tool_line(LineType::ToolResult{...}, ...)`
3. 修改服务端/客户端消息传递，确保工具名传递正确

### Phase 3: 渲染改造

1. 在 `theme.rs` 中添加图标映射函数
2. 在 `MessageCache::from_message` 中实现截断策略矩阵
3. 改造 `ToolResult` 的渲染逻辑
4. 调整 BlockStyle 为 ToolCall/ToolResult 差异化

### Phase 4: 测试

1. 测试每种工具类型的显示策略
2. 测试截断逻辑
3. 测试调用-结果关联渲染

## 验证标准

1. `read_file` 结果不显示文件内容，只显示 "Loaded N bytes"
2. `edit_file` / `write_file` 结果完整显示，无截断
3. `bash` 结果最多显示 30 行
4. 每种工具有唯一图标
5. ToolCall 和 ToolResult 视觉上明显区分但有从属关系
6. 错误工具调用显示为红色
