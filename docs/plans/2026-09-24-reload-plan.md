# visp 工作计划：/reload、自动文件监听热重载与 TUI 自动重连

- 日期：2026-09-24
- 依据设计：`docs/design/2026-09-24-reload-design.md`（v3.1，已通过用户审核）
- 质量门：`cargo test`、`cargo clippy -- -D warnings`、`cargo fmt -- --check`（每个子步骤收尾执行对应 crate 级命令，步骤 8 做 workspace 全量）

## 概述

按设计 v3.1 实施三大变更：(1) 显式 `/reload` 命令全链路（proto RPC + daemon 共享 reload 核心 + 命令解析 + TUI 接入）；(2) 自动文件监听热重载（daemon 新增 watch 模块，驱动同一套 reload 核心，含变化守卫、通知、开关）；(3) TUI 重连状态机 + idle 心跳看门狗 + 多 tab 恢复清理。

顺序遵循设计 §9 建议：**reload 核心与三资产改造先行**（显式路径可独立验收）→ **watcher 模块次之**（纯增量）→ **TUI 重连与看门狗最后**（改动面最大）。共 8 个步骤、20 个子步骤（= 20 个 TDD 循环 = 20 个 commit）。

关键落位决策（实施前约定）：

- reload 核心与文件监听模块放在 **`visp-daemon` 的 lib target**（`src/lib.rs` 新增两个公开模块 `reload`、`watch`），而非 main.rs 的私有 bin 模块——理由：`tests/` 集成测试与假体注入需要跨 target 可见性（lib.rs 现仅导出 config/observability，此为既有模式的自然延伸）；main.rs 经 crate 名引用。
- reload 核心与 watch 模块各自遵循 `*_tests.rs` inline-path 模式（`reload_tests.rs`、`watch_tests.rs`，参照 `visp-codegraph/src/watcher.rs` 的既有写法）。
- TUI 连接状态机抽到新文件 **`visp-tui/src/connection.rs` + `connection_tests.rs`**，使 event.rs 的改动最小化并满足「纯函数 / 可注入时钟与连接抽象」的可测性要求。

---

## 步骤 1：协议与基础类型

### 1a：proto 新增 ReloadConfig RPC（visp-proto）

#### 🔴 红 — 测试
| # | 测试用例 | 简明描述 |
|---|---|---|
| 1 | ReloadConfig 消息存在 | 生成代码含请求（空）与响应（逐条目：类别/成功/消息/变更数）两个消息类型，字段可构造 |
| 2 | 服务方法存在 | 生成的 client 与 server trait 均暴露 reload_config 方法 |
| 3 | oneof 回归 | ClientMessage/ServerMessage 的 oneof 变体集合与改动前完全一致（防误动） |

测试位置：`visp-proto/src/lib.rs` 新增 inline 测试模块（该 crate 首个测试，体量为编译期断言级别的 smoke）。

#### 🟢 绿 — 实现
在 `proto/visp.proto` 的「Daemon 控制」段落（与 HealthCheck/Shutdown 同组）新增两个 message 与一个 rpc；`build.rs` 重新生成。不改动任何既有 oneof 与字段编号。

#### 🧪 测试 → 🔍 类型检查
`cargo test -p visp-proto` / `cargo clippy -p visp-proto -- -D warnings` / `cargo fmt -- --check`
（若生成码未刷新：`cargo clean -p visp-proto` 后重跑）

#### ♻️ 重构
无。

#### 📦 提交
`git commit -m "feat(proto): add ReloadConfig unary RPC with per-item results"`

### 1b：AgentDefinition 全字段等值（visp-core）

#### 🔴 红 — 测试
| # | 测试用例 | 简明描述 |
|---|---|---|
| 1 | 全等判定 | 两个逐字段相同的定义判等 |
| 2 | permission 差异检出 | 仅权限规则列表不同 → 不等（安全字段，验收 15 依据） |
| 3 | system_prompt 差异检出 | 仅系统提示词不同 → 不等 |
| 4 | 可选字段差异检出 | temperature/model/steps 的 None↔Some 变化 → 不等 |
| 5 | 列表顺序敏感 | permission 规则顺序交换 → 不等（明确语义，避免歧义） |

测试位置：`agent_definition.rs` 内 inline 测试模块（visp-core 现有模式）。

#### 🟢 绿 — 实现
为 `AgentDefinition` 补充 `PartialEq` 派生（组成类型 `AgentMode`/`PermissionRule`/`PermissionAction` 已实现，一次 derive 即可，见设计 §5.3）。

#### 🧪 测试 → 🔍 类型检查
`cargo test -p visp-core` / `cargo clippy -p visp-core -- -D warnings` / `cargo fmt -- --check`

#### ♻️ 重构
无。

#### 📦 提交
`git commit -m "feat(core): derive PartialEq for AgentDefinition (9-field guard basis)"`

---

## 步骤 2：资产重载能力

### 2a：RuleEngine 重载写回与守卫素材（visp-config）

