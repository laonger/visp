# /init 斜杠命令设计

## 1. 目标

`/init` 是 TUI 聊天中的斜杠命令，一键完成项目初始化：创建目录结构、初始化 CodeGraph、生成 AGENTS.md。

对标 opencode 的 `/init` 命令。

## 2. 执行流程

```
用户输入: /init [--force]

CLI (event.rs):
  └─ handle_command(text)
       └─ chat_handle.send_input(text)  ← 像普通消息一样发送

Daemon (service.rs, chat handler):
  ├─ 收到 UserInput text="/init [--force]"
  ├─ 识别为 init 命令
  ├─ 状态检查: session 必须是 Idle
  │
  ├─ 调用 command::init::prepare(project_path, text)
  │     │
  │     ├─ 解析 --force 参数（text.contains("--force")）
  │     │
  │     ├─ 创建目录
  │     │      .vibewisp/rules/
  │     │      .vibewisp/skills/
  │     │      .vibewisp/plans/
  │     │   → 发送 StatusUpdate: "Creating .vibewisp/..."
  │     │
  │     ├─ 初始化 CodeGraph
  │     │      CodeGraph::open(project_path)
  │     │      → 创建 .vibewisp/codegraph.db
  │     │      → 后台 tokio::spawn(build_full)
  │     │   → 发送 StatusUpdate: "Initializing CodeGraph..."
  │     │   → open 失败记 warning 日志，不中断流程
  │     │
  │     └─ 构造 prompt（硬编码模板 + project_path 注入，--force 时修改指令）
  │         → 返回 Message::user(prompt)
  │
  ├─ 追加 user_msg 到 session
  ├─ 启动 agent loop
  │      → AI 读取 README.md、Cargo.toml 等
  │      → AI 调用 glob 浏览项目结构
  │      → AI 调用 codegraph_search 了解符号
  │      → AI 读取已有 AGENTS.md（如存在且非 --force）
  │      → AI 调用 write_file(AGENTS.md)
  │
  └─ Agent 正常完成，Done 事件发送给 CLI
```

## 3. 关键接口

### command::init::prepare

```
入参:
  - project_path: PathBuf  (项目根目录)
  - text: &str             (原始消息文本，如 "/init --force")

返回值:
  - Message                (构造好的 user message，包含 init prompt)
```

副作用（函数内部执行）:
- 文件系统: 创建 .vibewisp/ 子目录
- CodeGraph: 打开数据库 + 后台索引
- gRPC 流: 发送 StatusUpdate 到 CLI

## 4. Prompt 模板

### 默认模式 (无 --force)

```
你是一个项目初始化助手。你的任务是分析当前项目，生成或更新 AGENTS.md 文件。

当前项目路径: ${project_path}

执行步骤:
1. 使用 read_file 读取以下文件（如果存在）:
   - README.md
   - Cargo.toml (Rust 项目) / package.json (JS 项目) / 其他构建配置
2. 使用 glob 浏览项目的顶层文件结构
3. 使用 codegraph_search 搜索项目中的关键符号（如 main 函数、核心类型定义）
4. 如果项目已有 AGENTS.md，使用 read_file 读取它，然后在现有内容基础上追加补充 vibewisp 相关信息
5. 如果项目没有 AGENTS.md，创建新的
6. 使用 write_file 写入 AGENTS.md

AGENTS.md 格式要求:
- 使用 Markdown 格式
- 使用 XML 标签组织内容（如 <Role>、<Workflow>、<CodingStyle>），参考 vibewisp 项目的 AGENTS.md 风格
- 内容应包括:
  * 项目概述: 一句话描述 + 技术栈
  * 构建/测试/检查命令
  * 项目架构简要说明
  * 编码规范和注意事项
  * 各阶段的工具引用（如 read_file、bash、codegraph_search）
- 保持简洁，只写 AI 代理在做任务时需要知道的信息
- 不要写用户不需要看的内容
```

### --force 模式

