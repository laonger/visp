# visp 容器支持设计

## 1. 设计目标

visp 容器支持需要同时满足以下四个场景：

| 场景 | 核心诉求 | 实现手段 |
|---|---|---|
| 安全隔离 | Agent 执行的命令不破坏宿主机系统 | 命令在容器内执行，系统级隔离 |
| 环境可复现 | 不同项目使用不同运行时环境 | 项目级镜像配置，按需拉取 |
| 干净工作区 | 每 session 从干净状态开始，互不干扰 | 容器 per-session，结束后销毁；支持 ephemeral 模式 |
| CI/CD 集成 | 确定性执行环境 | 容器化 daemon + headless 模式 |

### 验收标准

1. 用户通过配置启用容器模式后，所有工具（Bash、File、Grep、Glob 等）的执行都在容器内完成
2. 每个 session 拥有独立的容器实例，session 结束后容器自动销毁
3. 项目目录通过 bind-mount 挂载到容器内，agent 的代码修改正常持久化到宿主机
4. 支持通过项目级配置指定容器镜像，不同项目使用不同运行时
5. 支持 ephemeral 模式：session 内的系统性变更（装包、改配置）不持久化
6. 默认行为（不启用容器）与当前完全一致，零破坏性

### 不做

- 不做容器镜像构建/管理工具（用户自行准备镜像）
- 不做 Kubernetes 编排
- 不做 Windows 容器支持（仅 Linux containers）
- 不改造 MCP 协议层（MCP 服务器位置通过配置控制）

---

## 2. 架构概览

### 2.1 整体架构

```
┌─────────────────── 宿主机 ───────────────────┐
│                                               │
│  ┌─────────┐     gRPC    ┌──────────────┐    │
│  │ visp-cli │───────────>│ visp-daemon  │    │
│  └─────────┘            │              │    │
│                         │  ┌──────────┐ │    │
│                         │  │Orchestr. │ │    │
│                         │  │  ┌────┐  │ │    │
│                         │  │  │Tools│  │ │    │
│                         │  │  └──┬─┘  │ │    │
│                         │  │     │    │ │    │
│                         │  │  ┌──▼───┐│ │    │
│                         │  │  │Executor││ │   │
│                         │  │  └──┬───┘│ │    │
│                         │  └─────┼────┘ │    │
│                         └────────┼──────┘    │
│                                  │            │
│                    ┌─────────────┼────────┐   │
│                    │  ContainerManager     │   │
│                    │  ┌─────────────────┐  │   │
│                    │  │ docker run/exec │  │   │
│                    │  └────────┬────────┘  │   │
│                    └───────────┼───────────┘   │
│                                │               │
│         ┌──────────────────────┼──────────┐    │
│         │     Docker Engine    │          │    │
│         │  ┌───────────────────▼───────┐  │    │
│         │  │  Container (per-session)  │  │    │
│         │  │  ┌─────────────────────┐  │  │    │
│         │  │  │ /workspace (mount)  │◄─┼──┼────┼── 项目目录
│         │  │  │ sh, rg, git, ...    │  │  │    │   (bind-mount)
│         │  │  └─────────────────────┘  │  │    │
│         │  └───────────────────────────┘  │    │
│         └─────────────────────────────────┘    │
└────────────────────────────────────────────────┘
```

### 2.2 核心设计决策

1. **Daemon 运行在宿主机**，不在容器内。Daemon 通过 Docker API 管理容器。
2. **容器 per-session**：每个 agent session 创建独立的容器实例。
3. **项目目录 bind-mount**：宿主机项目目录挂载到容器内 `/workspace`，agent 修改代码直接持久化。
4. **命令通过 `docker exec` 执行**：Bash/Grep/Glob 等命令在容器内运行。
5. **文件操作策略分两种模式**：
   - **persist 模式**（默认）：文件操作通过宿主机 `std::fs` 直接操作 bind-mount 目录（高性能），命令执行通过 `docker exec`。
   - **ephemeral 模式**：使用 Docker overlay，所有操作（含文件）都通过 `docker exec`，session 结束丢弃变更。

