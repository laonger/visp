# visp-config crate 设计文档

## 1. 背景与动机

### 1.1 当前问题

| 问题 | 现状 | 影响 |
|------|------|------|
| 配置类型分散 | `DaemonConfig` 在 visp-daemon，`AgentConfig` 在 visp-core，`LlmConfig` 在 visp-llm，`CodeGraphConfig` 在 visp-codegraph | 下游 crate 无法独立获取配置，必须由 visp-daemon 拆解后逐字段传递 |
| 加载逻辑耦合 | `load_config()` 三级合并逻辑写在 visp-daemon/src/config.rs | 只有 daemon 能加载配置，其他 crate 若独立运行（如 CLI 模式）无法复用 |
| `.visp` 路径逻辑重复 | `.visp/rules/`、`.visp/skills/`、`.visp/system-prompt.md`、`.visp/daemon.toml` 的路径拼接散落在 rules.rs、session.rs、config.rs 多处 | 修改目录结构需改多处，容易遗漏 |
| 规则/技能加载分散 | `RuleEngine` 在 visp-core/src/rules.rs，`load_skills` 在 visp-core/src/session.rs，`load_system_prompt_template` 也在 session.rs | 职责不清，session.rs 承担了不该承担的物料加载职责 |
| `~/.config/visp` 全局路径重复 | 全局配置目录的路径拼接在 config.rs、rules.rs、session.rs 中各写了一遍 | 路径变更需改多处 |

### 1.2 目标

新建 `visp-config` crate，作为**全项目唯一的配置管理中心**。所有配置的读、写、优先级、传递都通过 visp-config 处理：

- **读**：配置文件加载与三级合并 + 环境变量覆盖（`load_config`）
- **优先级**：环境变量 > CLI 指定 > 项目配置 > 全局配置；运行时覆盖 > 静态配置
- **传递**：配置合并、转换、下发均通过 visp-config 的 API，调用方不自行拼接配置
- **写**：运行时配置更新（`ConfigUpdate`、session 级覆盖）通过 visp-config 处理（持久化暂不实现）

具体职责：
- 配置类型定义（DaemonConfig 及其子节）
- 配置文件加载与三级合并 + 环境变量覆盖（`load_config`）
- 运行时配置合并（`merge_session_config`：客户端 config + model_key 解析 + daemon 默认值）
- 运行时配置更新（`apply_config_update`：处理 `/model`、`/temp` 等命令）
- proto LlmConfig <-> core LlmConfig 转换
- `.visp` 目录物料管理（rules、skills、AGENTS.md、system-prompt.md）
- 全局路径（`~/.config/visp/`、`~/.visp/`）管理

### 1.3 非目标

- 不改变现有配置文件格式（daemon.toml / rules/*.md / skills/*/SKILL.md）
- 不改变 agent loop 的运行时行为
- 配置持久化（写回 daemon.toml）暂不实现，后续迭代
- 不重构 MCP 协议通信（proto 消息定义保留在 visp-proto）

## 2. 架构设计

### 2.1 crate 定位

```
visp-proto (叶子 crate, 仅依赖 tonic/prost)
  ↑
visp-config (依赖 visp-proto; 不依赖其他 visp-* crate)
  ├── 依赖: serde, toml, tracing, visp-proto
  ├── 被依赖: visp-core, visp-llm, visp-mcp, visp-daemon, visp-tools, visp
```

`visp-config` 是叶子 crate，不依赖其他 visp crate（除 visp-proto）。所有需要配置的 crate 依赖它。

`visp-llm` 直接依赖 visp-config 获取 `LlmConfig`，不再通过 visp-core re-export。依赖链无循环：

```
visp-proto → visp-config → visp-core → visp-agent, visp-daemon
                         → visp-llm ↗ (visp-core dev-dep only)
```

### 2.2 模块划分

```
visp-config/src/
  lib.rs              -- 公开 API，re-export 子模块
  config.rs           -- DaemonConfig 及所有子节类型定义 + load_config()
  path.rs             -- 路径解析: project_dir(), global_dir(), visp_dir()
  rules.rs            -- RuleEngine, RuleSet, RuleFile, AGENTS.md 发现
  skills.rs           -- BuiltinSkill, load_skills(), 技能发现
  prompt.rs           -- load_system_prompt_template()
```

### 2.3 配置类型迁移

将以下类型从原位置迁移到 visp-config：

