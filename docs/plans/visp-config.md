# visp-config 实施计划

## 概述

将分散在 visp-daemon、visp-core、visp-mcp、visp-llm、visp-tools、visp-command、visp、visp-cli、visp-db 中的配置管理逻辑，统一迁移到新建的 `visp-config` crate。涵盖配置的读、写、优先级、传递四个维度。

设计文档：`docs/design/visp-config.md`

---

## Wave 1：crate 骨架 + path.rs + 依赖声明（串行，3 步）

### Step 1a：创建 visp-config crate 骨架

#### 🟢 绿 - 实现
1. 创建 `crates/visp-config/Cargo.toml`，依赖 serde、toml、tracing、visp-proto、chrono、async-trait
2. 创建 `crates/visp-config/src/lib.rs`，声明模块：config、path、rules、skills、prompt
3. 在 workspace Cargo.toml 的 members 中添加 `crates/visp-config`
4. 创建空的模块文件：config.rs、path.rs、rules.rs、skills.rs、prompt.rs

#### 📦 提交
`feat(visp-config): create crate skeleton`

---

### Step 1b：迁移 path.rs

#### 🔴 红 - 测试
| # | 测试用例 | 描述 |
|---|---------|------|
| 1 | `test_project_dir` | 返回当前工作目录 |
| 2 | `test_visp_dir` | `{project}/.visp` 拼接 |
| 3 | `test_global_config_dir` | `~/.config/visp`（HOME 设置时） |
| 4 | `test_global_config_dir_no_home` | HOME 未设置时返回 None |
| 5 | `test_global_data_dir` | `~/.visp`（HOME 设置时） |
| 6 | `test_daemon_toml_paths` | project 和 global 的 daemon.toml 路径 |
| 7 | `test_rules_dir_paths` | project 和 global 的 rules 目录 |
| 8 | `test_skills_dir_paths` | project 和 global 的 skills 目录 |
| 9 | `test_agents_dir_paths` | project 和 global 的 agents 目录 |
| 10 | `test_webfetch_toml_project` | `{project}/.visp/webfetch.toml` |
| 11 | `test_system_prompt_paths` | project 和 global 的 system-prompt.md |
| 12 | `test_global_agents_md` | `~/.config/visp/AGENTS.md` |
| 13 | `test_codegraph_db` | `{project}/.visp/codegraph.db` |
| 14 | `test_log_dir` | `~/.visp/logs` |
| 15 | `test_image_cache_dir` | `{temp_dir}/.visp/images` |
| 16 | `test_expand_home_with_tilde` | `~/foo` 展开为 `$HOME/foo` |
| 17 | `test_expand_home_without_tilde` | `/abs/path` 原样返回 |
| 18 | `test_expand_home_no_home` | `~/foo` 但 HOME 未设置时的行为 |
| 19 | `test_home_dir` | 返回 `$HOME` 的 PathBuf |

#### 🟢 绿 - 实现
将 visp-core/src/session.rs:342 的 `home_dir()` 迁移到 path.rs，并新增所有路径函数（`project_dir`、`visp_dir`、`global_config_dir`、`global_data_dir`、`daemon_toml_project/global`、`rules_dir_project/global`、`skills_dir_project/global`、`agents_dir_project/global`、`webfetch_toml_project`、`system_prompt_project/global`、`global_agents_md`、`codegraph_db`、`log_dir`、`image_cache_dir`、`expand_home`）。

#### 🧪 测试
```bash
cargo test -p visp-config
cargo clippy -p visp-config
```

#### 📦 提交
`feat(visp-config): migrate path management to path.rs`

---

### Step 1c：更新所有 Cargo.toml 依赖

#### 🟢 绿 - 实现
为以下 crate 的 Cargo.toml 添加 `visp-config = { path = "../visp-config" }`：
visp-core、visp-llm、visp-mcp、visp-daemon、visp-agent、visp-tools、visp-cli、visp、visp-command、visp-db