#### 🔴 红 — 测试
| # | 测试用例 | 简明描述 |
|---|---|---|
| 1 | 重载后新内容生效 | tempdir 项目修改 AGENTS.md → 执行重载 → 读取接口返回含新内容的拼接串 |
| 2 | 多级祖先发现 | 项目与上级目录各有一个 AGENTS.md → 重扫按「近先远后」顺序拼接 |
| 3 | IO 失败保留旧状态 | rules 目录某文件被锁读失败（模拟）→ 重载放弃写回，旧 RuleSet 原样保留，返回失败信息 |
| 4 | 目录缺失宽容 | 项目无 .visp/rules → 重载成功、计数 0 |
| 5 | 守卫素材可用 | 重扫结果暴露「拼接内容整体串」供等值比较（与消费形态一致，设计 §5.1） |

测试位置：`rules.rs` inline 测试模块（现有 tempdir 测试模式，全局目录经既有测试隔离钩子注入）。

#### 🟢 绿 — 实现
构造时补存 project_path 为私有字段；新增重载能力：重走与构造完全相同的扫描（祖先 AGENTS.md → 全局 AGENTS.md → 项目 rules → 全局 rules），在新变量中组装完整 RuleSet，成功后取写锁整体替换，失败放弃写回并上报（设计 §5.1）。

#### 🧪 测试 → 🔍 类型检查
`cargo test -p visp-config` / `cargo clippy -p visp-config -- -D warnings` / `cargo fmt -- --check`

#### ♻️ 重构
如扫描逻辑与构造出现重复，抽为内部共享的「扫描组装」步骤（构造与重载共用），保持行为等价。

#### 📦 提交
`git commit -m "feat(config): RuleEngine reload with build-then-swap and old-state preservation"`

### 2b：AgentRegistry 持有方式改 ArcSwap + Orchestrator 消费点（visp-agent、visp-daemon、workspace 依赖）

#### 🔴 红 — 测试
| # | 测试用例 | 简明描述 |
|---|---|---|
| 1 | 替换后新查询生效 | 替换 registry 快照后，下一次 spawn 查表使用新定义（新增用例，按目标语义先写） |
| 2 | 同流程快照一致 | 同一主流程内多次查表共享同一次快照，不撕裂（针对设计 §5.3 的 395/415 双查表点） |
| 3 | 跨 await 无借用违规 | 快照 clone 出的值跨 await 使用（既有模式的回归确认） |
| 4 | orchestrator 既有全量回归 | orchestrator_tests.rs 现有全部用例不回归 |
| 5 | daemon 编译与启动装配回归 | main.rs 构建 ArcSwap 后既有启动测试/编译通过 |

测试位置：`orchestrator_tests.rs`（inline-path 模式）+ daemon 侧编译验证。

#### 🟢 绿 — 实现
workspace `Cargo.toml` 增加 arc-swap 依赖；`visp-agent` 与 `visp-daemon` 的 Cargo.toml 引用。Orchestrator 的注册表字段改为 ArcSwap 持有；6 个消费点（设计标注约 395/405/415/439/630/654，**实施时以 grep `agent_registry` 于 orchestrator.rs 重新定位为准**）改为单次流程入口 load_full 快照并复用；main.rs 构建处包装为 ArcSwap。

#### 🧪 测试 → 🔍 类型检查
`cargo test -p visp-agent -p visp-daemon` / `cargo clippy -p visp-agent -p visp-daemon -- -D warnings` / `cargo fmt -- --check`

#### ♻️ 重构
若消费点改造后出现重复的快照获取样板，收敛为每个流程入口单点获取（不新增抽象层）。

#### 📦 提交
`git commit -m "refactor(agent,daemon): hold AgentRegistry as ArcSwap, snapshot per flow entry"`

---

## 步骤 3：daemon 共享 reload 核心（visp-daemon lib 新模块）

### 3a：reload 核心骨架与显式入口

#### 🔴 红 — 测试
| # | 测试用例 | 简明描述 |
|---|---|---|
| 1 | rules 条目成功 | tempdir 项目重载 → rules 条目成功且统计正确 |
| 2 | skills 条目成功 | skills 目录含一个 skill → skills 条目成功，ToolRegistry 中 skill 工具 description 反映新列表（经重建实例 + 同名替换，设计 §5.2） |
| 3 | agents 条目成功 | agents 目录含合法定义 → agents 条目成功，registry 已替换 |
| 4 | system_prompt 回执 | 显式入口恒返回 system_prompt 成功回执（L1 确认，设计 §5.4） |
| 5 | 部分失败隔离 | 注入一个非法 agent 文件 → agents 条目报跳过，其余条目正常成功 |
| 6 | 触发互斥 | 并发两次调用重载 → 串行执行、均返回完整结果（互斥量存在性验证） |

测试位置：`visp-daemon/src/reload_tests.rs`（inline-path 模式），tempdir + 真实 RuleEngine/ToolRegistry/ArcSwap。

#### 🟢 绿 — 实现
lib.rs 新增 `reload` 模块：持有三个重载目标引用（RuleEngine、ToolRegistry、ArcSwap）+ 一把异步互斥量 + 输入依赖（builtin_overrides clone、global_tx clone，设计 §5.5）；提供显式入口（四类全跑 + system_prompt 回执）与自动入口雏形（按领域子集执行，本步骤先打通 rules/skills/agents 三条路径与逐项结果类型）；agents 重载含 **agent_dirs 每次重算**（存在性检查不缓存）与 **builtin_overrides 静态持有**。