| 类型 | 原位置 | 迁移后 |
|------|--------|--------|
| `DaemonConfig` | visp-daemon/src/config.rs | visp-config/src/config.rs |
| `DaemonSection` | visp-daemon/src/config.rs | visp-config/src/config.rs |
| `LlmSection` / `LlmModelConfig` | visp-daemon/src/config.rs | visp-config/src/config.rs |
| `ToolsSection` | visp-daemon/src/config.rs | visp-config/src/config.rs |
| `AgentSection` / `BuiltinAgentConfig` | visp-daemon/src/config.rs | visp-config/src/config.rs |
| `StorageSection` | visp-daemon/src/config.rs | visp-config/src/config.rs |
| `ObservabilityConfig` 及子类型 | visp-daemon/src/config.rs | visp-config/src/config.rs |
| `McpConfig` | visp-mcp/src/config.rs | visp-config/src/config.rs |
| `LlmConfig` | visp-core/src/provider.rs | visp-config/src/config.rs |
| `ModelInfo` | visp-core/src/provider.rs | visp-config/src/config.rs |
| `DEFAULT_SYSTEM_PROMPT` | visp-core/src/prompt.rs | visp-config/src/prompt.rs |
| `RuleEngine` / `RuleSet` / `RuleFile` | visp-core/src/rules.rs | visp-config/src/rules.rs |
| `BuiltinSkill` / `builtin_skills()` | visp-core/src/skill.rs | visp-config/src/skills.rs |
| `load_skills()` / `load_skills_inner()` | visp-core/src/session.rs | visp-config/src/skills.rs |
| `load_system_prompt_template()` | visp-core/src/session.rs | visp-config/src/prompt.rs |
| `home_dir()` | visp-core/src/session.rs | visp-config/src/path.rs |

> **注意**：类型迁移时，其 `impl` 块中的方法一并迁移。例如 `LlmSection` 的 `effective_models()`、`available_models()`、`resolve_default_key()`，`LlmModelConfig` 的 `key()`、`model_alias()`、`matches_key()`、`matches_model()`、`merge_override()`，`LlmConfig` 的 `Default` 实现等。私有辅助函数如 `merge_llm_sections()`、`merge_agent_builtins()`、`default_storage_path()` 也随 `load_config()` 一起迁移。

**保留原地的类型**：

| 类型 | 原位置 | 原因 |
|------|--------|------|
| `AgentConfig` | visp-core/src/agent.rs | agent loop 运行时配置，由 visp-daemon 从 AgentSection 转换而来 |
| `AgentLoopContext` | visp-core/src/agent.rs | 运行时上下文，非配置 |
| `CodeGraphConfig` | visp-codegraph | codegraph 内部配置，可后续迁移 |

### 2.4 路径解析（path.rs）

统一管理所有路径：

- `project_dir() -> PathBuf` — 当前工作目录（`std::env::current_dir()`）
- `visp_dir(project: &Path) -> PathBuf` — `{project}/.visp`
- `global_dir() -> Option<PathBuf>` — `~/.config/visp`（跨平台）
- `global_visp_dir() -> Option<PathBuf>` — `~/.config/visp` 的别名
- `daemon_toml_project(project: &Path) -> PathBuf` — `{project}/.visp/daemon.toml`
- `daemon_toml_global() -> Option<PathBuf>` — `~/.config/visp/daemon.toml`
- `rules_dir_project(project: &Path) -> PathBuf` — `{project}/.visp/rules`
- `rules_dir_global() -> Option<PathBuf>` — `~/.config/visp/rules`
- `skills_dir_project(project: &Path) -> PathBuf` — `{project}/.visp/skills`
- `skills_dir_global() -> Option<PathBuf>` — `~/.config/visp/skills`
- `system_prompt_project(project: &Path) -> PathBuf` — `{project}/.visp/system-prompt.md`
- `system_prompt_global() -> Option<PathBuf>` — `~/.config/visp/system-prompt.md`
- `global_agents_md() -> Option<PathBuf>` — `~/.config/visp/AGENTS.md`

### 2.5 依赖关系处理

**McpConfig 迁移**：原定义在 visp-mcp 中，但 visp-mcp 依赖 visp-core（Tool trait），若 visp-config 依赖 visp-mcp 会形成循环依赖。因此将 `McpConfig` 迁移到 visp-config，visp-mcp 通过 re-export 保持对外 API 不变。

