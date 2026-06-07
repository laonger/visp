# vibewisp init 命令 — 项目初始化设计

## 1. 目标

`vbw init` 一键完成项目初始化：创建目录结构、初始化 CodeGraph AST 索引、生成 AI 行为指南和项目规则。

## 2. 执行流程

```
用户执行: vbw init [--force] [--no-ai]

CLI 端:
  ├─ 解析参数
  ├─ 构造 InitProjectRequest (project_path, force, skip_ai)
  ├─ 调用 gRPC VbwClient::init_project(request)
  └─ 接收 InitProjectResponse，打印 created 列表到终端

Server 端 (Daemon):
  ├─ 1. 创建目录结构
  │     .vibewisp/rules/
  │     .vibewisp/skills/
  │     .vibewisp/plans/
  │
  ├─ 2. 写入样例文件
  │     .vibewisp/rules/always.md      ← 始终生效的默认规则
  │
  ├─ 3. CodeGraph 初始化
  │     CodeGraph::open(project_path)
  │       → 创建 .vibewisp/codegraph.db（WAL + FTS5）
  │       → 初始化 SQLite schema（symbols, edges, files 表）
  │       → 启动后台 build_full（增量索引项目源码）
  │
   ├─ 4. AI 规则生成（除非 --no-ai）
   │     ├─ 创建临时 session
   │     ├─ 构建 init 专用 prompt（分析项目 → AGENTS.md + rules）
   │     ├─ 启动 agent loop（使用默认 provider + tools）
   │     │     工具：read_file, glob, codegraph_search, write_file
   │     ├─ Agent 执行：
   │     │   ├─ 读取现有的 AGENTS.md（如果存在）
   │     │   ├─ 读取 README.md、Cargo.toml 等
   │     │   ├─ 分析项目结构
   │     │   ├─ **更新** 现有的 AGENTS.md（追加或修改 vibewisp 相关信息）
   │     │   │   └─ 不存在则新建
   │     │   ├─ 写入 .vibewisp/rules/project.md
   │     │   └─ 更新 .vibewisp/rules/always.md
   │     └─ Agent 完成后销毁临时 session
  │
  └─ 5. 返回 created 列表
```

## 3. gRPC 接口

### Proto 定义

在 `vibewisp.proto` 中新增 RPC：

```
rpc InitProject(InitProjectRequest) returns (InitProjectResponse)
```

### 请求消息

`InitProjectRequest`：
- `project_path` — 项目根目录绝对路径
- `force` — 是否覆盖已有文件（默认 false）
- `skip_ai` — 是否跳过 AI 规则生成（默认 false）

### 响应消息

`InitProjectResponse`：
- `created` — 已创建的文件路径列表（相对或绝对）

## 4. 模块职责

| 模块 | 职责 |
|------|------|
| `cli/main.rs` | 新增 `Init` 子命令（clap），解析 `--force` / `--no-ai` 参数 |
| `cli/client.rs` | 新增 `VbwClient::init_project()` 方法，调用 gRPC |
| `proto/vibewisp.proto` | 新增 `InitProject` RPC + 消息定义 |
| `daemon/init.rs` | **新增**，init 全逻辑：目录创建、样例文件、CodeGraph 初始化、AI 规则生成 |
| `daemon/service.rs` | 新增 `init_project` endpoint handler，委托给 `init.rs` |

## 5. 目录结构

init 创建的目录结构：

```
<project_root>/
└── .vibewisp/
    ├── codegraph.db         ← CodeGraph SQLite 索引
    ├── rules/
    │   ├── always.md        ← 始终生效（alwaysApply: true）
    │   └── project.md       ← 项目专属（AI 生成时创建）
    ├── skills/              ← 预留，未来 Skills 功能
    └── plans/               ← 预留，未来工作计划
```

## 6. 样例文件内容

### .vibewisp/rules/always.md（默认模板）

YAML frontmatter 声明 `alwaysApply: true`，内容包含项目关键约束：
- 构建/测试命令
- 编码规范
- 注意事项

### AGENTS.md（AI 生成或默认模板）

为 AI 编程助手提供项目上下文：
- 构建/测试/检查命令
- 项目架构简要说明
- 编码规范和注意事项
- Monorepo 边界说明（如适用）

## 7. AI 规则生成的 Prompt

当 `--no-ai` 未指定时，init 构建以下 prompt 发送给 agent：

系统提示应该引导 agent：
- 检查项目中是否已有 `AGENTS.md`，如存在先读取其内容
- 阅读 README.md、Cargo.toml、已有配置文件
- 浏览项目文件结构
- 查询 CodeGraph 符号了解 API
- **更新** 已有的 `AGENTS.md`：
  - 如已有 AGENTS.md，追加 vibewisp 相关信息或补充缺失内容
  - 如不存在，新建 AGENTS.md
- 写入 .vibewisp/rules/project.md（项目专属规则）
- 更新 .vibewisp/rules/always.md（始终生效的约束）
- 保持文件简洁、可执行

## 8. 边界情况处理

| 场景 | 处理 |
|------|------|
| `.vibewisp/` 已存在 | 如 `--force`，覆盖；否则跳过已存在文件，创建缺失的 |
| `codegraph.db` 已存在 | `CodeGraph::open()` 幂等，不会损坏已有数据 |
| CodeGraph build_full 失败 | 后台执行，不影响 init 返回；warning 级别日志 |
| daemon 未启动 | CLI 提示 "Daemon not available" 并退出 |
| AI 规则生成失败 | 不阻塞 init 其他步骤；error 日志 + 使用默认模板 |
| 项目路径不存在 | 返回错误，不创建目录 |
| 已有 AGENTS.md | AI 读取并追加/更新，不覆盖 |
| 已有 AGENTS.md 但 --no-ai | 不修改已有 AGENTS.md |

## 9. 不做什么

- ❌ 不覆盖已有的 AGENTS.md（除非 --force）
- ❌ 不修改已有的 AGENTS.md（当 `--no-ai` 时）
- ❌ 不自动安装依赖或运行构建
- ❌ 不创建 daemon.toml（那是全局配置，不是项目配置）
- ❌ 不修改 .gitignore（用户自行管理）

## 10. 验收标准

- `vbw init` 创建 `.vibewisp/` 及其子目录
- CodeGraph 数据库已初始化，后台索引已启动
- 样例 `always.md` 已写入
- `--no-ai` 时 AGENTS.md 不修改
- `--force` 时覆盖已有文件
- 已有 AGENTS.md 时，AI 读取后追加/更新，不从头覆盖
- AI 模式生成的 AGENTS.md 包含所在任务的说明
- 没有 AI 生成的文件时，打印创建的目录列表
- CodeGraph 索引完成后，`codegraph_search` 可用