#### 🧪 测试 → 🔍 类型检查
`cargo test -p visp-daemon --lib` / `cargo clippy -p visp-daemon -- -D warnings` / `cargo fmt -- --check`

#### ♻️ 重构
逐项结果类型若与 proto 响应字段重复，仅保留核心内部一种并做单向映射（handler 薄封装在步骤 4a）。

#### 📦 提交
`git commit -m "feat(daemon): shared reload core with explicit entry and per-item results"`

### 3b：三领域变化守卫

#### 🔴 红 — 测试
| # | 测试用例 | 简明描述 |
|---|---|---|
| 1 | rules 无变化 | 二次重载内容相同 → 标注无变更、写锁替换不发生 |
| 2 | skills 无变化 | 二次重载 listing 相同 → 标注无变更 |
| 3 | agents 无变化 | 二次重载定义集合相同 → 标注无变更 |
| 4 | agents permission 单字段变更 | 仅改权限规则 → 判定有变化（安全漏报防线，验收 15） |
| 5 | agents system_prompt 单字段变更 | 仅改系统提示词 → 判定有变化 |
| 6 | 无变化不产生替换副作用 | 以可观察状态断言（如快照指针未变/工具对账零动作），验证守卫位于替换之前（设计 §7 决策 13） |

#### 🟢 绿 — 实现
在核心内部、替换动作之前实施三领域等值比较：rules 比拼接内容整体串、skills 比 listing 字符串、agents 比 name → 全 9 字段定义集合（复用 1b 的 PartialEq）；无变化领域跳过替换与对账。

#### 🧪 测试 → 🔍 类型检查
`cargo test -p visp-daemon --lib` / `cargo clippy -p visp-daemon -- -D warnings` / `cargo fmt -- --check`

#### ♻️ 重构
三领域守卫若出现重复比较样板，收敛为每领域一个守卫步骤的统一编排（不引入泛型抽象）。

#### 📦 提交
`git commit -m "feat(daemon): per-domain no-change guards before swap (rules/skills/agents)"`

### 3c：agent 工具对账

#### 🔴 红 — 测试
| # | 测试用例 | 简明描述 |
|---|---|---|
| 1 | 删除 agent → 工具移除 | 仅存在于旧集合的工具被移除 |
| 2 | 新增 agent → 工具注册 | 仅存在于新集合的注册为工具（经 global_tx 通道） |
| 3 | 重名失败隔离 | 与核心工具大小写不敏感撞名 → 该条失败计入结果，其余对账继续 |
| 4 | description 变更 → 同名替换 | 两边都有但描述变化 → 替换 |
| 5 | mode 切换 → 移除 | Subagent/All 改 Primary → 表现为旧有新无 → 移除 |
| 6 | 对账后集合严格一致 | ToolRegistry 中 agent 工具集合与 registry 的 subagent 集合完全一致（验收 3） |

#### 🟢 绿 — 实现
实现五分支对账算法（设计 §5.3），新工具注册复用启动时的注册路径与 global_tx 事件通道。

#### 🧪 测试 → 🔍 类型检查
`cargo test -p visp-daemon --lib` / `cargo clippy -p visp-daemon -- -D warnings` / `cargo fmt -- --check`

#### ♻️ 重构
无。

#### 📦 提交
`git commit -m "feat(daemon): agent tool reconciliation after registry swap"`

---

## 步骤 4：service 接入与命令解析

### 4a：ReloadConfig gRPC handler 薄封装（visp-daemon service.rs）

#### 🔴 红 — 测试
| # | 测试用例 | 简明描述 |
|---|---|---|
| 1 | 响应组装 | handler 调核心显式入口 → 逐项结果完整映射为 ReloadConfigResponse |
| 2 | 部分成功非 gRPC 错误 | 含失败条目时仍为正常响应（不走 gRPC status error，设计 §3） |
| 3 | system_prompt 回执透传 | 响应含恒成功的第四条目 |
| 4 | rule_engine 字段回归 | `CoderDaemonService.rule_engine` 真正被使用，`#[allow(dead_code)]` 移除后 clippy 通过 |

测试位置：`service.rs` inline 测试模块（该文件现有大量测试的既有模式）。

#### 🟢 绿 — 实现
service 新增 reload_config 处理：调用 reload 核心显式入口、组装响应；启用 rule_engine 字段。

#### 🧪 测试 → 🔍 类型检查
`cargo test -p visp-daemon` / `cargo clippy -p visp-daemon -- -D warnings` / `cargo fmt -- --check`

#### ♻️ 重构
无。

#### 📦 提交
`git commit -m "feat(daemon): wire ReloadConfig RPC to shared reload core"`

### 4b：visp-command 新增 Reload 变体（可并行，独立 crate）

#### 🔴 红 — 测试
| # | 测试用例 | 简明描述 |
|---|---|---|
| 1 | /reload 精确匹配 | parse 返回 Reload 变体 |
| 2 | /reloadxxx 不误配 | 回归既有 /init 与 /init-agent 的边界写法（前缀不误吞） |
| 3 | resolve 语义 | Reload 解析为「非 daemon 文本命令」动作（TUI unary 路径，不经 UserInput 拦截） |
| 4 | 既有命令回归 | 现有全部命令解析用例不回归 |