**LlmConfig 迁移**：原定义在 visp-core/src/provider.rs，运行时配置管理 API（`merge_session_config`、`apply_config_update` 等）需要操作 `LlmConfig`。若 visp-config 依赖 visp-core 会形成循环依赖（visp-core 依赖 visp-config）。因此将 `LlmConfig` 也迁移到 visp-config，visp-core re-export 保持兼容。

**ProtoLlmConfig 依赖**：proto 类型由 protobuf 生成，定义保留在 visp-proto（纯叶子 crate，仅依赖 tonic/prost）。visp-config 依赖 visp-proto 以实现 proto <-> core 转换。

依赖图（无循环）：

```
visp-proto (叶子 crate, 仅依赖 tonic/prost)
  ↑
visp-config (依赖 visp-proto; 不依赖其他 visp-* crate)
  ↑
visp-core (依赖 visp-config: RuleEngine, LlmConfig, skills, prompt 等)
visp-mcp (依赖 visp-config: McpConfig; 依赖 visp-core: Tool trait)
  ↑
visp-agent, visp-tools, visp-daemon, visp-cli, visp
```

| crate | 依赖 visp-config 的内容 |
|-------|------------------------|
| visp-core | RuleEngine, RuleSet, LlmConfig, ModelInfo, load_skills, BuiltinSkill, load_system_prompt_template, DEFAULT_SYSTEM_PROMPT |
| visp-llm | LlmConfig, ModelInfo (直接依赖) |
| visp-mcp | McpConfig (re-export) |
| visp-daemon | DaemonConfig, load_config, ObservabilityConfig, 所有配置类型 |
| visp-tools | load_skills (SkillTool) |
| visp | load_config (获取 listen_addr), path::log_dir |
| visp-agent | 通过 visp-core 间接依赖 |

### 2.6 配置加载流程

```
load_config(config_path: Option<&Path>) -> Result<DaemonConfig, String>

1. CLI 指定路径 -> 直接加载（最高优先级）
2. 全局 ~/.config/visp/daemon.toml -> 加载为 base
3. 项目 .visp/daemon.toml -> merge 到 base（项目优先）
   - 合并 [llm] 全部字段
   - 合并 [[agent.builtin]]（按 name 字段级合并）
4. 环境变量覆盖 -> 在 merge 完成后应用
   - VISP_LISTEN_ADDR -> 覆盖 daemon.listen_addr
   - OPENAI_API_KEY -> 回退填充 llm.models[*].api_key（当 api_key 为 None 时）
   - ANTHROPIC_API_KEY -> 同上（protocol 为 anthropic 时）
   - RUST_LOG -> 覆盖 observability.level
```

优先级从高到低：环境变量 > CLI 指定 > 项目配置 > 全局配置。

`load_config` 返回的 `DaemonConfig` 中所有字段已是最终值，调用方无需再读取环境变量。

### 2.7 规则加载流程（不变，迁移位置）

```
RuleEngine::new(project_path) -> io::Result<Self>

1. AGENTS.md: 从 project_path 向上遍历到根目录，收集所有 AGENTS.md（近 -> 远）
2. 全局 AGENTS.md: ~/.config/visp/AGENTS.md
3. 项目规则: .visp/rules/*.md（alwaysApply: true）
4. 全局规则: ~/.config/visp/rules/*.md（alwaysApply: true）

合并为 RuleSet.content（"\n\n" 连接）
```

### 2.8 技能加载流程（不变，迁移位置）

```
load_skills(project_path) -> String

1. 内置技能（最低优先级，可被覆盖）
2. 全局技能: ~/.config/visp/skills/*/SKILL.md
3. 项目技能: .visp/skills/*/SKILL.md（最高优先级）

同名技能: 项目 > 全局 > 内置
```

### 2.9 运行时配置管理

当前运行时配置逻辑散落在 visp-daemon/service.rs 和 visp-agent/orchestrator.rs，违反"所有配置管理通过 visp-config"原则。以下逻辑迁移到 visp-config：

#### 2.9.1 proto <-> core LlmConfig 转换

当前 `map_llm_config`（`service.rs:1179-1197`）将 proto `LlmConfig` 转换为 core `LlmConfig`。

