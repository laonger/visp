# visp Phase 4 阶段设计：CLI 前端

## 1. 阶段目标

实现终端交互界面，用户可通过 `vbw` 命令连接 daemon，在 REPL 中进行多轮对话。

**一句话总结**：`vbw` 启动 → 连接 daemon → 输入 prompt → 流式显示 LLM 响应 → 支持工具调用和 Human-in-Loop 确认。

## 2. 模块划分

Phase 4 涉及一个新 crate：

| Crate | 职责 | 类型 |
|---|---|---|
| **visp-cli** | gRPC 客户端、REPL 交互、流式显示 | 新建 |

依赖 Phase 3 的 daemon（gRPC 服务端已就绪）。

### 2.1 visp-cli crate

#### 2.1.1 模块结构

```
visp-cli/
├── Cargo.toml
└── src/
    ├── main.rs       # 入口：解析 CLI 参数、建立连接、启动 REPL
    ├── client.rs     # gRPC 客户端封装（连接、会话管理、Chat 流）
    ├── repl.rs       # REPL 循环（readline 输入、发送消息、处理响应）
    └── display.rs    # 终端格式化输出（流式文本、工具通知、错误、提示）
```

**依赖**：
- `visp-proto`：gRPC 类型和 service client
- `tonic`：gRPC 客户端
- `tokio`：异步运行时
- `rustyline` 或 `dialoguer`：REPL 输入（支持历史、编辑）
- `clap` (derive)：CLI 参数解析

#### 2.1.2 CLI 参数（`main.rs`）

| 参数 | 默认值 | 说明 |
|---|---|---|
| `-a, --addr` | `[::1]:50051` | daemon 监听地址 |
| `-p, --project` | 当前工作目录 | 项目路径 |
| `-c, --config` | 无 | daemon 配置文件路径 |
| `--model` | 无 | 覆盖模型名称 |
| `--temperature` | 无 | 覆盖温度参数 |

**启动流程**：
1. 解析 CLI 参数
2. 建立 gRPC 连接
3. 若连接失败 → 报清晰提示后退出（不自动启动 daemon）
4. 检查 daemon 健康状态（HealthCheck RPC）
5. 创建会话（CreateSession RPC）
6. 启动 REPL 循环

#### 2.1.3 gRPC 客户端（`client.rs`）

封装与 daemon 的通信：

- `VbwClient::connect(addr)` — 建立 gRPC 连接
- `health_check()` — 检查 daemon 存活
- `create_session(project_path)` — 创建会话，返回 session_id
- `chat(session_id)` — 返回 `ChatHandle`（双向流包装）

`ChatHandle` 封装 Chat 双向流，内部持有分离后的 sender 和 receiver：

- 发送方法（非阻塞）：`send_input(text)`、`send_response(query_id, approved)`、`send_cancel()`
- 接收方法：`recv() -> Option<ServerMessage>`（流式读取下一个消息）

使用 tonic 的 `Streaming` split 机制分离 sender/receiver，使 select! 中可同时 await 多个来源而不互斥。

#### 2.1.4 REPL 循环（`repl.rs`）

```
REPL 主循环 (使用 tokio::select!):
  loop {
    select! {
      // 分支 1：用户键盘输入
      input = readline("> ") => {
        若以 / 开头 → 处理特殊命令
        否则 → chat_handle.send_input(text)
      }
      // 分支 2：gRPC 响应流
      msg = chat_handle.recv() => {
        match msg.payload {
          TextDelta → display::print_streaming(delta)
          ToolCall → display::print_tool_call(name, args)
          ToolResult → display::print_tool_result(content, is_error)
          StatusUpdate → display::print_status(message)
          UserQuery →
            display::print_query(message)
            切换输入模式为 [y/N]
            (下一轮 readline 读取用户回答)
            chat_handle.send_response(query_id, approved)
          Error → display::print_error(code, message)
          Done → display::print_done()
        }
      }
      // 分支 3：Ctrl+C
      _ = tokio::signal::ctrl_c() => {
        若 Agent 正在运行 → chat_handle.send_cancel()
        连续两次 → 直接退出
        否则 → 忽略
      }
    }
  }
```