测试位置：`visp-command/src/lib.rs` inline 测试模块（现有模式）。

#### 🟢 绿 — 实现
Command enum 新增 Reload 变体（无参数）；parse 增加精确分支；resolve 对应映射。

#### 🧪 测试 → 🔍 类型检查
`cargo test -p visp-command` / `cargo clippy -p visp-command -- -D warnings` / `cargo fmt -- --check`

#### ♻️ 重构
无。

#### 📦 提交
`git commit -m "feat(command): add /reload slash command variant"`

---

## 步骤 5：TUI 显式 /reload 接入

> ⚠️ 本步骤修改 `event.rs`——与步骤 7（同文件）**必须串行**，不得与 Wave 7 并行。

### 5a：client 方法 + pending 接入 + 三处清单

#### 🔴 红 — 测试
| # | 测试用例 | 简明描述 |
|---|---|---|
| 1 | /reload 触发 pending | command::handle 识别 → 置 pending_reload 标志 + 状态行提示（沿用 /new 的 pending 模式） |
| 2 | 逐项结果渲染 | 响应到达 → 当前激活 tab 渲染：成功条目 Status 样式、失败条目 Error 样式、标志清除 |
| 3 | Tab 补全清单 | 补全列表含 /reload |
| 4 | 输入提示与帮助文案 | ui.rs 输入提示列表与帮助弹窗均含 /reload，文案注明「AGENTS.md/rules、skills、agents 另有保存自动生效；daemon.toml 需重启 daemon」 |
| 5 | 断线期调用报错不重试 | unary 失败 → 状态行错误提示、无自动重试（用户显式动作语义，设计 §5.9） |

测试位置：`event_tests.rs`、`ui_tests.rs`（既有假体模式）；client.rs 新方法若现有测试模式不含 mock gRPC server，则以编译级验证 + 步骤 8 端到端覆盖（在备注中说明）。

#### 🟢 绿 — 实现
client.rs 增加调用 ReloadConfig 的方法；command.rs 分支；event.rs 主循环 pending 处理块；event.rs Tab 补全、ui.rs 提示与帮助弹窗三处硬编码清单同步（设计 §5.8）。

#### 🧪 测试 → 🔍 类型检查
`cargo test -p visp-tui` / `cargo clippy -p visp-tui -- -D warnings` / `cargo fmt -- --check`

#### ♻️ 重构
无（三处清单是已知技术债的定点补录，不做额外抽象）。

#### 📦 提交
`git commit -m "feat(tui): /reload command wiring with per-item result rendering"`

---

## 步骤 6：文件监听模块（visp-daemon lib 新模块）

### 6a：监听计划构建（纯函数）

#### 🔴 红 — 测试
| # | 测试用例 | 简明描述 |
|---|---|---|
| 1 | 九类路径全集映射 | 设计 §5.6 表格的 9 行：每类监听对象 → 正确领域与过滤规则 |
| 2 | 祖先链构建 | 多级项目路径 → 祖先目录逐层非递归监听 + project 根层含 `.visp` 前缀规则 |
| 3 | 全新项目降级 | `.visp` 不存在 → 降级监听 project 根 + `.visp` 子树前缀过滤（P1-2 修复链） |
| 4 | 子目录缺失降级 | rules/agents/skills 目录不存在 → 降级监听最近存在祖先 + 前缀过滤 |
| 5 | 排除清单 | system-prompt.md、daemon.toml、webfetch.toml、logs、codegraph.db 不产生任何领域命中 |
| 6 | 全局根固定命中 | 全局配置根监听的顶层 AGENTS.md 与三个子目录创建/删除事件的分流正确 |

测试位置：`visp-daemon/src/watch_tests.rs`（inline-path 模式），tempdir 构造各存在性组合。

#### 🟢 绿 — 实现
`watch` 模块新增「监听计划」构建：输入 project root 与全局配置根解析结果，输出路径全集、监听模式（递归/非递归）、前缀过滤规则与领域映射（设计 §5.6 表格）。启动时一次性构建，路径全集固定。

#### 🧪 测试 → 🔍 类型检查
`cargo test -p visp-daemon --lib` / `cargo clippy -p visp-daemon -- -D warnings` / `cargo fmt -- --check`

#### ♻️ 重构
无。

#### 📦 提交
`git commit -m "feat(daemon): watch plan construction with missing-dir fallback chain"`

### 6b：watcher 运行时（notify feed → debounce → 领域重载）

#### 🔴 红 — 测试
| # | 测试用例 | 简明描述 |
|---|---|---|
| 1 | 写文件触发领域重载 | tempdir 中写 AGENTS.md → debounce 后 → 领域重载执行器收到 rules 领域调用（验收 22 主用例） |
| 2 | debounce 合并 | 200ms 窗口内多次写入 → 仅一次领域重载、只见最终状态 |
| 3 | 原子写收敛 | 写临时文件后 rename 覆盖 → 单次有效重载 |
| 4 | 跨领域聚合 | 同窗口修改 rules 与 agents 文件 → 一次调用、领域子集含两者 |
| 5 | 无关事件过滤 | `.visp` 下写入排除清单文件 → 执行器零调用 |
| 6 | 目录补挂后立即重扫 | 运行中创建缺失目录并立即写入 → 补挂完成后重扫拾取窗口内写入（kqueue 竞态缓解一，设计 §5.6） |
| 7 | 重复挂载去重 | 同一路径重复创建/删除风暴 → 已监听路径集合去重，执行器不被重复挂载干扰（缓解二） |
| 8 | 停止清理 | 停止后任务退出、不再触发重载 |