迁移到 visp-config：
- `proto_to_llm_config(proto: &ProtoLlmConfig) -> LlmConfig` - proto 转 core
- `llm_config_to_proto(config: &LlmConfig) -> ProtoLlmConfig` - core 转 proto（用于 CLI -> daemon 通信）

注意：proto 类型定义保留在 visp-proto，visp-config 依赖 visp-proto。

**字段差距说明**：proto LlmConfig 只有 6 个字段（model, model_key, temperature, max_tokens, max_context_tokens, extra），core LlmConfig 有 23 个字段。差距的 17 个字段包括：provider、langfuse 系列（11 个）、use_tool、image_generation。

- `proto_to_llm_config`：映射 6 个字段，其余 17 个用 `LlmConfig::default()` 填充。`provider`、`use_tool`、`image_generation`、langfuse 系列字段在 proto 层不存在，由 daemon 侧后续通过 `merge_session_config` / `build_llm_config_from_model` 填充。
- `llm_config_to_proto`：映射 6 个字段，丢弃 17 个。这些字段是服务端运行时状态，不需要回传给客户端。

#### 2.9.2 session 配置合并

当前 `create_session`（`service.rs:270-371`）中的合并逻辑：

```
优先级：客户端 config > model_key 解析 > daemon 默认值
```

具体逻辑：
1. 客户端传入的 proto LlmConfig -> 转换为 core LlmConfig
2. 若 `extra` 为空，填充 daemon 默认 extra
3. 若指定 `model_key`，从 `DaemonConfig.llm.models` 查找匹配的 `LlmModelConfig`，用其覆盖未显式设置的字段（model、provider、max_tokens、max_context_tokens、temperature、thinking_budget_tokens）
4. 若 model 仍是默认值，用 daemon 默认 model 填充

迁移到 visp-config：
```
merge_session_config(
    client_config: Option<&ProtoLlmConfig>,
    daemon_config: &DaemonConfig,
) -> LlmConfig
```

daemon service.rs 调用此函数，不再自行拼接配置。

> **未设置字段检测**：通过比较 `LlmConfig::default()` 的字段值来判断客户端是否显式设置了某字段。例如 `config.model == LlmConfig::default().model` 表示客户端未传 model。浮点数用 `f64::EPSILON` 容差比较。`merge_session_config` 和 `apply_config_update` 保留此 sentinel 比较行为。

#### 2.9.3 运行时配置更新（ConfigUpdate）

当前 `ConfigUpdate` handler（`service.rs:656-741`）处理 `/model`、`/temp` 等命令：

1. proto LlmConfig -> core LlmConfig 转换
2. 若指定 `model_key`，从 `DaemonConfig.llm.models` 查找并填充
3. 调用 `session.update_config()` 写入 session

迁移到 visp-config：
```
apply_config_update(
    current: &LlmConfig,
    update: &ProtoLlmConfig,
    daemon_config: &DaemonConfig,
) -> LlmConfig
```

返回更新后的 `LlmConfig`，daemon 调用 `session.update_config()` 持久化。

#### 2.9.4 agent 级 model 覆盖

当前 orchestrator（`orchestrator.rs:665-711`）对 sub-agent 的 model/temperature 覆盖直接修改 `LlmConfig` 字段。

这部分涉及 `AgentDefinition`（visp-core 类型）和 `resolve_model_info`（orchestrator 方法），迁移会造成循环依赖。

**决策**：agent 级覆盖逻辑保留在 orchestrator，但 model 解析（从 `LlmModelConfig` 提取字段填充 `LlmConfig`）抽成 visp-config 的公共函数：
```
apply_model_override(
    config: &mut LlmConfig,
    model_cfg: &LlmModelConfig,
)
```

orchestrator 调用此函数，不再自行逐字段赋值。`resolve_model_info`（从 `DaemonConfig.llm.models` 查找匹配的 `LlmModelConfig`）也迁移到 visp-config：
```
resolve_model(
    model_key: &str,
    daemon_config: &DaemonConfig,
) -> Option<LlmModelConfig>
```

#### 2.9.5 provider 解析的 model_key 选择

当前 `resolve_provider`（`orchestrator.rs:1167-1198`）是 4 级查找链：agent.model_key -> session.model_key -> session.model -> default_provider_key。返回 `Arc<dyn LlmProvider>` 实例。

**决策**：拆分职责。model_key 选择优先级迁移到 visp-config：
```
resolve_model_key(
    agent_model: Option<&str>,
    session_config: &LlmConfig,
    daemon_config: &DaemonConfig,
) -> String
```