```
你是一个项目初始化助手。你的任务是分析当前项目，重写 AGENTS.md 文件。

当前项目路径: ${project_path}

执行步骤:
1. 使用 read_file 读取以下文件（如果存在）:
   - README.md
   - Cargo.toml (Rust 项目) / package.json (JS 项目) / 其他构建配置
2. 使用 glob 浏览项目的顶层文件结构
3. 使用 codegraph_search 搜索项目中的关键符号（如 main 函数、核心类型定义）
4. 忽略已有的 AGENTS.md 内容，从头重写一个完整的 AGENTS.md
5. 使用 write_file 写入 AGENTS.md

AGENTS.md 格式要求:
- 使用 Markdown 格式
- 使用 XML 标签组织内容（如 <Role>、<Workflow>、<CodingStyle>），参考 vibewisp 项目的 AGENTS.md 风格
- 内容应包括:
  * 项目概述: 一句话描述 + 技术栈
  * 构建/测试/检查命令
  * 项目架构简要说明
  * 编码规范和注意事项
  * 各阶段的工具引用（如 read_file、bash、codegraph_search）
- 保持简洁，只写 AI 代理在做任务时需要知道的信息
- 不要写用户不需要看的内容
```

### 区别

| 模式 | 第 4 步 |
|------|--------|
| 默认 | 读取已有 AGENTS.md，在现有内容基础上追加补充 |
| --force | 忽略已有 AGENTS.md，从头重写 |

## 5. 文件变动

| 文件 | 改动 |
|------|------|
| `cli/event.rs` | `handle_command` 新增 `/init` 分支 |
| `daemon/src/command/init.rs` | **新增**：目录创建、CodeGraph 初始化、prompt 构造 |
| `daemon/src/command/mod.rs` | **新增**：模块声明 |
| `daemon/src/service.rs` | chat handler 调用 `command::init::prepare()` |
| 其他 | 无 proto 改动，无新增 RPC |

## 6. 关键设计决策

| 决策 | 方案 |
|------|------|
| init 逻辑位置 | `daemon/src/command/init.rs`，chat handler 只调用入口函数 |
| 返回值 | `prepare()` 返回 `Message`，CodeGraph 结果通过 tracing 记录 |
| `--force` 参数 | daemon 端字符串解析（`text.contains("--force")`） |
| CodeGraph::open 失败 | 记 warning 日志，不阻止 agent loop |
| CLI 显示 | `/init` 作为普通 User 消息显示 |
| Prompt 模板 | 硬编码为 const 字符串在 `init.rs` 中 |
| CLI 进度反馈 | daemon 发送 StatusUpdate："Creating .vibewisp/..."、"Initializing CodeGraph..." |
| AGENTS.md 处理 | 默认：读取后追加补充；--force：重写 |

## 7. 产出物

```
<project_root>/
├── .vibewisp/
│   ├── codegraph.db        ← CodeGraph SQLite 索引
│   ├── rules/              ← 规则目录（空，用户自行添加）
│   ├── skills/             ← 预留
│   └── plans/              ← 预留
│
└── AGENTS.md               ← AI 生成的 AI 编程指南
```

## 8. 边界情况

| 场景 | 处理 |
|------|------|
| `.vibewisp/` 已存在 | 跳过目录创建，CodeGraph::open 幂等 |
| AGENTS.md 已存在，无 --force | AI 读取后追加补充 |
| AGENTS.md 已存在，--force | AI 忽略已有内容，重写 |
| daemon 未连接 | CLI 提示 "Daemon not available" |
| CodeGraph build_full 失败 | 后台执行，不影响 agent loop |
| AI 生成 AGENTS.md 失败 | 错误信息正常返回给用户 |
| session 非 Idle | 返回 "Session is busy" 错误 |
| 目录创建失败 | 返回错误，不继续后续步骤 |

## 9. 验收标准

- 用户输入 `/init` 后，daemon 创建 .vibewisp/ 目录
- CLI 显示 "Creating .vibewisp/..." 和 "Initializing CodeGraph..." 状态
- CodeGraph 数据库已初始化
- AI 分析项目并生成/更新 AGENTS.md
- AGENTS.md 包含项目关键信息
- 已有 AGENTS.md 时（无 --force）AI 读取后更新，不覆盖
- --force 时 AI 重写 AGENTS.md
- 操作结果在对话区可见