#### 🧪 测试
```bash
cargo build --workspace
```

#### 📦 提交
`build: add visp-config dependency to all crates`

---

## Wave 2：类型迁移（2 路并行，依赖 Wave 1）

### 任务 A：迁移 config.rs

#### Step 2a：迁移 DaemonConfig + McpConfig + LlmConfig + ModelInfo

#### 🟢 绿 - 实现
1. 将 visp-daemon/src/config.rs 全部类型定义迁移到 visp-config/src/config.rs：
   - `DaemonConfig`、`DaemonSection`、`LlmSection`、`LlmModelConfig`、`ToolsSection`、`AgentSection`、`BuiltinAgentConfig`、`StorageSection`、`ObservabilityConfig` 及子类型
   - 所有 `impl` 块方法（`effective_models()`、`available_models()`、`resolve_default_key()`、`key()`、`model_alias()`、`matches_key()`、`matches_model()`、`merge_override()` 等）
   - 私有函数 `merge_llm_sections()`、`merge_agent_builtins()`、`default_storage_path()` 等
   - `load_config()` 函数，新增环境变量覆盖逻辑（`VISP_LISTEN_ADDR`、`OPENAI_API_KEY`、`ANTHROPIC_API_KEY`、`RUST_LOG`）
2. 将 visp-mcp/src/config.rs 的 `McpConfig` 迁移到 visp-config/src/config.rs
3. 将 visp-core/src/provider.rs 的 `LlmConfig`、`ModelInfo` 及 `impl Default for LlmConfig` 迁移到 visp-config/src/config.rs
4. 将 visp-daemon/src/config.rs 中的 ~60 个单元测试迁移到 visp-config

#### 🧪 测试
```bash
cargo test -p visp-config
cargo clippy -p visp-config
```

#### 📦 提交
`feat(visp-config): migrate DaemonConfig, McpConfig, LlmConfig, ModelInfo types`

---

### 任务 B：迁移 rules + skills + prompt

#### Step 2b：迁移 rules.rs、skills.rs、prompt.rs

#### 🟢 绿 - 实现
1. 将 visp-core/src/rules.rs 全部迁移到 visp-config/src/rules.rs（含 `RuleEngine`、`RuleSet`、`RuleFile`、`has_always_apply_true` 等辅助函数及测试）
2. 将 visp-core/src/skill.rs 全部迁移到 visp-config/src/skills.rs（含 `BuiltinSkill`、`builtin_skills()`、`find_builtin_skill()` 及测试）
3. 将 visp-core/src/session.rs 中的 `load_skills`、`load_skills_inner` 迁移到 visp-config/src/skills.rs
4. 将 visp-core/src/session.rs 中的 `load_system_prompt_template` 和 visp-core/src/prompt.rs 中的 `DEFAULT_SYSTEM_PROMPT` 迁移到 visp-config/src/prompt.rs；同时将 prompt.rs 中依赖该常量的 4 个测试迁移到 visp-config，保留 PromptBuilder 的测试在 visp-core

#### 🧪 测试
```bash
cargo test -p visp-config
cargo clippy -p visp-config
```

#### 📦 提交
`feat(visp-config): migrate rules, skills, prompt modules`

---

## Wave 3：re-export（串行，1 步，依赖 Wave 2）

### Step 3a：添加 re-export 到原位置