---

## 3. 模块划分

### 3.1 新增 crate: `visp-executor`

定义执行抽象层，包含：

- **`Executor` trait**：统一抽象命令执行和文件操作
- **`LocalExecutor`**：封装当前的直接 `Command::new("sh")` 和 `std::fs` 调用，作为默认实现
- **`ContainerExecutor`**：通过 Docker CLI（`docker exec`/`docker cp`）实现，所有操作转发到容器内

#### Executor trait 职责

| 方法 | 说明 | LocalExecutor 实现 | ContainerExecutor 实现 |
|---|---|---|---|
| `execute_command` | 执行 shell 命令 | `Command::new("sh")` | `docker exec <ctr> sh -c` |
| `read_file` | 读取文件内容 | `std::fs::read` | persist: `std::fs::read`(host path); ephemeral: `docker exec cat` |
| `write_file` | 写入文件 | `std::fs::write` | persist: `std::fs::write`(host path); ephemeral: `docker exec tee` |
| `edit_file` | 编辑文件（读-改-写） | `std::fs` 读写 | 同 read_file + write_file |
| `path_exists` | 检查路径是否存在 | `std::path::exists` | persist: host path exists; ephemeral: `docker exec test` |
| `list_dir` | 列出目录内容 | `std::fs::read_dir` | persist: `std::fs::read_dir`; ephemeral: `docker exec ls` |
| `canonicalize` | 解析符号链接 | `std::fs::canonicalize` | 对应路径映射 |

### 3.2 新增 crate: `visp-container`

容器生命周期管理，包含：

- **`ContainerManager`**：管理容器的创建、启动、销毁
- **`ContainerConfig`**：容器配置（镜像、挂载、环境变量、网络等）
- **`ContainerHandle`**：代表一个运行中的容器实例，持有容器 ID 和元数据

#### ContainerManager 职责

| 操作 | 说明 |
|---|---|
| `create_container` | 根据 config 创建并启动容器，返回 ContainerHandle |
| `destroy_container` | 停止并删除容器（`docker rm -f`） |
| `health_check` | 检查容器是否正常运行 |
| `cleanup_stale` | daemon 启动时清理遗留的 visp 容器 |

#### 容器生命周期与 Session 绑定

```
Session 创建 ──> ContainerManager.create_container() ──> 容器启动
                        │
                   session 运行中
                   (工具通过 ContainerExecutor 执行)
                        │
Session 结束 ──> ContainerManager.destroy_container() ──> 容器销毁
```

### 3.3 现有模块改动

#### visp-core

**ToolContext 扩展**：增加 `executor: Arc<dyn Executor>` 字段。所有工具通过 context.executor 执行命令和文件操作，不再直接调用 `std::fs` 或 `Command`。

**AgentLoopContext 扩展**：增加 executor 引用，在构造 ToolContext 时传递。

#### visp-tools

每个工具的 `execute()` 方法从直接使用 `std::fs`/`Command` 改为使用 `context.executor`：

| 工具 | 当前实现 | 改造后 |
|---|---|---|
| Bash | `Command::new("sh")` | `executor.execute_command()` |
| ReadFile | `std::fs::read` + `validate_path` | `executor.read_file()` + 路径校验 |
| WriteFile | `std::fs::write` + `validate_write_path` | `executor.write_file()` + 路径校验 |
| EditFile | `std::fs` 读写 | `executor.edit_file()` + 路径校验 |
| Grep | `Command::new("rg")` | `executor.execute_command("rg ...")` |
| Glob | `Command::new("rg --files")` | `executor.execute_command("rg --files ...")` |

**path.rs 改造**：`validate_path` 需要感知路径映射。在容器模式下，工具收到的路径是容器内路径（`/workspace/...`），但 persist 模式下文件操作走宿主机，需要映射回宿主机路径。这个映射由 Executor 内部处理，`validate_path` 仍然基于 `working_dir`（容器内路径）进行校验。