测试位置：**`visp-daemon/tests/filewatch_integration.rs`**（集成测试，依赖 lib 导出；注入「领域重载执行器」假体记录调用——真实实现委托 reload 核心自动入口，假体供测试，这是本步骤的抽象前提）。断言以「最终收敛」为准（轮询等待），不对事件序列做脆弱断言（macOS kqueue 后端时序差异，见备注）。

#### 🟢 绿 — 实现
watch 模块运行时：notify watcher 创建（多路径挂载）、回调线程过滤、后台任务 debounce（200ms 按领域聚合）、缺目录补挂（+ 补挂后立即领域重扫）、已监听路径集合去重、调用 reload 核心自动入口；stop 语义（abort 任务 + drop watcher）。「执行器」以 trait 抽象注入。

#### 🧪 测试 → 🔍 类型检查
`cargo test -p visp-daemon` / `cargo clippy -p visp-daemon -- -D warnings` / `cargo fmt -- --check`

#### ♻️ 重构
如补挂与去重逻辑与计划构建重叠，收敛计划模块为唯一事实来源。

#### 📦 提交
`git commit -m "feat(daemon): file watcher runtime with debounce, remount-rescan and dedup"`

### 6c：自动重载通知推送

#### 🔴 红 — 测试
| # | 测试用例 | 简明描述 |
|---|---|---|
| 1 | 有变更推送单条汇总 | reload 结果含变更领域 → 下行通道收到**至多一条**汇总 StatusUpdate，session_id 为空 |
| 2 | 无变化静默 | 全部领域无变更 → 零消息 |
| 3 | 失败提示 | 领域整体失败 → 一条错误性质 Status 消息 |
| 4 | 通道不可用不 panic | 下行通道已关闭 → 发送失败被静默容忍，reload 结果不受影响（best-effort，设计 §7 决策 11） |

#### 🟢 绿 — 实现
通知发送封装：按 reload 汇总结果构造 session_id 为空的 StatusUpdate 帧，经 orchestrator_grpc_tx 下行通道 best-effort 发送；挂在自动入口完成之后。

#### 🧪 测试 → 🔍 类型检查
`cargo test -p visp-daemon --lib` / `cargo clippy -p visp-daemon -- -D warnings` / `cargo fmt -- --check`

#### ♻️ 重构
无。

#### 📦 提交
`git commit -m "feat(daemon): best-effort hot-reload notification via chat downlink"`

### 6d：开关与 daemon 生命周期装配（可与 6b/6c 并行，依赖 6a）

#### 🔴 红 — 测试
| # | 测试用例 | 简明描述 |
|---|---|---|
| 1 | 默认开启 | 配置缺省 → 开关为 true（serde 默认值） |
| 2 | 显式关闭解析 | 配置置 false → 解析为关闭 |
| 3 | 关闭不创建 watcher | 工厂在开关关闭时返回空，不创建 notify 实例与后台任务 |
| 4 | 装配顺序回归 | daemon 启动序列在 service 组装后、gRPC server 前挂载；Ctrl+C 关闭序列先停 watcher 再 MCP shutdown（设计 §5.6 生命周期） |

测试位置：`config.rs` inline 测试（1-2）；`watch_tests.rs`（3）；4 以编译 + main.rs 启动测试/手工冒烟覆盖（bin 内装配不可 lib 测试，如实说明）。

#### 🟢 绿 — 实现
DaemonSection 增加布尔字段（默认 true）；watch 工厂函数（输入计划、执行器、通知句柄）；main.rs 在步骤 9/10 之间创建、Ctrl+C 分支最先停止（main.rs 约 615-624 行区域，实施时以 grep 定位）。回滚要求：该开关即自动路径的唯一回滚入口（设计 §9）。

#### 🧪 测试 → 🔍 类型检查
`cargo test -p visp-daemon` / `cargo clippy -p visp-daemon -- -D warnings` / `cargo fmt -- --check`

#### ♻️ 重构
无。

#### 📦 提交
`git commit -m "feat(daemon): filewatcher toggle and lifecycle wiring in daemon startup/shutdown"`

---

## 步骤 7：TUI 重连状态机、idle 看门狗与多 tab 恢复

> ⚠️ 全部子步骤涉及 `event.rs` 及其依赖，Wave 内部串行为主；与步骤 5 串行。

### 7a：连接状态机抽象（visp-tui 新模块 connection.rs）

**接口抽象与测试策略（特别要求，只述职责）**：