**特殊命令**（以 `/` 开头）：
- `/quit` 或 `/exit` — 退出 REPL
- `/clear` — 清除屏幕
- `/temp 0.3` — 修改温度（发送 ConfigUpdate）
- `/model claude-sonnet-4` — 切换模型
- `/help` — 显示帮助

**Ctrl+C 处理**：
- 用户按 Ctrl+C 时，CLI 捕获信号，**不**退出进程
- 若 Agent 正在运行中：CLI 通过 Chat 流发送 `Cancel` 消息通知 daemon 取消当前 Agent 循环
- daemon 收到 Cancel 后触发 CancellationToken，Agent 循环退出，返回 Error(Cancelled)
- 之后恢复正常 REPL prompt，等待下一条输入
- 连续两次 Ctrl+C → 直接退出

**协议扩展**（Phase 3 proto 需追加）：
- `ClientMessage` oneof 新增 `Cancel cancel = 4;`
- 新增消息：`message Cancel { string session_id = 1; }`
- daemon Chat handler 在循环中同时监听 `Cancel` 消息和 UserInput

**流式输出期间输入提示**：
- Agent 正在生成响应时，输入区显示 `[Generating...]`（非阻塞提示）
- UserQuery 等待确认时，输入区切换为 `[y/N]` 提示等待输入

**工具结果截断**：
- daemon 已截断到 100KB，CLI 在显示时进一步截断到 2000 字符
- 超出部分显示 `... [truncated, N bytes total]`

#### 2.1.5 终端显示（`display.rs`）

格式化输出到终端：

**流式文本**：实时打印（`print!` 不加换行），display 模块维护 `line_has_output: bool` 状态。

换行规则：
- 收到 TextDelta → `print!(delta)`，不换行
- 收到非 TextDelta 事件（ToolCall/ToolResult/Error/Done）→ 若当前行有输出，先 `println!()` 换行，再输出该事件

**工具调用通知**：
```
🔧 read_file(src/lib.rs)
```

**工具结果**：
```
📄 [100 lines] ...
```

**Human-in-Loop 确认**：
```
❓ 是否允许执行: rm -rf node_modules? [y/N]
```

**错误**：
```
❌ Error: Session not found
```

**完成标记**：
```
✓ Done
```

**设计决策**：
- MVP 不做颜色输出（后续可加），使用 emoji 前缀区分消息类型
- 流式文本直接 `print!` 到 stdout，不使用缓冲
- 工具结果 > 200 字符时截断

## 3. 依赖关系

```
visp-cli (二进制)
    ├──→ visp-proto (gRPC 类型 + client)
    ├──→ tonic (gRPC)
    ├──→ tokio (异步)
    ├──→ clap (CLI 参数)
    └──→ rustyline (REPL 输入)
```

visp-cli 只依赖 visp-proto（通过 gRPC 协议与 daemon 通信），不依赖 visp-core/visp-llm/visp-tools。

## 4. 核心数据流

```
用户输入 "重构 src/lib.rs"
    │
    ▼
REPL → client.send_input(text)
    │
    ▼
gRPC Chat 双向流
    │ UserInput(text, session_id)
    ▼
Daemon (Phase 3) → Agent Loop → LLM + Tools
    │
    ▼
gRPC Chat 流返回 ServerMessages
    │
    ▼
REPL 循环接收:
    TextDelta ──→ "好的，让我先看看这个文件..."
    ToolCall ──→ 🔧 read_file(src/lib.rs)
    ToolResult ──→ 📄 [file content]
    TextDelta ──→ "重构完成，主要改动:"
    Done ──→ ✓
    │
    ▼
回到 prompt "> "
```

## 5. 不做什么

- ❌ VSCode 插件
- ❌ 非 REPL 模式（管道输入、脚本文件）
- ❌ 多会话管理
- ❌ 终端颜色主题
- ❌ 会话历史持久化
- ❌ 自动补全
- ❌ 代码高亮

## 6. 验收标准

- `cargo build -p visp-cli` 编译通过
- `cargo clippy -p visp-cli -- -D warnings` 通过
- `cargo fmt --check` 通过
- daemon 启动后，`vbw` 能成功连接并完成 HealthCheck
- 输入 prompt 后流式显示 TextDelta
- ToolCall / ToolResult 正确显示
- UserQuery 能交互式确认 (y/n)
- `/quit` 退出正常
