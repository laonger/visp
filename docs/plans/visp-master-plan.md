# visp 总体工作计划

## 概述

本文档定义 visp 项目从零到 MVP 的总体执行路线。每个阶段（Phase）在进入前会有独立的阶段性设计文档、工作计划、以及需求反思讨论。

## 阶段总览

```
Phase 1           Phase 2           Phase 3           Phase 4           Phase 5
项目骨架            LLM + 工具         Agent + Daemon     CLI 前端           CodeGraph
  │                  │                   │                  │                  │
  │── visp-core       │── visp-llm         │── Rule Engine    │── visp-cli        │── 解析器
  │── visp-proto      │── visp-tools       │── Session Mgr    │── 流式输出       │── 索引
  │                  │                   │── Agent 循环     │── REPL           │── 查询
  │                  │                   │── visp-daemon ────│                  │── 持久化
  │                  │                   │                  │                  │── 文件监听
  ▼                  ▼                   ▼                  ▼                  ▼
cargo build      unit tests         首条 gRPC 连通      用户可交互         代码智能就绪
```

## 阶段依赖关系

- Phase 2 依赖 Phase 1（需要 visp-core 的 trait 定义）
- Phase 3 依赖 Phase 2（Agent 需要 LLM provider 和工具）
- Phase 4 依赖 Phase 3（CLI 需要 daemon 可用）
- Phase 5 可与 Phase 3-4 并行（CodeGraph 相对独立）

## 各阶段概要

### Phase 1: 项目骨架 + 核心抽象

**目标**：搭建 Cargo workspace，定义核心 trait 和 gRPC 协议。

**交付物**：
- `visp/` 目录结构，workspace 级别的 Cargo.toml
- `visp-core` crate：Tool trait、LlmProvider trait、Error 类型、Message 类型
- `visp-proto` crate：visp.proto 定义，tonic/prost 编译配置

**验收标准**：
- `cargo build --workspace` 通过
- proto 文件编译生成 Rust 代码无误

**依赖**：无

---

### Phase 2: LLM Provider + 内置工具

**目标**：实现 LLM 调用抽象和基础工具集。

**交付物**：
- `visp-llm` crate：OpenAI provider、Anthropic provider、SSE 流解析
- `visp-tools` crate：文件读写工具、bash 执行工具、grep/glob 搜索工具

**验收标准**：
- 各 provider 的单元测试通过（mock HTTP 响应）
- 各工具的单元测试通过
- `pyrefly check` 类型检查通过

**依赖**：Phase 1

---

### Phase 3: Agent 核心 + Daemon

**目标**：实现 Agent 编排循环和 gRPC daemon 服务。

**交付物**：
- `visp-core` 扩展：Rule Engine（规则文件加载/热重载）、Session Manager（会话生命周期）、Prompt Builder、Agent 编排循环
- `visp-daemon` crate：gRPC server 启动、配置加载、Service 实现（CreateSession/Chat/HealthCheck）

**验收标准**：
- daemon 启动成功，监听指定端口
- 集成测试：通过 gRPC 客户端调用 HealthCheck 返回正常
- 集成测试：通过 gRPC 客户端调用 Chat，Agent 完成一轮完整的 "用户输入 → LLM → 工具 → LLM → 响应" 循环
- `pytest` 等价测试（或 Rust 集成测试）通过
- `pyrefly check` 通过

**依赖**：Phase 2

---

### Phase 4: CLI 前端

**目标**：实现终端交互界面，用户可通过 `vbw` 命令使用 visp。

**交付物**：
- `visp-cli` crate：gRPC 客户端、流式输出显示、基础 REPL 模式

**验收标准**：
- `vbw` 命令启动，自动连接 daemon（如 daemon 未运行则报清晰提示）
- 输入 prompt 后流式显示 LLM 响应
- REPL 模式支持多轮对话
- `pyrefly check` 通过

**依赖**：Phase 3

---

### Phase 5: CodeGraph 代码智能

**目标**：实现基于 tree-sitter 的代码解析、索引和查询。

**交付物**：
- `visp-codegraph` crate：tree-sitter 解析器封装、符号提取、关系提取、符号图索引、查询引擎
- SQLite 持久化（符号表、边表、倒排索引表）
- 文件监听器（notify）+ 增量更新

**验收标准**：
- 对 TypeScript/JavaScript 项目完成全量索引
- 符号搜索返回正确结果
- 调用者/被调用者查询正确
- 文件修改后增量索引更新正确
- `pyrefly check` 通过

**依赖**：可独立开发，与 Phase 3/4 并行

---

## 阶段文档产出物

每个阶段在开始前需完成以下前置文档：

```
docs/
├── design/
│   ├── visp-design.md              # ✅ 总设计文档 (已完成)
│   ├── visp-design-phase1.md       # Phase 1 阶段设计
│   ├── visp-design-phase2.md       # Phase 2 阶段设计
│   ├── ...
│
└── plans/
    ├── visp-master-plan.md         # ✅ 总计划文档 (当前文件)
    ├── visp-plan-phase1.md         # Phase 1 阶段计划
    ├── visp-plan-phase2.md         # Phase 2 阶段计划
    └── ...
```

**阶段启动流程**：
1. 反思讨论（需求确认 + 技术风险 + 方案调整）
2. 阶段性设计文档（该阶段的模块职责、数据流、接口规范）
3. 阶段性计划文档（具体步骤 + 验收标准 + TDD 测试清单）
4. 用户审核通过
5. 执行

## TDD 执行规范

每个阶段内的所有实现严格遵循 TDD 循环：

- **红**：先编写测试用例，运行确认未通过
- **绿**：编写最小实现让测试通过
- **测试**：`cargo test` 全量通过
- **类型检查**：`pyrefly check` 通过（Rust 项目如有对应工具；否则用 `cargo clippy`）
- **重构**：优化代码结构，再次运行测试 + 类型检查
- **提交**：`git commit`（conventional commits 格式）

## 时间线（参考）

| 阶段 | 预估工作量 | 说明 |
|---|---|---|
| Phase 1 | 小 | 纯脚手架和类型定义 |
| Phase 2 | 中 | LLM provider 实现涉及外部 API 对接 |
| Phase 3 | 中 | 核心逻辑，最复杂的部分 |
| Phase 4 | 小-中 | 依赖 Phase 3 的 daemon 就绪 |
| Phase 5 | 大 | 解析器 + 索引 + 查询 + 持久化，工作量大 |

---

## 下一步

进入 Phase 1，开始反思讨论 → 阶段设计 → 阶段计划。