#### 🟢 绿 - 实现
1. visp-core/src/rules.rs: 替换为 `pub use visp_config::rules::*;`
2. visp-core/src/skill.rs: 替换为 `pub use visp_config::skills::*;`
3. visp-core/src/provider.rs: 添加 `pub use visp_config::{LlmConfig, ModelInfo};`，删除原定义
4. visp-core/src/session.rs: 添加 `pub use visp_config::skills::load_skills;`、`pub use visp_config::path::home_dir;`、`pub use visp_config::load_system_prompt_template;`
5. visp-core/src/prompt.rs: 替换为 `pub use visp_config::prompt::DEFAULT_SYSTEM_PROMPT;`
6. visp-daemon/src/config.rs: 替换为 `pub use visp_config::{DaemonConfig, DaemonSection, LlmSection, LlmModelConfig, ToolsSection, AgentSection, BuiltinAgentConfig, StorageSection, ObservabilityConfig, load_config};`
7. visp-mcp/src/config.rs: 替换为 `pub use visp_config::McpConfig;`

#### 🧪 测试
```bash
cargo build --workspace
cargo test --workspace
```

#### 📦 提交
`refactor: add re-exports for migrated config types`

---

## Wave 4：运行时工具函数 + visp-llm 更新（2 路并行，依赖 Wave 3）

### 任务 A：运行时配置工具函数

#### Step 4a：proto 转换 + model_config_to_info + resolve + override

#### 🔴 红 - 测试
| # | 测试用例 | 描述 |
|---|---------|------|
| 1 | `test_proto_to_llm_config_full` | 6 个字段全部映射 |
| 2 | `test_proto_to_llm_config_empty` | proto 为空时返回 Default |
| 3 | `test_proto_to_llm_config_partial` | 只传 model，其余 Default |
| 4 | `test_llm_config_to_proto` | core 转 proto，6 个字段映射，17 个丢弃 |
| 5 | `test_model_config_to_info` | LlmModelConfig 转 ModelInfo，所有字段正确 |
| 6 | `test_model_config_to_info_minimal` | LlmModelConfig 只有必填字段时 |
| 7 | `test_resolve_model_found` | model_key 匹配，返回 LlmModelConfig |
| 8 | `test_resolve_model_not_found` | model_key 不匹配，返回 None |
| 9 | `test_resolve_model_key_agent` | agent.model 优先 |
| 10 | `test_resolve_model_key_session` | session.model_key 回退 |
| 11 | `test_resolve_model_key_session_model` | session.model 回退 |
| 12 | `test_resolve_model_key_default` | 全部不匹配，返回 default |
| 13 | `test_apply_model_override_full` | 所有字段覆盖 |
| 14 | `test_apply_model_override_partial` | 部分字段为 None，不覆盖 |

#### 🟢 绿 - 实现
在 visp-config/src/config.rs 新增：
1. `proto_to_llm_config(proto: &ProtoLlmConfig) -> LlmConfig`
2. `llm_config_to_proto(config: &LlmConfig) -> ProtoLlmConfig`
3. `model_config_to_info(mc: &LlmModelConfig) -> ModelInfo`
4. `resolve_model(model_key: &str, daemon_config: &DaemonConfig) -> Option<LlmModelConfig>`
5. `resolve_model_key(agent_model: Option<&str>, session_config: &LlmConfig, daemon_config: &DaemonConfig) -> String`
6. `apply_model_override(config: &mut LlmConfig, model_cfg: &LlmModelConfig)`

#### 🧪 测试
```bash
cargo test -p visp-config
cargo clippy -p visp-config
```

#### 📦 提交
`feat(visp-config): add proto conversion, model_config_to_info, resolve, override utilities`

---

### 任务 B：更新 visp-llm 引用

#### Step 4b：visp-llm 改用 visp_config

#### 🟢 绿 - 实现
1. visp-llm/src/openai.rs: `use visp_config::{LlmConfig, ModelInfo};` 替换 `use visp_core::provider::{LlmConfig, ModelInfo}`（保留 `use visp_core::provider::{ChatEvent, LlmProvider};`）
2. visp-llm/src/anthropic.rs: 同上
3. visp-llm/src/mock.rs: 同上
4. visp-llm/src/mock_tests.rs: `use visp_config::LlmConfig;`

#### 🧪 测试
```bash
cargo test -p visp-llm
```