- **连接抽象（trait）**：职责五项——建立新连接（返回可收发的新连接实例）、下行收帧（异步等待下一 ServerMessage 或流结束）、上行发送（join/config/cancel 等既有消息）、健康探测（独立 unary，不经 Chat 流，带超时）、主动丢弃旧连接。`client.rs` 的 VispClient/ChatHandle 作为真实实现挂接；测试用**假体连接**（脚本化帧序列 + 故障注入：流关闭/无响应/延迟），沿用 `ChatHandle::new_mock` 的既有假体先例。
- **状态机纯函数**：输入（当前状态 + 事件 + 注入时钟读数），输出（新状态 + 动作指令清单）。事件集合：流干净关闭（recv=None）、idle 超时、探测成功、探测失败/超时、重连成功、用户退出。迁移规则严格按设计 §5.9/§7 决策 14：**失败即重连、成功即重置，无任何累计宽限**。
- **退避计算纯函数**：尝试次数 → 延迟（1s 起步倍增、30s 封顶、无限重试）。
- **idle 判定纯函数**：最后收帧时刻 + 当前时刻 + 阈值 → 是否超时（时钟可注入）。
- **常量集中定义**：idle 45s、探测超时 5s、退避序列端点，全部集中在本模块（回滚策略要求，设计 §9）。
- event loop 保留既有 select! 骨架，仅经适配层对接抽象；本步骤不接线（接线在 7b/7c）。

#### 🔴 红 — 测试
| # | 测试用例 | 简明描述 |
|---|---|---|
| 1 | recv=None → Reconnecting | 流干净关闭立即迁移并首次重试 |
| 2 | idle 超时 + 探测失败 → Reconnecting | 看门狗检出（SIGSTOP/半开路径） |
| 3 | idle 超时 + 探测成功 → 重置 | 不迁移、计时重置，进入下一周期（周期性探测稳态） |
| 4 | 退避序列 | 1s→2s→4s→…→30s 封顶；重连成功后归零 |
| 5 | exit 通道保留 | Reconnecting 状态下用户退出事件无条件生效 |
| 6 | 重连成功 → Connected | 探测成功 + 新流建立 + 恢复动作完成 → 迁回 |
| 7 | 无第三路径 | 不存在「连续 N 次强制重建」迁移（防止回潮的负向断言：连续多次探测成功不触发任何重建） |

测试位置：`connection_tests.rs`（inline-path 模式，注入假体连接与时钟）。

#### 🟢 绿 — 实现
新建 connection.rs：状态枚举、迁移纯函数、退避与 idle 判定纯函数、常量、连接抽象 trait 定义；client.rs 为真实实现挂接适配。

#### 🧪 测试 → 🔍 类型检查
`cargo test -p visp-tui` / `cargo clippy -p visp-tui -- -D warnings` / `cargo fmt -- --check`

#### ♻️ 重构
无（本步骤本身即抽离型重构，但以新模块 + 新测试呈现）。

#### 📦 提交
`git commit -m "feat(tui): injectable connection state machine with idle watchdog semantics"`

### 7b：event loop 接入重连恢复流程

#### 🔴 红 — 测试
| # | 测试用例 | 简明描述 |
|---|---|---|
| 1 | recv=None 不再退出 | 现状 `event.rs:161` 的 quit 行为被状态机取代（回归性质的方向翻转验证） |
| 2 | 恢复后回放渲染 | 重连成功 → send_join 回放帧渲染进主 tab |
| 3 | 模型列表刷新 | get_session 刷新 available_models/model_keys（重连后 daemon.toml 可能已变，设计 §5.9） |
| 4 | streaming 状态清理 | 重建流后本地 streaming 残留清零 |
| 5 | 重连期间输入保护 | Enter 提交在 Reconnecting 状态 → 提示不发送不清空；重连成功后可正常发送 |
| 6 | 生成中断提示 | 断线时正在 generating → 显示「连接中断，生成已终止」 |

测试位置：`event_tests.rs`（假体连接 + 脚本化故障注入）。

#### 🟢 绿 — 实现
event::run 改为持有 client 所有权（替换 `event.rs:85` 起的借用方式）；select! 的 recv 分支接入状态机；恢复流程五步（新连接 → 健康探测 → send_join 回放 → get_session 刷新 → 本地清理）经抽象执行。

#### 🧪 测试 → 🔍 类型检查
`cargo test -p visp-tui` / `cargo clippy -p visp-tui -- -D warnings` / `cargo fmt -- --check`

#### ♻️ 重构
若 event.rs 内联逻辑过长，将恢复流程收敛到 connection.rs 的动作执行侧（event loop 只保留分发）。

#### 📦 提交
`git commit -m "feat(tui): reconnect state machine wired into event loop with session restore"`

### 7c：idle 看门狗接线（依赖 7a + 7b）

#### 🔴 红 — 测试
| # | 测试用例 | 简明描述 |
|---|---|---|
| 1 | idle 到期触发探测 | 注入时钟推进 45s → 发起健康探测 |
| 2 | 探测 5s 超时按失败处理 | 探测无响应 → 进入 Reconnecting |
| 3 | 探测成功重置并周期循环 | 成功 → 计时重置 → 再 45s → 再探测（稳态验证，验收 18b） |
| 4 | 帧到达重置计时 | 任意流帧（TextDelta/ToolCall 等）到达 → 计时清零，看门狗不触发 |
| 5 | 常量集中核对 | 45s/5s 仅在 connection 模块定义（回滚要求的落实验证） |

测试位置：`connection_tests.rs` + `event_tests.rs`（注入时钟的假体循环）。

#### 🟢 绿 — 实现
select! 新增 idle 计时分支（与 spinner_tick 并列）：任何流帧重置计时；到期触发独立 unary 探测；结果交状态机判定。