#### visp-daemon

**main.rs**：根据配置创建 `LocalExecutor` 或 `ContainerExecutor`，注入到 Orchestrator 和 SessionManager。

**config.rs**：新增 `ExecutorSection` 配置段。

#### visp-agent

**Orchestrator**：在创建/销毁 session 时，协调 ContainerManager 创建/销毁容器。构造 `AgentLoopContext` 时注入 executor。

---

## 4. 路径映射策略

容器模式下存在两套路径：

```
宿主机路径:  /Users/foo/myproject/src/main.rs
容器内路径:  /workspace/src/main.rs
```

### 映射规则

- **working_dir**：`ToolContext.working_dir` 始终使用**容器内路径**（`/workspace`），对工具透明
- **persist 模式**：ContainerExecutor 内部维护 `host_path ↔ container_path` 映射，文件操作时自动转换
- **ephemeral 模式**：所有操作都走 `docker exec`，无需路径映射

### validate_path 适配

`validate_path(target, working_dir)` 逻辑不变——仍以 `working_dir`（容器内路径）为基准校验。但 `canonicalize` 需要通过 executor 执行（因为 `std::fs::canonicalize` 操作的是宿主机路径）。

---

## 5. 配置设计

### daemon.toml 新增配置段

```toml
[executor]
# 执行后端: "local" (默认，当前行为) | "container"
backend = "local"

# 容器运行时: "docker" | "podman"（默认 docker）
# runtime = "docker"

# 基础镜像（全局默认，可被项目级配置覆盖）
image = "visp/workspace:latest"

# 工作区挂载点（容器内路径，默认 /workspace）
# workspace_mount = "/workspace"

# 会话模式: "persist" (默认，变更持久化) | "ephemeral" (变更不持久化)
# session_mode = "persist"

# 容器自动删除（默认 true）
# auto_remove = true

# 额外挂载（宿主机路径:容器路径 格式）
# extra_mounts = ["/tmp/shared:/shared"]

# 额外环境变量
# env = ["NODE_ENV=development", "DATABASE_URL=..."]

# 网络模式: "bridge" (默认) | "host" | "none"
# network = "bridge"

# MCP 服务器运行位置: "host" (默认) | "container"
# mcp_location = "host"
```

### 项目级覆盖

项目 `.visp/daemon.toml` 可以覆盖镜像和模式，实现按项目定制环境：

```toml
[executor]
image = "node:20"        # 此项目用 Node 20
session_mode = "persist"
```

### 配置优先级

与现有配置一致：CLI 参数 > 项目级 > 全局 > 默认值

---

## 6. 容器镜像要求

### 基础镜像约定

visp 不提供镜像构建工具，但约定基础镜像需包含：

| 依赖 | 用途 | 必需性 |
|---|---|---|
| `sh` | Bash 工具执行 | 必需 |
| `rg` (ripgrep) | Grep/Glob 工具 | 推荐（缺失时回退到 grep/find） |
| `git` | 版本控制操作 | 推荐 |
| `python3` / `node` / `go` ... | 项目运行时 | 按项目需求 |

### 镜像选择策略

1. 项目级配置指定 → 使用该镜像
2. 未配置 → 使用全局默认镜像
3. 全局未配置 → 报错提示用户配置镜像

未来可扩展自动推断（检测 `package.json` → 建议 Node 镜像），但不在当前设计范围内。

---

## 7. 数据流

### 7.1 Session 启动流程（容器模式）

