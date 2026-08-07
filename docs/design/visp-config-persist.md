# visp-config 配置持久化设计

## 1. 背景与动机

### 1.1 当前问题

| 问题 | 现状 | 影响 |
|------|------|------|
| 配置只读 | `load_config()` 只做读取和合并，无写回能力 | 运行时通过 `/model`、`/temp` 等命令修改的配置，重启后丢失 |
| 无 Serialize | `DaemonConfig` 及大部分子结构体只 derive `Deserialize` | 无法将配置序列化回 TOML |
| 设计文档已标记 | 原设计文档标注"持久化暂不实现，后续迭代" | 现在是该迭代的时机 |

### 1.2 目标

为 visp-config 加入**项目级配置写入**能力：将 `DaemonConfig` 持久化到 `{project}/.visp/daemon.toml`。

本次范围：
- 仅写入项目级配置（`.visp/daemon.toml`），不写全局配置
- 提供底层 `save_config()` API，暂不接入 daemon 运行时流程

### 1.3 非目标

- 不写全局配置（`~/.config/visp/daemon.toml`）
- 不接入 daemon 运行时（`/model`、`/temp` 命令的自动持久化）
- **不自动写入**：`save_config()` 只能由用户主动触发，任何运行时配置变更（如 `/model`、`/temp`）不得自动调用 `save_config()`
- 不改变 `load_config()` 的合并逻辑
- 不实现增量/diff 写入（本轮写全量配置）
- 不处理并发写冲突（单线程写场景）

---

## 2. 架构设计

### 2.1 模块定位

配置持久化逻辑放在 `crates/visp-config/src/config.rs` 中，与 `load_config()` 对称：

```
config.rs
├── load_config()       ← 已有：读取 + 合并
└── save_config()       ← 新增：序列化 + 写入
```

### 2.2 数据流

```
调用方
  │
  ├─ 1. toml::to_string_pretty(&config) -> TOML 字符串
  │
  ├─ 2. 原子写入 {project}/.visp/daemon.toml
  │     - 确认 .visp/ 目录存在（create_dir_all）
  │     - 写入临时文件 .visp/.daemon.toml.visp-tmp
  │     - rename 到 daemon.toml
  │
  └─ 返回 Result<(), String>
```

### 2.3 Serialize 派生

需要为以下结构体添加 `Serialize` derive（当前仅有 `Deserialize`）：

| 结构体 | 当前 derive | 目标 derive |
|--------|------------|------------|
| `DaemonConfig` | Deserialize | Serialize, Deserialize |
| `DaemonSection` | Deserialize | Serialize, Deserialize |
| `LlmSection` | Deserialize | Serialize, Deserialize |
| `LlmModelConfig` | Deserialize | Serialize, Deserialize |
| `McpConfig` | Deserialize | Serialize, Deserialize |
| `McpServerConfig` | Deserialize | Serialize, Deserialize |
| `McpTransport` | Deserialize | Serialize, Deserialize |
| `ToolsSection` | Deserialize | Serialize, Deserialize |
| `AgentSection` | Deserialize | Serialize, Deserialize |
| `BuiltinAgentConfig` | Deserialize | Serialize, Deserialize |
| `StorageSection` | Deserialize, Default | Serialize, Deserialize, Default |

已有 `Serialize` 的结构体无需改动：`LlmConfig`、`ObservabilityConfig`、`LangfuseConfig`、`OtlpConfig`。

### 2.4 写入策略

**全量写入**：将传入的 `DaemonConfig` 完整序列化写入，不做脱敏。

理由：
- `.visp/` 已被 `.gitignore` 忽略，不存在泄露到版本控制的风险
- `load_config()` 读取时包含 `api_key`，写入时也必须保留，否则 roundtrip 后 `api_key` 丢失，模型无法调用
- MCP env 中的密钥同理，需要保留以保持配置可用性

这意味着如果传入的是 `load_config()` 合并后的配置，全局配置会被"吸收"到项目级文件中。后续迭代可考虑增量写入（只写 diff），但本轮不实现。