#### 🧪 测试 → 🔍 类型检查
`cargo test -p visp-tui` / `cargo clippy -p visp-tui -- -D warnings` / `cargo fmt -- --check`

#### ♻️ 重构
无。

#### 📦 提交
`git commit -m "feat(tui): idle watchdog branch (45s idle + health probe, fail-reconnect/reset-continue)"`

### 7d：多 tab 恢复清理（依赖 7a 的状态定义，可与 7b/7c 并行，改 app.rs）

#### 🔴 红 — 测试
| # | 测试用例 | 简明描述 |
|---|---|---|
| 1 | subagent tab 丢弃不入回收站 | 重连成功 → 所有 subagent tab 移除，且不写入 closed_tabs（区别于既有 close_tab 回收语义，设计 §5.9） |
| 2 | per-tab 字段全重置 | 遍历断言 9 个字段（frames/rendered_up_to/streaming_text/stream_started_at/stream_output_tokens/pending_usage/last_stream_tps/last_stream_elapsed/generating，清单见 `app.rs:335-352`）全部复位 |
| 3 | 主 tab 保留 | 仅主 session tab 存活且历史完整 |
| 4 | 恢复提示文案 | 对话区出现「子 agent 标签页已关闭（其历史不可恢复）」 |

测试位置：`app_tests.rs`（清理逻辑抽为 app 层可测纯函数）。

#### 🟢 绿 — 实现
app 层新增「重连恢复清理」入口：复刻关闭逻辑但跳过回收站写入 + per-tab 状态遍历重置；由 7b 的恢复流程调用。

#### 🧪 测试 → 🔍 类型检查
`cargo test -p visp-tui` / `cargo clippy -p visp-tui -- -D warnings` / `cargo fmt -- --check`

#### ♻️ 重构
无。

#### 📦 提交
`git commit -m "feat(tui): multi-tab cleanup on reconnect (drop subagent tabs, reset per-tab state)"`

---

## 步骤 8：端到端回归与回滚验证

### 8a：全量质量门、验收清单核对与回滚演练

#### 🔴 红 — 测试
| # | 测试用例 | 简明描述 |
|---|---|---|
| 1 | workspace 全量回归 | `cargo test`（workspace）全绿，既有测试零回归 |
| 2 | 静态检查全绿 | `cargo clippy -- -D warnings`、`cargo fmt -- --check`（workspace 级） |
| 3 | 设计验收映射核对 | 对照设计 §10 的 23 条验收逐条标注「已被哪个子步骤的测试覆盖 / 需手工验收」，输出核对清单进提交说明 |

#### 🟢 绿 — 实现
修复回归暴露的问题（若有，问题修复各自小提交）；无实现性新增。

#### 🧪 测试 → 🔍 类型检查
`cargo test` / `cargo clippy -- -D warnings` / `cargo fmt -- --check`（workspace）

#### ♻️ 重构
无（超出本计划范围的重构建议单独提案）。

#### 📦 提交
`git commit -m "chore: full regression pass and acceptance checklist mapping"`

**手工验收清单（无法自动化的部分，交用户执行）**：daemon 重启 TUI 重连回放（验收 16）、SIGSTOP 看门狗检出（验收 18a）、真实编辑器保存的热重载体感（验收 7-9）、开关关闭后重启 daemon 的回滚路径（验收 12 + §9）。

---

## Wave 并行策略

### Wave 1：协议与基础类型（2 个并行任务）
任务 A: 1a
任务 B: 1b

### Wave 2：资产重载能力（2 个并行任务，不同 crate）
任务 A: 2a（visp-config）
任务 B: 2b（workspace 依赖 + visp-agent + visp-daemon）

### Wave 3：reload 核心（1 个串行链，同模块强依赖）
任务 A: 3a → 3b → 3c

### Wave 4：service 接入与命令解析（2 个并行任务）
任务 A: 4a（依赖 3c）
任务 B: 4b（独立）

### Wave 5：TUI 显式 /reload（1 个任务，event.rs 热点，独占）
任务 A: 5a（依赖 4a + 4b）

### Wave 6：文件监听模块（1 条主链 + 1 个并行分支）
任务 A: 6a → 6b → 6c（依赖 3c 的自动入口）
任务 B: 6d（依赖 6a；与 6b/6c 并行——不同文件：6d 动 config.rs/main.rs，6b/6c 动 watch 模块）

### Wave 7：TUI 重连与看门狗（1 条主链 + 1 个并行分支，event.rs 热点）
任务 A: 7a → 7b → 7c（串行：7b/7c 均改 event.rs）
任务 B: 7d（依赖 7a 的状态定义；与 7b/7c 并行——改 app.rs）
**约束：Wave 7 必须在 Wave 5 完成后开始（event.rs 串行）。**

### Wave 8：收尾（1 个任务，依赖全部）
任务 A: 8a

---

## 依赖关系总览

