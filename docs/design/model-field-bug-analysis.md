# Model 字段 Bug 分析

## 错误现象

请求 API 时，model 字段发送了错误的值。
例如配置为 `model = "deepseek-v4-flash"`，但实际发送到 API 的是 `"OpenCode.DeepSeek v4 Flash"`。
API 返回：`Model Opencode.DeepSeek v4 Flash is not supported`。

---

## 概念对比

代码中三个易混淆的概念：

| 概念 | 含义 | 示例值 | 用途 |
|------|------|--------|------|
| `LlmModelConfig.name` | 显示名 | `"DeepSeek v4 Flash"` | `/model` 列表中展示给用户 |
| `LlmModelConfig.model` | API model key | `"deepseek-v4-flash"` | 发送到 API 的 `model` 字段值 |
| `LlmModelConfig.provider` | 服务商名 | `"OpenCode"` | 用于区分同名模型，默认回退到 `protocol` |
| `LlmModelConfig::key()` | lookup key | `"OpenCode.DeepSeek v4 Flash"` | 格式 `{provider}.{name}`，用于模型切换 |
| `LlmConfig.model` | 运行时 model | 应等于 `model` 字段 | 被多个位置读取，但取值来源有误 |

---

## 数据流图

```
daemon.toml
  model = "deepseek-v4-flash"       ← API 真正需要的值
  name  = "DeepSeek v4 Flash"       ← 用户看到的显示名
  provider = "OpenCode"             ← 服务商名
                    │
                    ▼
        LlmModelConfig { model, name, provider }
                    │
                    ├── key() = "OpenCode.DeepSeek v4 Flash"  ← lookup key
                    │
                    ▼
        service.rs:132  model: default_cfg.key()    ★ BUG
                    │
                    ▼
        LlmConfig.model = "OpenCode.DeepSeek v4 Flash"   ← 错误值
                    │
                    ▼
        openai.rs:20   "model": config.model
        anthropic.rs:42 "model": config.model
                    │
                    ▼
        API 收到 "OpenCode.DeepSeek v4 Flash" → 不认识 → 报错
```

---

## Bug 位置

### 1. 根因 — `service.rs:131-132`

```rust
// 错误：用 lookup key 作为 API model
let default_llm_config = LlmConfig {
    model: default_cfg.key(),   // "OpenCode.DeepSeek v4 Flash"
    ...
};
```

应为：

```rust
model: default_cfg.model.clone(),   // "deepseek-v4-flash"
```

### 2. 连锁问题 — `service.rs:480-482` (JoinSession)

```rust
model_configs.iter().find(|mc| mc.model == session.config.model)
```

`mc.model` 是 `"deepseek-v4-flash"`，而 `session.config.model` 是 `"OpenCode.DeepSeek v4 Flash"`，**永远匹配不上**。切换 session 后无法找到正确的模型配置来创建 provider。

---

## 所有涉及位置清单

| 文件 | 行号 | 代码 | 问题 |
|------|------|------|------|
| `crates/visp-daemon/src/config.rs` | 48-70 | `LlmModelConfig` 定义 | 三个字段含义清晰，无问题 |
| `crates/visp-daemon/src/config.rs` | 72-78 | `key()` 返回 `{provider}.{name}` | 设计合理 |
| **`crates/visp-daemon/src/service.rs`** | **132** | `model: default_cfg.key()` | **BUG：用 key 代替 model** |
| **`crates/visp-daemon/src/service.rs`** | **482** | `mc.model == session.config.model` | **BUG：格式不匹配，JoinSession 失效** |
| `crates/visp-daemon/src/service.rs` | 577-578 | `mc.key() == *model` | ConfigUpdate 用 key 匹配，OK |
| `crates/visp-llm/src/openai.rs` | 20 | `"model": config.model` | 被动接收 LlmConfig 的值，无问题 |
| `crates/visp-llm/src/openai.rs` | 641 | `model = %config.model` (log) | 同上 |
| `crates/visp-llm/src/anthropic.rs` | 42 | `"model": config.model` | 同上 |
| `crates/visp-llm/src/anthropic.rs` | 455 | `model = %config.model` (log) | 同上 |
| `crates/visp-core/src/provider.rs` | 13 | `pub model: String` | 字段 doc 写"模型名称"，语义模糊 |

---

## 修复方案

两处修改：

1. **`service.rs:132`**: `key()` → `model.clone()`
2. **`service.rs:482`**: `mc.model` → `mc.key()`，对齐两边格式

这样 `LlmConfig.model` 全程存储 API model key，lookup 用 `key()` 方法隔离。