### 2.5 原子写入

复用项目中已有的原子写模式（参考 `visp-tools/src/file.rs:507`）：

1. `create_dir_all` 确保 `.visp/` 存在
2. 写入临时文件 `{target}.visp-tmp`
3. `rename` 临时文件到目标路径

这保证了写入过程中即使崩溃，也不会产生损坏的配置文件。

---

## 3. API 设计

### 3.1 公开函数

| 函数 | 职责 |
|------|------|
| `save_config(config: &DaemonConfig, project: &Path) -> Result<(), String>` | 将配置原子写入 `{project}/.visp/daemon.toml` |

### 3.2 lib.rs 导出

在 `lib.rs` 的 `pub use config::` 中新增 `save_config`。

### 3.3 错误处理

- 目录创建失败 -> `Err("Failed to create .visp directory: {e}")`
- 序列化失败 -> `Err("Failed to serialize config: {e}")`
- 临时文件写入失败 -> `Err("Failed to write temp file: {e}")`
- rename 失败 -> `Err("Failed to rename temp file: {e}")`

---

## 4. 测试策略

| # | 测试用例 | 描述 |
|---|---------|------|
| 1 | `test_save_config_creates_file` | 写入后 `.visp/daemon.toml` 存在且内容正确 |
| 2 | `test_save_config_creates_dir` | `.visp/` 不存在时自动创建 |
| 3 | `test_save_config_overwrites` | 已有文件时覆盖写入 |
| 4 | `test_save_config_preserves_api_key` | api_key 正确保留在写入的文件中 |
| 5 | `test_save_config_roundtrip` | save -> load 往返：写入后 load_config 能正确读取 |
| 6 | `test_save_config_atomic` | 不残留 `.visp-tmp` 临时文件 |
| 7 | `test_save_config_toml_format` | 写入的 TOML 格式正确（可被 toml::from_str 解析） |

测试使用 `tempfile::TempDir` 隔离文件系统副作用。

---

## 5. 影响范围

| 模块 | 改动 |
|------|------|
| `crates/visp-config/src/config.rs` | 添加 Serialize derive、`save_config()`、测试 |
| `crates/visp-config/src/lib.rs` | 导出新函数 |
| `crates/visp-config/Cargo.toml` | 无需改动（toml、serde 已有依赖） |

不改动其他 crate。后续接入 daemon 运行时时再改 `visp-daemon`。

---

## 6. 待讨论问题

### 问题 1：全量写入 vs 增量写入（已确认）

**决策**：本轮先全量写入，后续迭代再加增量。

**现状**：全量序列化传入的 `DaemonConfig`。如果传入 merged config，全局配置会被"吸收"到项目文件中。下次加载时，即使删除全局配置，项目配置仍包含完整内容。

**后续方向**：增量写入可考虑只持久化与全局配置的 diff，或让调用方显式传入仅含项目级字段的 `DaemonConfig`。

### 问题 2：MCP env 密钥脱敏（已否决）

**决策**：不做脱敏。

**理由**：`.visp/` 已被 `.gitignore` 忽略；`load_config()` 读取时包含密钥，写入时也必须保留，否则 roundtrip 后密钥丢失导致配置不可用。

### 问题 3：默认值序列化导致 TOML 冗长（已确认）

**决策**：本轮不做优化，全量写出默认值字段。后续可考虑用 `#[serde(skip_serializing_if)]` 省略默认值。

**现状**：添加 `Serialize` 后，带 `#[serde(default = "...")]` 的字段即使值等于默认值也会出现在 TOML 中。写入的文件较冗长，但对功能无影响--`load_config()` 读取时 `default` 属性仍然生效。

### 问题 4：触发入口（已确认）

**决策**：本轮只提供 `save_config()` API，触发入口留后续迭代。

**约束**：`save_config()` 只能由用户主动触发。任何运行时配置变更（如 `/model`、`/temp`）不得自动调用 `save_config()`。后续迭代可考虑新增 `/save-config` slash command 等用户主动触发入口。