返回最终选定的 model_key。orchestrator 拿到 model_key 后自行从 `self.providers` 查找 provider 实例。provider 实例管理不属于配置管理，保留在 orchestrator。

#### 2.9.6 从 LlmModelConfig 构造 LlmConfig

当前 `service.rs:171-200` 从 `LlmModelConfig` 逐字段构造 `LlmConfig`（含 langfuse 字段），与 `apply_model_override` 高度重合。

迁移到 visp-config：
```
/// 从 LlmModelConfig 构造完整的 LlmConfig。
///
/// `langfuse_cfg` 提供 langfuse 相关字段（enabled、session_id、trace_name 等），
/// 这些字段不在 LlmModelConfig 中，而是来自 ObservabilityConfig.langfuse_*。
/// 传入 None 时 langfuse 字段全部使用默认值（disabled）。
build_llm_config_from_model(
    model_cfg: &LlmModelConfig,
    langfuse_cfg: Option<&ObservabilityConfig>,
) -> LlmConfig
```

daemon 的 `default_llm_config` 构造和 service.rs 的测试都调用此函数，不再自行逐字段赋值。

### 2.10 公开 API

visp-config 对外暴露：

- **配置**: `DaemonConfig` 及所有子类型、`LlmConfig`、`ModelInfo`、`load_config()`
- **运行时配置**: `merge_session_config()`、`apply_config_update()`、`apply_model_override()`、`resolve_model()`、`resolve_model_key()`、`model_config_to_info()`、`build_llm_config_from_model()`、`proto_to_llm_config()`、`llm_config_to_proto()`
- **路径**: `path` 模块的所有路径函数
- **规则**: `RuleEngine`、`RuleSet`、`RuleFile`
- **技能**: `BuiltinSkill`、`builtin_skills()`、`find_builtin_skill()`、`load_skills()`
- **Prompt**: `load_system_prompt_template()`、`DEFAULT_SYSTEM_PROMPT`

## 3. 迁移计划

### 阶段 1: 创建 crate + 迁移类型

1. 创建 `crates/visp-config/Cargo.toml`
2. 将 visp-daemon/src/config.rs 的类型定义和 load_config 迁移到 visp-config/src/config.rs
3. 将 visp-daemon/src/config.rs 中的 ~60 个单元测试随 config.rs 一起迁移到 visp-config
4. 将 visp-mcp/src/config.rs 的 McpConfig 迁移到 visp-config/src/config.rs
5. 将 visp-core/src/rules.rs 迁移到 visp-config/src/rules.rs（含 `has_always_apply_true` 等辅助函数）
6. 将 visp-core/src/skill.rs 迁移到 visp-config/src/skills.rs（包含内置技能定义）
7. 将 visp-core/src/session.rs 中的 `load_skills`、`load_skills_inner`、`home_dir` 迁移到 visp-config
8. 将 visp-core/src/session.rs 中的 `load_system_prompt_template` 和 visp-core/src/prompt.rs 中的 `DEFAULT_SYSTEM_PROMPT` 迁移到 visp-config/src/prompt.rs；同时将 prompt.rs 中依赖该常量的 4 个测试（test_default_prompt_contains_role 等）迁移到 visp-config，保留 PromptBuilder 的测试在 visp-core
9. 将 visp-core/src/provider.rs 中的 `LlmConfig` 和 `ModelInfo` 迁移到 visp-config/src/config.rs
10. 将 visp-daemon/src/main.rs 中 `LlmModelConfig` -> `ModelInfo` 的构造逻辑迁移为 visp-config 的 `model_config_to_info`
11. 将 visp-daemon/src/service.rs 中的 `map_llm_config` 迁移为 visp-config 的 `proto_to_llm_config` / `llm_config_to_proto`
12. 将 visp-daemon/src/service.rs 中 `create_session` 的配置合并逻辑迁移为 visp-config 的 `merge_session_config`
13. 将 visp-daemon/src/service.rs 中 `ConfigUpdate` handler 的配置更新逻辑迁移为 visp-config 的 `apply_config_update`
14. 将 visp-daemon/src/service.rs 中 model_key 解析、default_llm_config 构造和 orchestrator 中的 model 覆盖逻辑迁移为 visp-config 的 `resolve_model` / `apply_model_override` / `resolve_model_key` / `build_llm_config_from_model`
15. 创建 visp-config/src/path.rs 统一路径管理（含 `expand_home`）

