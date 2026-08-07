# visp-config 配置持久化实施计划

## 概述

为 visp-config 加入项目级配置写入能力。将 `DaemonConfig` 全量序列化为 TOML，原子写入 `{project}/.visp/daemon.toml`。

设计文档：`docs/design/visp-config-persist.md`

改动范围：`crates/visp-config/src/config.rs`、`crates/visp-config/src/lib.rs`

---

## 步骤 1：添加 Serialize derive

### 1a：为 11 个结构体添加 Serialize

#### 🟢 绿 - 实现

为以下结构体的 derive 宏添加 `Serialize`（`use serde::{Deserialize, Serialize}` 已存在，无需改动 import）：

| 结构体 | 行号（约） | 当前 derive | 目标 derive |
|--------|-----------|------------|------------|
| `McpConfig` | :7 | `Debug, Clone, Deserialize, Default` | `Debug, Clone, Serialize, Deserialize, Default` |
| `McpServerConfig` | :14 | `Debug, Clone, Deserialize` | `Debug, Clone, Serialize, Deserialize` |
| `McpTransport` | :40 | `Debug, Clone, Deserialize` | `Debug, Clone, Serialize, Deserialize` |
| `DaemonConfig` | :108 | `Debug, Clone, Deserialize` | `Debug, Clone, Serialize, Deserialize` |
| `DaemonSection` | :145 | `Debug, Clone, Deserialize` | `Debug, Clone, Serialize, Deserialize` |
| `LlmSection` | :154 | `Debug, Clone, Deserialize, Default` | `Debug, Clone, Serialize, Deserialize, Default` |
| `LlmModelConfig` | :177 | `Debug, Clone, Deserialize` | `Debug, Clone, Serialize, Deserialize` |
| `ToolsSection` | :485 | `Debug, Clone, Deserialize` | `Debug, Clone, Serialize, Deserialize` |
| `AgentSection` | :495 | `Debug, Clone, Deserialize` | `Debug, Clone, Serialize, Deserialize` |
| `BuiltinAgentConfig` | :525 | `Debug, Clone, Deserialize` | `Debug, Clone, Serialize, Deserialize` |
| `StorageSection` | :540 | `Debug, Clone, Deserialize, Default` | `Debug, Clone, Serialize, Deserialize, Default` |

注意：`LlmConfig`、`ObservabilityConfig`、`LangfuseConfig`、`OtlpConfig`、`LangfuseCaptureConfig` 已有 `Serialize`，不改。`ModelInfo` 是运行时类型，不改。

#### 🧪 验证
```bash
cargo check -p visp-config
```

#### 📦 提交
`feat(visp-config): add Serialize derive to config structs`

---

## 步骤 2：实现 save_config()

### 2a：编写 save_config 测试

#### 🔴 红 - 测试

在 `crates/visp-config/src/config.rs` 的 `#[cfg(test)]` mod 中添加测试：

| # | 测试用例 | 描述 |
|---|---------|------|
| 1 | `test_save_config_creates_file` | 写入后 `.visp/daemon.toml` 存在，内容包含预期字段 |
| 2 | `test_save_config_creates_dir` | `.visp/` 不存在时自动创建 |
| 3 | `test_save_config_overwrites` | 已有旧文件时覆盖写入 |
| 4 | `test_save_config_preserves_api_key` | api_key 正确保留在写入的文件中 |
| 5 | `test_save_config_roundtrip` | save 后用 `load_from_file` 读取，验证字段一致 |
| 6 | `test_save_config_no_temp_residue` | 写入完成后不残留 `.visp-tmp` 临时文件 |
| 7 | `test_save_config_toml_parseable` | 写入的文件可被 `toml::from_str::<DaemonConfig>` 解析 |

测试使用 `tempfile::TempDir`（已是 dev-dependency）。

#### 📦 提交
`test(visp-config): add save_config test cases`

---

### 2b：实现 save_config()

#### 🟢 绿 - 实现

在 `crates/visp-config/src/config.rs` 中实现 `pub fn save_config(config: &DaemonConfig, project: &Path) -> Result<(), String>`：

1. `path::daemon_toml_project(project)` 获取目标路径
2. `create_dir_all` 确保 `.visp/` 存在
3. `toml::to_string_pretty(config)` 序列化
4. 写入临时文件 `{target}.visp-tmp`
5. `rename` 到目标路径
6. 错误处理：每步失败返回 `Err("Failed to ...: {e}")`

#### 🧪 验证
```bash
cargo test -p visp-config -- save_config
```

#### 📦 提交
`feat(visp-config): implement save_config for project-level persistence`

---

## 步骤 3：导出 save_config

### 3a：在 lib.rs 中导出

#### 🟢 绿 - 实现

在 `crates/visp-config/src/lib.rs` 的 `pub use config::{ ... }` 中添加 `save_config`。

#### 🧪 验证
```bash
cargo check -p visp-config
```

#### 📦 提交
`feat(visp-config): export save_config from lib`

---

## Wave 并行策略

### Wave 1（串行，3 个步骤）

任务 A: 1a -> 2a -> 2b -> 3a

全部在 `crates/visp-config` 内，改动集中在 config.rs 和 lib.rs 两个文件，无并行空间，串行执行。

```
1a (Serialize derive)
  ↓
2a (写测试) → 2b (实现 save_config)
  ↓
3a (导出)
```

## 依赖关系总览

```
1a: Serialize derive ──────┐
                           ↓
2a: 测试用例 ──→ 2b: save_config 实现
                           ↓
                    3a: lib.rs 导出
```

- 1a 是基础：没有 Serialize，save_config 无法序列化
- 2a → 2b：TDD 先写测试再实现
- 3a 依赖 2b：需要函数已定义才能导出

## 测试覆盖汇总

| Wave | 并行数 | 模块/包 | 步骤 | 测试用例 |
|------|--------|---------|------|---------|
| 1 | 1（串行） | visp-config | 1a | - (编译验证) |
| 1 | 1（串行） | visp-config | 2a-2b | 7 个测试用例 |
| 1 | 1（串行） | visp-config | 3a | - (编译验证) |

## 备注

- `tempfile` 和 `toml` 已在 `crates/visp-config/Cargo.toml` 的 dev-dependencies / dependencies 中，无需修改 Cargo.toml
- 不改动 `load_config()` 和 `load_from_file()` 的逻辑
- 不接入 daemon 运行时，无其他 crate 改动