```
Wave1:  1a proto ─────────────────────────────────┐
        1b PartialEq ──┐                          │
Wave2:  2a RuleEngine ─┤                          │
        2b ArcSwap ─────┴─► Wave3: 3a ─► 3b ─► 3c ─┼─► Wave4: 4a ─┐
                                                    │         4b ─┤
                                                    │             ▼
                                                    │       Wave5: 5a (event.rs ①)
                                                    │
                                                    └─► Wave6: 6a ─┬─► 6b ─► 6c
                                                                   └─► 6d
Wave7 (在 Wave5 后):  7a ─┬─► 7b ─► 7c (event.rs ②)
                          └─► 7d
Wave8 (全部完成后):   8a
```

- 硬依赖：3a 依赖 2a+2b+1b；4a 依赖 3c；5a 依赖 4a+4b；6b 依赖 6a+3c；6c 依赖 6b；6d 依赖 6a；7b 依赖 7a；7c 依赖 7a+7b；7d 依赖 7a；8a 依赖全部。
- 资源互斥：`event.rs` 被 5a、7b、7c 三个子步骤修改 → 三者严格串行（Wave 5 → Wave 7）。
- 无依赖可提前：7a（纯新模块）理论上可与 Wave 4/6 并行启动，但为控制 review 粒度建议按 Wave 顺序。

## 测试覆盖汇总

| Wave | 并行数 | 模块/crate | 步骤 | 测试用例数 |
|---|---|---|---|---|
| 1 | 2 | visp-proto；visp-core | 1a；1b | 3；5 |
| 2 | 2 | visp-config；visp-agent+visp-daemon | 2a；2b | 5；5 |
| 3 | 1（串行链） | visp-daemon（lib reload 模块） | 3a；3b；3c | 6；6；6 |
| 4 | 2 | visp-daemon（service）；visp-command | 4a；4b | 4；4 |
| 5 | 1 | visp-tui | 5a | 5 |
| 6 | 2（主链+分支） | visp-daemon（lib watch 模块 + tests/ + config） | 6a；6b；6c；6d | 6；8；4；4 |
| 7 | 2（主链+分支） | visp-tui（connection/event/app） | 7a；7b；7c；7d | 7；6；5；4 |
| 8 | 1 | workspace | 8a | 3 |
| **合计** | — | 7 个 crate | 20 子步骤 | **约 90** |

## 备注

1. **行号会漂移**：设计中的行号引用（orchestrator 6 消费点、main.rs 615-624 等）是 v2/v3.1 核实时的快照，实施时一律以 grep 重新定位为准，不做盲改。
2. **已知缺陷不修**：`append_system_prompt_template` 无去重、模板持续膨胀（`session.rs:417-426`）是既有缺陷，设计 §5.4 明确本次不修；验收 4 的手工核对需避开该 session 的并发输入干扰。实施中**严禁顺手修复**。
3. **macOS kqueue 时序**：本机集成测试运行在 kqueue 后端（workspace 显式启用该特性），事件到达时序有平台差异——watcher 测试一律以「最终重扫收敛」（有限时间轮询断言）为准，禁止对事件序列或精确次数做脆弱断言；debounce 时长在测试中注入（不睡真实 200ms 的整数倍）。
4. **VISP_CONFIG_DIR 类环境变量测试**：沿用 visp-config 既有做法（tempdir + 显式注入目录参数的内部钩子，如 load_skills 的测试隔离模式），避免 env 全局态在并行测试下互踩。
5. **proto 生成缓存**：改动 visp.proto 后若生成码未刷新，`cargo clean -p visp-proto` 后重跑（build.rs 的 rerun 触发依赖 tonic-build 内部指令）。
6. **event.rs 改动面**：约 1200 行主 select! 循环是最大风险点；5a 与 7 系列合并 review 时重点核对：exit 通道在所有新分支保留、per-tab 不变量（frames/rendered_up_to）、client 所有权替换后无悬空借用。
7. **回滚预案落点**：自动路径回滚 = 6d 的开关（改 daemon.toml 后重启 daemon）；看门狗回滚 = 7a 集中定义的常量（45s/5s）。两者都在对应子步骤落地，8a 做演练确认。
8. **单连接架构约束**：daemon 端 Chat 下行 rx 被 take 一次，通知发送天然 best-effort（6c 的测试已覆盖通道不可用路径）；重载核心与通知解耦，通知失败不影响 reload 结果。
9. **执行环境**：仓库已配置 codegraph MCP——执行 agent 可用 codegraph 定位符号与消费点（尤其 2b 的 6 处消费点核对），比 grep 更快；若未初始化索引，先 `codegraph init`。
10. **提交纪律**：每子步骤一个 commit，红绿分明的测试先行可从 diff 历史审计；任何子步骤发现设计偏差（如某消费点实际形态与设计不符），停下更新设计文档后再继续，不带病推进。
11. **proto RPC 与 server trait 的编译期耦合（实施中发现，2b 上报）**：1a 落地 `ReloadConfig` RPC 后，tonic 生成的 server trait 即**强制要求** `reload_config` 方法，导致 `visp-daemon` 在 4a 实现之前无法编译。实际处理：2b 在 `service.rs` 加了一个返回 `unimplemented` 的**占位方法**（无任何行为），4a 落真实 handler 时替换。后续若再新增 RPC，需注意此编译期耦合（或调整步骤顺序）。
12. **2a 的 rules 守卫落位（2a 上报）**：`RuleEngine::reload` 内部实现「无变化不写回」并返回 `changed`；**3b 的 rules 守卫据此消费 `changed`**，不重复比较。