### 阶段 2: 更新依赖

1. visp-daemon/Cargo.toml 添加 `visp-config = { path = "../visp-config" }`
2. visp-core/Cargo.toml 添加 `visp-config = { path = "../visp-config" }`
3. visp-llm/Cargo.toml 添加 `visp-config = { path = "../visp-config" }`，移除对 visp-core 的 `LlmConfig`/`ModelInfo` 引用
4. visp-mcp/Cargo.toml 添加 `visp-config = { path = "../visp-config" }`
5. visp-agent/Cargo.toml 添加 `visp-config = { path = "../visp-config" }`（如需要）
6. visp-tools/Cargo.toml 添加 `visp-config = { path = "../visp-config" }`
7. visp-cli/Cargo.toml 添加 `visp-config = { path = "../visp-config" }`
8. visp/Cargo.toml 添加 `visp-config = { path = "../visp-config" }`
9. visp-command/Cargo.toml 添加 `visp-config = { path = "../visp-config" }`
10. visp-db/Cargo.toml 添加 `visp-config = { path = "../visp-config" }`
11. workspace Cargo.toml members 添加 visp-config

### 阶段 3: 更新引用

1. visp-daemon: `use visp_config::{DaemonConfig, load_config, merge_session_config, apply_config_update, model_config_to_info, build_llm_config_from_model, ...}`
2. visp-daemon: main.rs 中 `LlmModelConfig` -> `ModelInfo` 构造改为调用 `visp_config::model_config_to_info`
3. visp-daemon: service.rs 中 `default_llm_config` 构造改为调用 `visp_config::build_llm_config_from_model`
4. visp-daemon: service.rs 中 `create_session` 改为调用 `visp_config::merge_session_config`
5. visp-daemon: service.rs 中 `ConfigUpdate` handler 改为调用 `visp_config::apply_config_update`
6. visp-daemon: service.rs 中删除 `map_llm_config`，改为 `visp_config::proto_to_llm_config`
7. visp-agent: orchestrator.rs 中 model 覆盖改为调用 `visp_config::apply_model_override` / `visp_config::resolve_model` / `visp_config::resolve_model_key`
8. visp-core: `use visp_config::{RuleEngine, LlmConfig, ModelInfo, ...}`；删除 rules.rs、skill.rs
9. visp-core: session.rs 删除 `load_skills`、`load_system_prompt_template`，改为 re-export 或直接调用 visp-config
10. visp-tools: `use visp_config::load_skills`（SkillTool）
11. visp-daemon: observability 模块 `use visp_config::ObservabilityConfig`
12. visp-db: `use visp_config::path::expand_home`
13. visp-command: `use visp_config::path::{agents_dir_project, skills_dir_project}`
14. visp-llm: `use visp_config::{LlmConfig, ModelInfo}` 替换 `use visp_core::provider::{LlmConfig, ModelInfo}`（保留 `use visp_core::provider::{ChatEvent, LlmProvider}` 不变）

### 阶段 4: 向后兼容 re-export

为减少破坏性变更，在原位置提供 re-export：

- visp-core/src/rules.rs: `pub use visp_config::rules::*;`
- visp-core/src/skill.rs: `pub use visp_config::skills::*;`
- visp-core/src/provider.rs: `pub use visp_config::{LlmConfig, ModelInfo};`
- visp-core/src/session.rs: `pub use visp_config::skills::load_skills;`、`pub use visp_config::path::home_dir;`
- visp-core/src/prompt.rs: `pub use visp_config::prompt::DEFAULT_SYSTEM_PROMPT;`
- visp-daemon/src/config.rs: `pub use visp_config::{DaemonConfig, ...};`
- visp-mcp/src/config.rs: `pub use visp_config::McpConfig;`

这样 visp-daemon（`visp_core::session::home_dir`）和 visp-db（`visp_core::session::home_dir`）等现有引用无需修改。

## 4. 影响范围

### 需要修改的文件