#### 📦 提交
`refactor(visp-llm): use visp_config for LlmConfig and ModelInfo`

---

## Wave 5：运行时配置 API（2 路并行，依赖 Wave 4）

### 任务 A：merge_session_config + build_llm_config_from_model

#### Step 5a：session 配置合并 + 从 model 构造 LlmConfig

#### 🔴 红 - 测试
| # | 测试用例 | 描述 |
|---|---------|------|
| 1 | `test_merge_no_client_config` | 客户端不传 config，返回 daemon 默认 |
| 2 | `test_merge_with_model_key` | 客户端传 model_key，从 daemon models 解析 |
| 3 | `test_merge_model_key_not_found` | model_key 不匹配，回退默认 |
| 4 | `test_merge_client_overrides_model` | 客户端传 model，不用默认 |
| 5 | `test_merge_sentinel_temperature` | 客户端未传 temperature（== 默认值），从 model_cfg 填充 |
| 6 | `test_merge_sentinel_max_tokens` | 同上，max_tokens |
| 7 | `test_merge_extra_thinking_budget` | model_cfg 的 thinking_budget_tokens 注入 extra |
| 8 | `test_build_llm_config_from_model_full` | 完整构造含 langfuse 字段 |
| 9 | `test_build_llm_config_from_model_no_langfuse` | langfuse_cfg 为 None 时 langfuse 字段 disabled |

#### 🟢 绿 - 实现
1. `merge_session_config(client_config: Option<&ProtoLlmConfig>, daemon_config: &DaemonConfig) -> LlmConfig`
   - 保留 sentinel 比较逻辑（`== LlmConfig::default().xxx`，浮点用 `f64::EPSILON`）
2. `build_llm_config_from_model(model_cfg: &LlmModelConfig, langfuse_cfg: Option<&ObservabilityConfig>) -> LlmConfig`

#### 🧪 测试
```bash
cargo test -p visp-config
cargo clippy -p visp-config
```

#### 📦 提交
`feat(visp-config): add merge_session_config and build_llm_config_from_model`

---

### 任务 B：apply_config_update

#### Step 5b：运行时配置更新

#### 🔴 红 - 测试
| # | 测试用例 | 描述 |
|---|---------|------|
| 1 | `test_apply_config_update_model_key` | 更新 model_key，从 daemon models 解析填充 |
| 2 | `test_apply_config_update_temperature` | 更新 temperature |
| 3 | `test_apply_config_update_model_key_not_found` | model_key 不匹配，保留原 config |
| 4 | `test_apply_config_update_partial` | 只更新部分字段，其余保留原值 |

#### 🟢 绿 - 实现
`apply_config_update(current: &LlmConfig, update: &ProtoLlmConfig, daemon_config: &DaemonConfig) -> LlmConfig`
- 保留 sentinel 比较逻辑

#### 🧪 测试
```bash
cargo test -p visp-config
cargo clippy -p visp-config
```

#### 📦 提交
`feat(visp-config): add apply_config_update`

---

## Wave 6：更新调用方（5 路并行，依赖 Wave 5）

> 以下 5 个任务互不依赖，各自修改不同文件，可完全并行。

### 任务 A：visp-daemon/main.rs

#### Step 6a：更新 main.rs
#### 🟢 绿 - 实现
1. `load_config` 调用改为 `visp_config::load_config`
2. agents 目录路径改用 `visp_config::path::agents_dir_project` / `agents_dir_global`
3. `ModelInfo` 构造改为 `visp_config::model_config_to_info`
4. 移除 `VISP_LISTEN_ADDR` 环境变量覆盖逻辑（已由 `load_config` 处理）
5. `create_llm_provider` 移除 `OPENAI_API_KEY` / `ANTHROPIC_API_KEY` 环境变量回退逻辑

#### 📦 提交
`refactor(visp-daemon/main): use visp_config for config loading and paths`

---