```
CLI 发送 CreateSessionRequest(project_path="/Users/foo/myproject")
    │
    ▼
Daemon gRPC handler
    │  创建 session，存储 project_path
    ▼
用户发送第一条消息
    │
    ▼
Orchestrator.start_main_agent()
    │  调用 session_mgr.start_loop() 获取 AgentLoopContext
    ▼
ContainerManager.create_container(
    image = config.executor.image,
    mount  = { project_path: "/workspace" },
    env    = config.executor.env,
)
    │  返回 ContainerHandle { container_id, ... }
    ▼
创建 ContainerExecutor(handle, host_to_container_path_map)
    │
    ▼
将 executor 注入 AgentLoopContext
    │
    ▼
进入 agent loop，工具通过 context.executor 执行
```

### 7.2 工具执行流程（容器模式）

```
LLM 返回 tool_call: { "command": "cargo test", "workdir": "/workspace" }
    │
    ▼
Bash.execute(arguments, context)
    │  从 arguments 解析 command 和 workdir
    │  validate_path(workdir, context.working_dir)  // 容器内路径校验
    ▼
context.executor.execute_command("cargo test", "/workspace", timeout)
    │
    ▼  (ContainerExecutor)
docker exec <container_id> sh -c "cd /workspace && cargo test"
    │
    ▼
返回 stdout + stderr + exit_code
    │
    ▼
Bash 格式化输出，返回 ToolResult
```

### 7.3 Session 结束流程

```
Session 结束（用户退出 / cancel / daemon 关闭）
    │
    ▼
Orchestrator 调用 session_mgr.finish_loop()
    │
    ▼
ContainerManager.destroy_container(container_id)
    │  docker rm -f <container_id>
    ▼
资源清理完成
```

---

## 8. MCP 服务器策略

MCP 服务器通过 `tokio::process::Command` 作为子进程启动，通过 stdin/stdout 通信。

### 两种模式

| 模式 | 说明 | 适用场景 |
|---|---|---|
| host（默认） | MCP 服务器运行在宿主机，与 daemon 同级 | MCP 服务器需要访问宿主机资源 |
| container | MCP 服务器运行在容器内 | 完全隔离，需要镜像内包含 MCP 服务器 |

### host 模式下的网络问题

MCP 服务器在宿主机运行时，如果它需要访问容器内的服务（如容器内数据库），需要通过端口映射或 host 网络模式。这由用户通过 `extra_mounts` 和 `network` 配置自行解决。

### container 模式

MCP 服务器作为容器内的附加进程运行。这要求容器镜像内预装 MCP 服务器二进制。实现上，daemon 通过 `docker exec` 启动 MCP 服务器进程，并通过容器的 stdin/stdout 通信。这增加了复杂度，作为 Phase 3 功能。

---

## 9. CodeGraph 适配

CodeGraph（`visp-codegraph`）使用 tree-sitter 解析源码 + SQLite 存储索引。

### 容器模式下的处理

| 组件 | 处理方式 |
|---|---|
| 源码文件读取 | 通过 executor 读取（bind-mount 目录） |
| tree-sitter 解析 | 在 daemon 进程内执行（不依赖容器环境） |
| SQLite 索引存储 | 存储在宿主机（daemon 本地路径），不进容器 |

CodeGraph 只需要读取源码文件，不需要在容器内执行命令。因此通过 executor 的 `read_file` / `list_dir` 适配即可，tree-sitter 解析和 SQLite 存储保持在 daemon 侧。

---

## 10. 错误处理

### 容器相关错误

| 错误场景 | 处理策略 |
|---|---|
| Docker 未安装/未运行 | daemon 启动时检测，报错并提示用户 |
| 镜像不存在 | 尝试 `docker pull`；失败则报错 |
| 容器创建失败 | session 启动失败，返回错误给 CLI |
| 容器运行中异常退出 | 检测到后尝试重建；重建失败则终止 session |
| `docker exec` 超时 | 与当前 bash timeout 机制一致 |
| daemon 异常退出 | 下次启动时 `cleanup_stale()` 清理遗留容器 |

### 容器命名与标签

- 容器名：`visp-{session_id}`（截断 UUID 到 12 位，避免 Docker 名称长度限制）
- 标签：`visp.session={session_id}`, `visp.version={version}`
- 用于 `cleanup_stale()` 和 `docker ps` 过滤