| crate | 文件 | 变更 |
|-------|------|------|
| visp-config | 新建 | 全新 crate |
| visp-daemon | config.rs | 类型迁移走，保留 re-export 或直接引用 |
| visp-daemon | main.rs | `load_config` 调用改为 `visp_config::load_config`；agents 目录路径改用 `visp_config::path::agents_dir_*`；`ModelInfo` 构造改为 `visp_config::model_config_to_info`；移除 `VISP_LISTEN_ADDR` 环境变量覆盖逻辑（已由 `load_config` 处理）；`create_llm_provider` 移除 API Key 环境变量回退逻辑（已由 `load_config` 处理） |
| visp-daemon | service.rs | 类型引用改为 `visp_config::`；`create_llm_provider` 移除 API Key 环境变量回退逻辑；`default_llm_config` 构造改为调用 `visp_config::build_llm_config_from_model`；`create_session` 配置合并改为调用 `visp_config::merge_session_config`；`ConfigUpdate` 改为调用 `visp_config::apply_config_update`；删除 `map_llm_config` |
| visp-daemon | observability/init.rs | `ObservabilityConfig` 引用改为 `visp_config::`；移除 `RUST_LOG` 环境变量覆盖逻辑，直接使用 `cfg.level`（已由 `load_config` 处理）；`~/` 路径展开改用 `visp_config::path::expand_home` |
| visp-core | provider.rs | `LlmConfig`、`ModelInfo` 迁移走，保留 re-export |
| visp-llm | openai.rs, anthropic.rs, mock.rs | `LlmConfig`、`ModelInfo` 引用改为 `visp_config::`，不再通过 visp-core re-export |
| visp-core | rules.rs | 迁移走，保留 re-export |
| visp-core | skill.rs | 迁移走，保留 re-export |
| visp-core | session.rs | `load_skills`、`load_system_prompt_template`、`home_dir` 迁移走，保留 re-export |
| visp-core | prompt.rs | `DEFAULT_SYSTEM_PROMPT` 迁移走，保留 re-export；4 个相关测试迁移到 visp-config |
| visp-core | agent_loop.rs | `RuleEngine` 引用路径更新（通过 re-export 无需改） |
| visp-agent | orchestrator.rs | model 覆盖改为调用 `visp_config::apply_model_override` / `visp_config::resolve_model`；`resolve_provider` 的 model_key 选择改为调用 `visp_config::resolve_model_key`，provider 实例查找保留 |
| visp-mcp | config.rs | `McpConfig` 迁移走，保留 re-export |
| visp-tools | skill.rs | `load_skills` 引用改为 `visp_config::`（或通过 re-export 无需改）；`env::var("HOME")` 全局技能路径改用 `visp_config::path::skills_dir_global` |
| visp-tools | fetch.rs | `.visp/webfetch.toml` 路径改用 `visp_config::path::webfetch_toml_project` |
| visp-tools | codegraph.rs | `.visp/codegraph.db` 路径改用 `visp_config::path::codegraph_db`（5 处） |
| visp-db | store.rs | `~/` 路径展开改用 `visp_config::path::expand_home`（`home_dir` 通过 re-export 兼容） |
| visp | main.rs | `get_log_dir()` 改用 `visp_config::path::log_dir`；启动 daemon 前调用 `visp_config::load_config()` 读取配置获取基础 listen_addr，找到可用端口后通过 `VISP_LISTEN_ADDR` 环境变量注入给 daemon 子进程 |
| visp-cli | image.rs | `.visp/images` 临时缓存路径改用 `visp_config::path::image_cache_dir` |
| visp-command | init_agent.rs | `.visp/agents/` 路径改用 `visp_config::path::agents_dir_project` |
| visp-command | init_skill.rs | `.visp/skills/` 路径改用 `visp_config::path::skills_dir_project` |
| Cargo.toml | workspace | members 添加 visp-config |

### 不受影响

- agent loop 运行逻辑
- LLM provider 实现
- tool 执行逻辑
- session 管理逻辑
- 配置文件格式
- CLI 端 proto `LlmConfig` 构造（`/model`、`/temp`、CLI flags）保留在 visp-cli，属于 gRPC 协议层操作
- `BuiltinAgentOverride` 及 agent 文件加载逻辑（保留在 visp-agent/agent_loader.rs）

## 5. 验收标准

- `cargo build --workspace` 通过
- `cargo test --workspace` 全部通过
- `cargo clippy --workspace` 无 warning
- `cargo fmt --check` 通过
- visp-daemon 启动后配置加载行为与迁移前一致
- rules/skills/system-prompt 加载结果与迁移前一致