### 任务 B：visp-daemon/service.rs

#### Step 6b：更新 service.rs
#### 🟢 绿 - 实现
1. `default_llm_config` 构造改为 `visp_config::build_llm_config_from_model`
2. `create_session` 配置合并改为 `visp_config::merge_session_config`
3. `ConfigUpdate` handler 改为 `visp_config::apply_config_update`
4. 删除 `map_llm_config`，改为 `visp_config::proto_to_llm_config`
5. `create_llm_provider` 移除 API Key 环境变量回退逻辑

#### 📦 提交
`refactor(visp-daemon/service): use visp_config for runtime config management`

---

### 任务 C：visp-daemon/observability + visp-agent/orchestrator

#### Step 6c：更新 observability + orchestrator
#### 🟢 绿 - 实现
**observability/init.rs:**
1. `ObservabilityConfig` 引用改为 `visp_config::ObservabilityConfig`
2. 移除 `RUST_LOG` 环境变量覆盖逻辑，直接使用 `cfg.level`
3. `~/` 路径展开改用 `visp_config::path::expand_home`

**visp-agent/orchestrator.rs:**
1. model 覆盖改为调用 `visp_config::apply_model_override`
2. `resolve_model_info` 改为调用 `visp_config::resolve_model`
3. `resolve_provider` 的 model_key 选择改为调用 `visp_config::resolve_model_key`，provider 实例查找保留

#### 📦 提交
`refactor(visp-daemon/observability, visp-agent/orchestrator): use visp_config`

---

### 任务 D：visp-tools + visp-command + visp-cli + visp + visp-db

#### Step 6d：更新路径引用
#### 🟢 绿 - 实现
1. visp-tools/skill.rs: `env::var("HOME")` 全局技能路径改用 `visp_config::path::skills_dir_global`
2. visp-tools/fetch.rs: `.visp/webfetch.toml` 路径改用 `visp_config::path::webfetch_toml_project`
3. visp-tools/codegraph.rs: `.visp/codegraph.db` 路径改用 `visp_config::path::codegraph_db`（5 处）
4. visp-command/init_agent.rs: `.visp/agents/` 路径改用 `visp_config::path::agents_dir_project`
5. visp-command/init_skill.rs: `.visp/skills/` 路径改用 `visp_config::path::skills_dir_project`
6. visp-cli/image.rs: `.visp/images` 临时缓存路径改用 `visp_config::path::image_cache_dir`
7. visp/main.rs: `get_log_dir()` 改用 `visp_config::path::log_dir`；启动 daemon 前调用 `visp_config::load_config()` 获取基础 listen_addr
8. visp-db/store.rs: `~/` 路径展开改用 `visp_config::path::expand_home`

#### 📦 提交
`refactor: update visp-tools, visp-command, visp-cli, visp, visp-db to use visp_config::path`

---

### 任务 E：visp-mcp re-export 清理

#### Step 6e：visp-mcp config.rs 清理
#### 🟢 绿 - 实现
1. 确认 visp-mcp/src/config.rs 的 re-export 正确
2. 移除任何残留的 McpConfig 定义

#### 📦 提交
`refactor(visp-mcp): cleanup config re-export`

---

## Wave 7：全量验收（串行，1 步，依赖 Wave 6）

### Step 7a：全量构建与测试

#### 🧪 测试
```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace
```

#### 📦 提交
`test: full workspace build, test, and clippy pass`

---

## Wave 并行策略

### Wave 1：crate 骨架 + path.rs + 依赖声明（串行，3 步）
1a -> 1b -> 1c

### Wave 2：类型迁移（2 路并行）
- 任务 A: 2a (config.rs)
- 任务 B: 2b (rules/skills/prompt)

### Wave 3：re-export（串行，1 步）
3a

### Wave 4：运行时工具函数 + visp-llm（2 路并行）
- 任务 A: 4a (proto 转换 + resolve + override)
- 任务 B: 4b (visp-llm 引用更新)