---

## 11. 实施阶段

### Phase 1: Executor 抽象 + LocalExecutor（无行为变化）

**目标**：引入 Executor trait，将所有工具改为通过 executor 执行，但默认使用 LocalExecutor（行为与当前完全一致）。

**改动范围**：
- 新增 `visp-executor` crate（Executor trait + LocalExecutor）
- 改造 `ToolContext` 增加 executor 字段
- 改造所有工具（Bash、ReadFile、WriteFile、EditFile、Grep、Glob）使用 executor
- 改造 `AgentLoopContext` 和 agent_loop 传递 executor
- daemon main.rs 创建 LocalExecutor 并注入

**验收**：所有现有测试通过，行为零变化。

### Phase 2: ContainerManager + ContainerExecutor（核心功能）

**目标**：实现容器生命周期管理和容器内执行。

**改动范围**：
- 新增 `visp-container` crate（ContainerManager + ContainerConfig）
- 实现 ContainerExecutor（persist 模式）
- daemon config 增加 `[executor]` 段
- Orchestrator 在 session 启动/结束时创建/销毁容器
- daemon 启动时 `cleanup_stale()`
- 路径映射实现

**验收**：配置 `backend = "container"` 后，Bash 命令在容器内执行，文件操作正常，session 结束容器销毁。

### Phase 3: ephemeral 模式 + MCP 容器化 + 完善

**目标**：完整覆盖所有场景。

**改动范围**：
- ContainerExecutor ephemeral 模式（文件操作走 `docker exec`）
- MCP 服务器 container 模式
- CodeGraph 适配
- 容器健康检查和自动重建
- Dockerfile 和基础镜像文档

**验收**：四个场景全部可用，CI/CD 场景可 headless 运行。

---

## 12. 影响范围分析

### 新增文件

| 路径 | 说明 |
|---|---|
| `crates/visp-executor/` | Executor trait + LocalExecutor + ContainerExecutor |
| `crates/visp-container/` | ContainerManager + 容器生命周期管理 |
| `docs/design/visp-design-container.md` | 本设计文档 |
| `Dockerfile` | visp daemon 容器镜像（Phase 3） |

### 修改文件

| 路径 | 改动说明 |
|---|---|
| `crates/visp-core/src/tool.rs` | ToolContext 增加 executor 字段 |
| `crates/visp-core/src/agent.rs` | AgentLoopContext 增加 executor 字段 |
| `crates/visp-core/src/agent_loop.rs` | 构造 ToolContext 时注入 executor |
| `crates/visp-core/src/session.rs` | start_loop 返回值携带 executor |
| `crates/visp-tools/src/bash.rs` | 改用 executor.execute_command |
| `crates/visp-tools/src/file.rs` | ReadFile/WriteFile/EditFile 改用 executor |
| `crates/visp-tools/src/search.rs` | Grep/Glob 改用 executor.execute_command |
| `crates/visp-tools/src/path.rs` | validate_path 适配路径映射 |
| `crates/visp-daemon/src/main.rs` | 创建 executor 并注入 |
| `crates/visp-daemon/src/config.rs` | 新增 ExecutorSection |
| `crates/visp-agent/src/orchestrator.rs` | session 启动/结束时管理容器 |
| `docs/daemon.example.toml` | 新增 [executor] 配置段 |
| `Cargo.toml` (workspace) | 新增 visp-executor, visp-container 成员 |

### 不受影响

| 模块 | 原因 |
|---|---|
| visp-llm | LLM 调用不涉及本地执行 |
| visp-proto | gRPC 协议不变（project_path 已有） |
| visp-cli | CLI 逻辑不变（容器对用户透明） |
| visp-db | SQLite 存储不进容器 |
| visp-context | 上下文裁剪不涉及执行 |
| visp-command | 斜杠命令解析不涉及执行 |
