# 日志 level 与 OTLP level 分离设计

> 日期：2026-08-15
> 状态：待用户审核
> 范围：visp-daemon（init.rs、main.rs）、visp-config（config.rs）

## 1. 需求概述

修复 `log_level` 与 `observability.level` 混淆：

1. **`daemon.log_level`** 只管**写入日志（stdout/文件）的 level**。
2. **`observability.level`** 只管 **OTLP/tracing 导出的 level**。

**已确认的决策**（用户选定）：

| 决策点 | 方案 |
|---|---|
| RUST_LOG 覆盖对象 | RUST_LOG → `daemon.log_level`（日志过滤传统语义） |
| MetricsLayer 归属 | 随 `observability.level`（与导出同 level） |

## 2. 根因

| 问题 | 现状 |
|---|---|
| `daemon.log_level` 从未被读取 | 仅 config.rs 定义 + dead_code 标注，生产代码零引用——用户设它无效 |
| `observability.level` 是全局唯一 filter | `init.rs` 用 `EnvFilter::new(&cfg.level)` 挂 registry 最外层，**同时控制日志写入和 OTLP 导出** |
| tracing 机制限制 | registry 级 EnvFilter 无法区分"哪个层用什么 level" |

## 3. 架构概览（改造后）

```
配置：
  daemon.log_level        = "info"   → 控制台/文件日志
  observability.level     = "info"   → OTLP 导出 + Metrics
  RUST_LOG (env)                    → 覆盖 daemon.log_level

init_observability(log_level, otel_level):
  registry()
    .with(fmt::layer()...    .with_filter(LevelFilter::from_str(log_level)))
    .with(otel_layer         .with_filter(LevelFilter::from_str(otel_level)))
    .with(parent_link)
    .with(metrics            .with_filter(LevelFilter::from_str(otel_level)))
    .try_init()
```

## 4. 模块职责

| 模块 | 改动 |
|---|---|
| visp-config / config.rs | `load_config` 的 RUST_LOG 处理改为覆盖 `daemon.log_level`；`DaemonSection.log_level` 移除 `#[allow(dead_code)]` |
| visp-daemon / init.rs | 4 个初始化函数签名加 `log_level` 参数；去掉全局 EnvFilter，改 per-layer `LevelFilter`；去掉 `try_from_default_env()` |
| visp-daemon / main.rs | `init_observability(&config.observability, &config.daemon.log_level)`（或传 DaemonConfig） |

## 5. 核心实现

### 5.1 init_observability 签名

```rust
// 从 init_observability(&ObservabilityConfig) 改为
pub fn init_observability(obs: &ObservabilityConfig, log_level: &str)
```

内部（4 个函数同一模式）：
```rust
let log_filter = LevelFilter::from_str(log_level).unwrap_or(LevelFilter::INFO);
let otel_filter = LevelFilter::from_str(&obs.level).unwrap_or(LevelFilter::INFO);

tracing_subscriber::registry()
    .with(fmt::layer().json().with_writer(writer).with_filter(log_filter))
    .with(otel_layer.with_filter(otel_filter))
    .with(parent_link.clone())
    .with(metrics.clone().with_filter(otel_filter))
    .try_init()
```

- 用 `tracing_subscriber::filter::LevelFilter`（简单，两个 level 都是纯字符串级别）
- fmt 层 → `log_filter`（daemon.log_level）
- otel 层 → `otel_filter`（observability.level）
- metrics 层 → `otel_filter`（随 otel，用户确认）
- parent_link 层 → 不过滤（事件透传辅助层）

### 5.2 RUST_LOG 覆盖（config.rs）

```rust
// RUST_LOG -> daemon.log_level（日志过滤语义）
if let Ok(level) = std::env::var("RUST_LOG") {
    config.daemon.log_level = level;
}
```

（原来覆盖 `observability.level`）

### 5.3 去掉 try_from_default_env

`init.rs` 的 `EnvFilter::try_from_default_env()` 删除——RUST_LOG 已由 load_config 写入 `daemon.log_level`，init 直接用传入的 log_level 即可（避免重复读环境变量）。

## 6. 边界情况

| 场景 | 处理 |
|---|---|
| log_level 非法值 | `LevelFilter::from_str` 失败 → 兜底 INFO |
| observability.level 非法值 | 同样兜底 INFO |
| RUST_LOG 未设置 | 用 daemon.toml 的 log_level |
| observability.enabled = false | 日志照常（log_level 生效），otel 层不构建 |
| 既有 RUST_LOG 用户 | 语义从"控制 otel"变为"控制日志"——行为变更但符合用户要求 |

## 7. 影响范围

| 文件 | 改动 |
|---|---|
| `crates/visp-config/src/config.rs` | RUST_LOG → log_level；移除 dead_code 标注；测试更新 |
| `crates/visp-daemon/src/observability/init.rs` | 签名 + per-layer filter + 去 try_from_default_env |
| `crates/visp-daemon/src/main.rs` | 传 log_level |
| 测试 | config RUST_LOG 测试更新；init.rs 分离 level 测试 |

## 8. 测试策略（TDD 重点）

1. **config**：RUST_LOG=debug → `daemon.log_level == "debug"`（更新现有 test_env_override_rust_log）。
2. **init 分离**：设 `log_level="error"`、`obs.level="debug"` → 文件 writer 只收 error 事件、otel 层收 debug 事件；反向亦然。
3. **非法 level 兜底**：log_level="garbage" → 兜底 INFO 不 panic。

## 9. 验证标准

- `cargo test`（config + init 相关全过）+ `cargo clippy -D warnings` + `cargo fmt --check`。
- 手动验证：设 `log_level="error"` + `observability.level="debug"`，观察控制台只打 error、而 OTLP 收到 debug span。