### Wave 5：运行时配置 API（2 路并行）
- 任务 A: 5a (merge + build)
- 任务 B: 5b (apply_config_update)

### Wave 6：更新调用方（5 路并行）
- 任务 A: 6a (visp-daemon/main.rs)
- 任务 B: 6b (visp-daemon/service.rs)
- 任务 C: 6c (observability + orchestrator)
- 任务 D: 6d (tools/command/cli/visp/db)
- 任务 E: 6e (visp-mcp cleanup)

### Wave 7：验收（串行，1 步）
7a

## 依赖关系总览

```
Wave 1 (骨架 + path + 依赖)
  1a ── 1b (path.rs) ── 1c (Cargo.toml)
  │
  ├── 1a: crate 骨架（一切的基础）
  ├── 1b: path.rs（所有模块依赖路径函数）
  └── 1c: Cargo.toml 依赖（re-export 编译需要）
        │
Wave 2 (类型迁移，2 路并行)
  │
  ├── 2a: config.rs（依赖 1b 的 path 函数用于 load_config）
  └── 2b: rules/skills/prompt（依赖 1b 的 path 函数用于文件加载）
        │
Wave 3 (re-export，串行)
  │
  └── 3a: 全部 re-export（依赖 Wave 2 所有类型就位）
        │
Wave 4 (工具函数 + visp-llm，2 路并行)
  │
  ├── 4a: 运行时工具函数（依赖 3a 的类型）
  └── 4b: visp-llm 引用更新（依赖 1c 的 Cargo.toml + 3a 的 re-export）
        │
Wave 5 (运行时 API，2 路并行)
  │
  ├── 5a: merge + build（依赖 4a 的 proto_to_llm_config + resolve_model）
  └── 5b: apply_config_update（依赖 4a 的 proto_to_llm_config + resolve_model）
        │
Wave 6 (调用方更新，5 路并行)
  │
  ├── 6a: daemon/main.rs（依赖 3a + 4a）
  ├── 6b: daemon/service.rs（依赖 5a + 5b + 4a）
  ├── 6c: observability + orchestrator（依赖 4a + 3a）
  ├── 6d: tools/command/cli/visp/db（依赖 1b）
  └── 6e: visp-mcp cleanup（依赖 3a）
        │
Wave 7 (验收，串行)
  │
  └── 7a: 全量 build + test + clippy
```

## 测试覆盖汇总

| Wave | 步骤 | 测试用例数 |
|------|------|-----------|
| Wave 1 | 1b (path.rs) | 19 |
| Wave 2 | 2a (config.rs) | ~60（迁移自 visp-daemon） |
| Wave 2 | 2b (rules/skills/prompt) | 迁移自 visp-core |
| Wave 4 | 4a (工具函数) | 14 |
| Wave 5 | 5a (merge + build) | 9 |
| Wave 5 | 5b (apply_config_update) | 4 |
| Wave 7 | 7a (全量) | 全量 workspace |

## 备注

- Wave 1 严格串行：骨架 -> path.rs -> Cargo.toml
- Wave 2 两路并行：config.rs 和 rules/skills/prompt 互不依赖，但都依赖 path.rs
- Wave 3 必须串行：re-export 需要所有类型就位
- Wave 4 两路并行：工具函数在 visp-config 内新增，visp-llm 引用更新在外部修改，无文件冲突
- Wave 5 两路并行：merge/build 和 apply_config_update 都只依赖 Wave 4 的工具函数，修改同一文件不同函数区域，需注意合并
- Wave 6 五路并行：全部是不同 crate 的不同文件，无冲突
- 每个步骤完成后执行 `cargo test` 和 `cargo clippy` 确保无回归
- re-export 策略确保现有测试无需修改 import 路径
- visp-llm 直接依赖 visp-config，不通过 visp-core re-export
