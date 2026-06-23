# tracing / subscriber / appender API 调研笔记

> **生成时间**: 2026-06-23  
> **用途**: visp-plan-self-observability 的 Wave 1 Step 3 / Step 4 / Step 5-sub 实现参考  
> **来源**: context7 (docs.rs), crates.io API, tokio-rs/tracing GitHub README

---

## 最新稳定版本

| Crate | 最新版本 | 来源 |
|-------|---------|------|
| `tracing` | **0.1.44** | [crates.io](https://crates.io/crates/tracing) |
| `tracing-subscriber` | **0.3.23** | [crates.io](https://crates.io/crates/tracing-subscriber) |
| `tracing-appender` | **0.2.5** | [crates.io](https://crates.io/crates/tracing-appender) |
| `tracing-test` | **0.2.6** | [crates.io](https://crates.io/crates/tracing-test) |

> ✅ **确认**：所有 API 均为 0.1.x / 0.3.x / 0.2.x 主版本，与工作计划假设一致，无 breaking change。

---

## 1. tracing 核心宏与 Span 操作（Step 3 用）

### 1.1 `info_span!` / `error_span!` 宏 —— 创建带 fields 的 Span

来源：`tracing` 0.1.x [docs](https://docs.rs/tracing/latest/tracing/macro.info_span.html)

```rust
use tracing::{info_span, error_span};

// 基本用法
let span = info_span!("agent_loop");
let span = error_span!("llm_error");

// 带 fields（visp 典型写法）
let span = info_span!(
    "llm_request",
    gen_ai.model = "claude-sonnet-4-20250514",
    visp.session.id = "sess_abc123",
    prompt_tokens = 1500,
);

// fields 可以用变量简写（类似 struct 初始化）
let model = "claude-sonnet-4-20250514";
let span = info_span!("llm_request", gen_ai.model = model);
// 等价于: gen_ai.model = model

// 预先声明空的 field，稍后动态填充
use tracing::field;
let span = info_span!("tool_call", tool_name = field::Empty);
// 后续: span.record("tool_name", "bash");
```

### 1.2 进入 Span：`.enter()` / `.entered()` / `.in_scope()` 三者对比

来源：`tracing` 0.1.x [docs](https://docs.rs/tracing/latest/tracing/span/index.html)

| 方法 | 签名 | 行为 | 适用场景 |
|------|------|------|----------|
| `.enter()` | `&Span -> Entered<'_>` | 借用一个已有 span 并返回 guard。guard drop 时退出。span 保留所有权。 | **同步代码**。异步代码中 guard 不可跨 await，否则 trace 错乱。 |
| `.entered()` | `Span -> EnteredSpan` | **消耗** span 并返回 guard。可以 `.exit()` 取回 span。 | **同步代码**中创建即进入。一条语句搞定创建+进入。同样不可跨 await。 |
| `.in_scope(closure)` | `&Span, impl FnOnce -> R` | 在闭包执行期间进入 span，闭包结束后自动退出。 | **同步代码**的临时作用域。干净，无需管理 guard。 |

```rust
use tracing::{info_span, span, Level};

// === enter() —— 借用 span，返回 guard ===
let span = info_span!("my_fn");
let _guard = span.enter();   // 进入 span
// ... 同步代码 ...
// drop(_guard) → 退出 span

// === entered() —— 消耗 span，一条语句创建+进入 ===
let span = span!(Level::INFO, "doing_something").entered();
// ... 同步代码 ...
let span = span.exit();       // 显式退出，拿回 span
let span = span.entered();    // 再次进入

// === in_scope() —— 闭包作用域 ===
let span = info_span!("my_fn");
span.in_scope(|| {
    // 同步代码在此 span 内执行
});
// 闭包结束，span 自动退出

// === 异步代码中的正确用法 ===
// ❌ 错误：guard 不能跨 await
// let _guard = span.enter();
// some_async_fn().await;   // BUG!

// ✅ 正确：用 #[instrument] 或 .instrument()
```

### 1.3 `#[tracing::instrument]` 属性宏 —— 异步函数挂 span

来源：`tracing` 0.1.x [docs](https://docs.rs/tracing/latest/tracing/attr.instrument.html)

```rust
use tracing::instrument;

// 基本：函数名 = span 名，INFO 级别，所有参数自动作为 fields
#[instrument]
async fn handle_message(session_id: String, content: String) {
    // span name = "handle_message"
    // fields: session_id, content
}

// 配置参数
#[instrument(
    name = "agent_loop_turn",
    level = "debug",
    skip(non_debug_arg),
    fields(
        visp.session.id = %session_id,
        turn_number = turn,
    )
)]
async fn run_agent_turn(
    session_id: String,
    turn: u64,
    non_debug_arg: NonDebugType,
) {
    // span name = "agent_loop_turn"
    // level = DEBUG（也可以写 Level::DEBUG）
    // skip(non_debug_arg) → 不记录该参数
    // fields 中的表达式在进入函数时求值
}
```

**关键参数说明**（来自 docs.rs）：
- `name = "..."` — 覆盖 span 名（默认是函数名）
- `level = "trace|debug|info|warn|error"` — 覆盖 span 级别
- `skip(arg1, arg2)` — 不记录某些参数
- `skip_all` — 不记录任何参数
- `fields(key = value, ...)` — 额外的 fields，表达式求值

### 1.4 `tokio::spawn` 任务挂父 span

来源：`tracing` 0.1.x [Instrument trait](https://docs.rs/tracing/latest/tracing/trait.Instrument.html)

```rust
use tracing::Instrument; // 必须 use 这个 trait

let parent = info_span!("agent_loop");

// === 方法1：.instrument(span) —— 将新 span 附加到 future ===
tokio::spawn(
    async move {
        tracing::info!("inside sub-task");
    }
    .instrument(info_span!("sub_task"))  // ← 关键
);

// === 方法2：.in_current_span() —— 将当前 span 附加到 future ===
let span = info_span!("outer");
let _enter = span.enter();

tokio::spawn(
    async move {
        tracing::debug!("inside outer span");
    }
    .in_current_span()  // ← 继承当前 span
);

// === 方法3：Span::or_current() —— fallback 策略 ===
// 若 "my_future" span 被禁用，则回退使用当前 span
tokio::spawn(
    my_future.instrument(
        tracing::debug_span!("my_future").or_current()
    )
);
```

**`.instrument()` vs `.in_current_span()` 区别**：

| 方法 | Span 来源 | 父链 | 适用场景 |
|------|----------|------|----------|
| `.instrument(span)` | 显式传入的 span | span 的父是当前 span | 给 task 创建**新子 span** |
| `.in_current_span()` | 取 `Span::current()` | 直接用当前 span | task 内事件**挂在当前 span 下** |

### 1.5 Span 内动态添加 field —— `Span::current().record()`

来源：`tracing` 0.1.x [record docs](https://docs.rs/tracing/latest/tracing/span/struct.Span.html#method.record)

**⚠️ 关键约束**：`record()` 只能修改已在 span 创建时声明的 field。未声明的字段会被**静默丢弃**，不报错。

```rust
use tracing::{info_span, field, Span};

// === 在异步函数内获取当前 span 并 record ===

// ✅ 正确：field 提前声明为 field::Empty
#[instrument(fields(result = field::Empty))]
async fn do_work() {
    // ...
    Span::current().record("result", "success");
    // ✅ 生效
}

// ❌ 错误：field 未提前声明
#[instrument]
async fn do_work() {
    Span::current().record("result", "success");
    // ❌ 静默丢弃！没有任何效果、没有警告
}

// === 同步代码中 record ===
let span = info_span!("tool_exec", duration_ms = field::Empty);
let _guard = span.enter();
// ...
span.record("duration_ms", 42_i64);
```

**核心规则**：span 创建时 `key = val` 或 `key = field::Empty` 都是「声明」。只有 `record()` 之前未声明过的 key 才会丢失。

### 1.6 Event：`tracing::info!(field = value, "message")`

来源：`tracing` 0.1.x [event docs](https://docs.rs/tracing/latest/tracing/macro.info.html)

```rust
use tracing::{info, warn, error, debug};

// 结构化日志 —— visp 用这个发 metrics.session.summary
info!(
    session.id = %session_id,
    total_tokens = 4200,
    duration_ms = 1500,
    "session completed"
);

// 简写（变量名 = field 名）
let model = "claude-sonnet";
let temperature = 0.7;
info!(model, temperature, "sending LLM request");

// 组合使用
info!(
    gen_ai.model = "claude-sonnet",
    gen_ai.temperature = 0.7,
    "llm request sent",
);
```

---

## 2. tracing-subscriber Layer 体系（Step 4 + Step 5-sub 用）

### 2.1 多 Layer 装配标准模板

来源：`tracing-subscriber` 0.3.x [registry docs](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/registry/index.html)

```rust
use tracing_subscriber::{prelude::*, EnvFilter, fmt, Registry};

// 标准多 layer 装配
tracing_subscriber::registry()
    .with(EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"))
    )
    .with(fmt::layer()
        .with_target(false)   // 可选：隐藏 target
        .compact()            // 可选：紧凑格式
    )
    .with(MyCustomLayer::new())   // 自定义 layer
    .init();   // set_global_default 并用 expect 处理
```

**⚠️ Layer 顺序约束**（来自文档和社区实践）：
- `EnvFilter` 应该**最先**添加，这样后续 layer 的 `enabled()` 判断会基于它
- `fmt::layer()` 通常放在末尾（格式化输出）
- 自定义 layer 放中间

### 2.2 Layer 完整生命周期方法

来源：`tracing-subscriber` 0.3.x [Layer trait](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/layer/trait.Layer.html)

```rust
use tracing_subscriber::{layer::Layer, registry::LookupSpan};
use tracing::{span::{Attributes, Id, Record}, Event, Subscriber};
use tracing_subscriber::layer::Context;

struct MyLayer;

impl<S> Layer<S> for MyLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    // ——— 以下为可选方法，默认实现均为空 ———

    // span 创建时调用（可读初始 fields）
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {}

    // 任何时候 span fields 被记录/更新时调用
    fn on_record(&self, _id: &Id, _values: &Record<'_>, _ctx: Context<'_, S>) {}

    // span 进入时调用
    fn on_enter(&self, _id: &Id, _ctx: Context<'_, S>) {}

    // span 退出时调用
    fn on_exit(&self, _id: &Id, _ctx: Context<'_, S>) {}

    // span 关闭（drop）时调用
    fn on_close(&self, _id: Id, _ctx: Context<'_, S>) {}

    // span ID 变更时（如 clone/follows_from）
    fn on_id_change(&self, _old: &Id, _new: &Id, _ctx: Context<'_, S>) {}

    // span 之间 follows-from 关系建立时
    fn on_follows_from(&self, _span: &Id, _follows: &Id, _ctx: Context<'_, S>) {}

    // 事件（info!/error! 等）发生时调用
    fn on_event(&self, _event: &Event<'_>, _ctx: Context<'_, S>) {}

    // 事件过滤（性能优化 hook，返回 false 则 on_event 不调用）
    fn event_enabled(&self, _event: &Event<'_>, _ctx: Context<'_, S>) -> bool { true }

    // 注册 dispatcher 时调用
    fn on_register_dispatch(&self, _dispatch: &tracing::Dispatch) {}

    // Layer 安装到 subscriber 时调用
    fn on_layer(&mut self, _subscriber: &mut S) {}
}
```

**各方法触发时机**：

| 方法 | 触发时机 | 适用场景 |
|------|---------|----------|
| `on_new_span` | `info_span!()` 宏展开创建 span 时 | 读取初始 fields，存储 span 元数据 |
| `on_record` | `span.record("key", val)` 调用时 | 捕获动态更新的 field 值 |
| `on_enter` | span 进入时 | 计数器、活跃 span 跟踪 |
| `on_exit` | span 退出时 | 同上 |
| `on_close` | span drop 时（**最后退出后**） | 汇总数据、写入指标 |
| `on_event` | `info!()` / `error!()` 等宏展开时 | 收集事件 |
| `on_id_change` | span clone 或 follows_from | 分布式 tracing |

### 2.3 在 Layer 中读取 span fields —— Visitor 模式

来源：`tracing-subscriber` 0.3.x [Visit trait](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/field/trait.Visit.html)

```rust
use std::collections::BTreeMap;
use tracing::field::{Field, Visit};
use std::fmt;

/// 最小化的 field 收集器
/// 注意：record_debug 是唯一必须实现的方法
struct FieldCollector {
    fields: BTreeMap<String, String>,
}

impl Visit for FieldCollector {
    /// ⚠️ 这是唯一必须实现的方法
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.fields
            .insert(field.name().to_string(), format!("{:?}", value));
    }

    /// 覆盖 record_str 以获得更干净的输出（非必须，但推荐）
    fn record_str(&mut self, field: &Field, value: &str) {
        self.fields
            .insert(field.name().to_string(), value.to_string());
    }

    /// 覆盖 record_i64 以保留数值精度（非必须，但推荐）
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.fields
            .insert(field.name().to_string(), value.to_string());
    }
}

// 在 on_new_span 中使用
fn on_new_span(&self, attrs: &tracing::span::Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
    let mut collector = FieldCollector { fields: BTreeMap::new() };
    attrs.record(&mut collector);

    // collector.fields 现在包含 span 创建时的所有 fields

    // 检查 span name
    let metadata = attrs.metadata();
    println!("span: {}, fields: {:?}", metadata.name(), collector.fields);
}
```

**关键点**：
- `record_debug` 是唯一的 **required method**，所有其他方法都有默认实现（fallback 到 `record_debug`）
- 推荐覆盖 `record_str`、`record_i64`、`record_bool` 以获得更精确的值
- `field.name()` 返回字段名（不含前缀 `my_field`，带前缀 `%my_field` 也会被 `.name()` 去掉）

### 2.4 Layer 中按 span name 过滤

```rust
fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
    let name = attrs.metadata().name();

    match name {
        "agent_loop" => { /* 只处理 agent_loop span */ }
        "llm_request" => { /* 只处理 llm_request span */ }
        _ => { /* 其他忽略 */ }
    }
}
```

`attrs.metadata()` 还提供：
- `.level()` — span 的 Level
- `.target()` — 模块路径
- `.module_path()` / `.file()` / `.line()` — 代码位置

### 2.5 Layer 中读父 span 和当前 span

```rust
fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
    // 获取当前 span（即本 span 的父）
    let parent = ctx.lookup_current();
    // 或者通过 ctx.span(id) 获取本 span 的父 ID
    if let Some(span_ref) = ctx.span(id) {
        let parent_id = span_ref.parent();   // Option<&Id>
    }
}

fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
    // 当前事件所在的 span
    if let Some(span_ref) = ctx.lookup_current() {
        let span_name = span_ref.metadata().name();
        // ...
    }
}
```

### 2.6 Layer 中往 span 存自定义结构 —— Extensions

来源：`tracing-subscriber` 0.3.x [extensions API](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/registry/struct.Data.html)

```rust
// 自定义数据结构
#[derive(Debug, Clone)]
struct SpanMetrics {
    start: std::time::Instant,
    event_count: u64,
}

// 在 on_new_span 中存储
fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
    if let Some(span_ref) = ctx.span(id) {
        span_ref.extensions_mut().insert(SpanMetrics {
            start: std::time::Instant::now(),
            event_count: 0,
        });
    }
}

// 在 on_event 中读取
fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
    if let Some(span_ref) = ctx.lookup_current() {
        if let Some(metrics) = span_ref.extensions().get::<SpanMetrics>() {
            println!("span has been active for {:?}", metrics.start.elapsed());
        }
    }
}

// 在 on_close 中读取并清理
fn on_close(&self, id: Id, ctx: Context<'_, S>) {
    if let Some(span_ref) = ctx.span(&id) {
        if let Some(metrics) = span_ref.extensions().get::<SpanMetrics>() {
            eprintln!("span closed, duration: {:?}", metrics.start.elapsed());
        }
    }
}
```

**⚠️ 注意**：`extensions_mut().insert(T)` 会替换同类型的已有值（按 TypeId 索引）。

### 2.7 EnvFilter 默认级别

```rust
use tracing_subscriber::EnvFilter;

// 从环境变量 RUST_LOG 读取，失败时 fallback 到 info
let filter = EnvFilter::try_from_default_env()
    .unwrap_or_else(|_| EnvFilter::new("info"));

// 额外添加指令（如只对特定 crate 开 debug）
let filter = EnvFilter::try_from_default_env()
    .unwrap_or_else(|_| EnvFilter::new("info"))
    .add_directive("visp_core=debug".parse().unwrap())
    .add_directive("tower_http=warn".parse().unwrap());
```

---

## 3. tracing-appender 文件输出 + non-blocking（Step 5-sub 用）

### 3.1 滚动文件输出

来源：`tracing-appender` 0.2.x [README](https://github.com/tokio-rs/tracing/blob/main/tracing-appender/README.md)

```rust
use tracing_appender::rolling;

// daily: 每天 00:00 UTC 滚动，文件名如 visp.log.2026-06-23
let file_appender = rolling::daily("/var/log/visp", "visp.log");

// hourly: 每小时滚动，文件名如 visp.log.2026-06-23-14
let file_appender = rolling::hourly("/var/log/visp", "visp.log");

// minutely: 每分钟滚动
let file_appender = rolling::minutely("/var/log/visp", "visp.log");

// never: 不滚动，所有日志写同一个文件
let file_appender = rolling::never("/var/log/visp", "visp.log");
```

### 3.2 Non-blocking 模式（生产环境必须）

```rust
use tracing_appender::{rolling, non_blocking};

// 创建 non-blocking writer
let file_appender = rolling::daily("/var/log/visp", "visp.log");
let (non_blocking, _guard) = non_blocking(file_appender);
//      ↑ writer      ↑ WorkerGuard —— 必须存活到进程结束！
```

**⚠️ WorkerGuard 是核心**：
- `_guard` drop 时，后台 writer 线程会被 shut down
- 如果 `_guard` 提前 drop，缓冲区中未刷出的日志会**丢失**
- 标准做法：**存到 `main()` 的顶层变量，直到所有 await 结束**

### 3.3 标准 main() 装配模板

```rust
#[tokio::main]
async fn main() {
    // Step 1: 创建 file appender
    let file_appender = tracing_appender::rolling::daily("/var/log/visp", "visp.log");
    let (non_blocking_writer, _guard) = tracing_appender::non_blocking(file_appender);
    //  ⚠️ _guard 绑定在 main 作用域，直到函数返回才 drop

    // Step 2: 装配 subscriber
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    tracing_subscriber::registry()
        .with(env_filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(non_blocking_writer)   // 文件输出
                .json()                             // JSON 格式（可选）
        )
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stdout)       // 同时输出到终端
        )
        .init();

    // ... 你的 async 应用逻辑 ...

    // main 返回时 _guard drop，此时所有日志已刷出
}
```

### 3.4 JSON 格式输出

```rust
use tracing_subscriber::fmt;

fmt::layer()
    .json()           // 结构化 JSON 输出
    .with_current_span(true)     // 在事件中包含当前 span
    .with_span_list(true)        // 包含 span 列表
    .flatten_event(false)        // 不展平 event fields（默认 false）
    .with_target(true)           // 包含 target 字段
```

---

## 4. 测试基础设施（Step 3 / Step 4 测试用）

### 4.1 临时 subscriber + DefaultGuard

来源：`tracing-subscriber` 0.3.x [set_default](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/util/trait.SubscriberInitExt.html#method.set_default)

```rust
use tracing_subscriber::{prelude::*, Registry};

// set_default() 返回 DefaultGuard，drop 时自动还原全局 subscriber
let _guard = tracing_subscriber::registry()
    .with(MyTestLayer::new(&output))
    .set_default();   // ← 不是 init()，而是 set_default()

// 在 _guard 作用域内，这个 subscriber 生效
// drop(_guard) → 还原为之前的全局 subscriber
```

### 4.2 手写最简 in-memory Layer 收集 span/event

来源：手写，基于 `tracing-subscriber` 0.3.x Layer trait

**推荐用这个模式，而非 tracing-test**。理由：
- 零外部依赖
- 可以精确控制收集什么数据
- 可以断言自定义结构（如 Extensions 中的 SpanMetrics）

```rust
use std::sync::{Arc, Mutex};
use tracing::{
    span::{Attributes, Id, Record},
    Event, Subscriber,
};
use tracing_subscriber::{
    layer::{Context, Layer},
    registry::LookupSpan,
};
use tracing::field::{Field, Visit};
use std::fmt;

// ====== 收集到内存的数据结构 ======
#[derive(Debug, Clone)]
struct Collected {
    span_name: String,
    fields: Vec<(String, String)>,
    events: Vec<String>,
}

// ====== 测试 Layer ======
struct TestLayer {
    spans: Arc<Mutex<Vec<Collected>>>,
}

impl TestLayer {
    fn new(spans: Arc<Mutex<Vec<Collected>>>) -> Self {
        Self { spans }
    }
}

// ====== Field Visitor ======
struct SpanFieldVisitor {
    fields: Vec<(String, String)>,
}

impl Visit for SpanFieldVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.fields
            .push((field.name().to_string(), format!("{:?}", value)));
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        self.fields
            .push((field.name().to_string(), value.to_string()));
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.fields
            .push((field.name().to_string(), value.to_string()));
    }
}

impl<S> Layer<S> for TestLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, _id: &Id, _ctx: Context<'_, S>) {
        let mut visitor = SpanFieldVisitor { fields: Vec::new() };
        attrs.record(&mut visitor);

        self.spans.lock().unwrap().push(Collected {
            span_name: attrs.metadata().name().to_string(),
            fields: visitor.fields,
            events: Vec::new(),
        });
    }

    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        // 找到当前活跃的 span 并追加 event
        if let Ok(mut spans) = self.spans.lock() {
            if let Some(last) = spans.last_mut() {
                let metadata = event.metadata();
                last.events.push(metadata.name().to_string());
            }
        }
    }
}

// ====== 测试用例 ======
#[test]
fn test_span_contains_field() {
    let collected = Arc::new(Mutex::new(Vec::new()));

    let _guard = tracing_subscriber::registry()
        .with(TestLayer::new(collected.clone()))
        .set_default();

    // 执行被测试的代码
    let span = tracing::info_span!("test_span", my_field = "hello");
    let _enter = span.enter();
    tracing::info!("test_event");

    drop(_enter);
    drop(span);
    drop(_guard);   // explicit drop 以减少时序问题

    // 断言
    let spans = collected.lock().unwrap();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].span_name, "test_span");
    assert!(spans[0].fields.iter().any(|(k, v)| k == "my_field" && v == "hello"));
    assert!(spans[0].events.contains(&"event test_event".to_string()));
}
```

### 4.3 async 测试中使用 subscriber

```rust
use tracing_subscriber::{prelude::*, Registry};

#[tokio::test]
async fn test_async_with_tracing() {
    let collected = Arc::new(Mutex::new(Vec::new()));
    let _guard = tracing_subscriber::registry()
        .with(TestLayer::new(collected.clone()))
        .set_default();

    // 异步被测试代码
    run_agent_turn().await;

    // 断言
    let spans = collected.lock().unwrap();
    assert!(!spans.is_empty());
}
```

使用 `set_default()` 而非 `init()`，好处：
- 每个测试独立，互不干扰
- `DefaultGuard` drop 时自动恢复
- 可以嵌套（但通常不推荐）

### 4.4 tracing-test crate 评估

来源：`tracing-test` 0.2.6 [crates.io](https://crates.io/crates/tracing-test)

**优点**：
- 提供 `#[traced_test]` 属性宏，自动为每个测试设置 subscriber
- 提供 `logs_contain("text")` 断言宏，直接检查日志输出
- 快速上手，代码少

**缺点**：
- 只能检查文本日志（格式化后的字符串），不能断言 **结构化 fields**
- `tracing_test::internal::set_global_default` 使用 `MOCK_LOCK`，并发测试可能阻塞
- 不支持检查 Span 属性（只面向 Event 文本）
- 不适用于 visp 的 Span-based 可观测性场景（我们需要断言 span fields、metrics）

**结论**：
> **不推荐** visp 使用 `tracing-test`。visp 的测试需要检查 span 的结构化 fields、extensions 中的自定义 SpanMetrics，这些 `tracing-test` 不支持。手写 in-memory Layer（4.2 节）是更合适的选择。

---

## 5. visp-core dev-dependency 约束

### 5.1 问题

`visp-core` 是 IO-free crate，其 `Cargo.toml` 的 `[dependencies]` 中**不能**包含 `tracing-subscriber`、`tracing-appender` 等运行时库。但单元测试需要在受控 subscriber 环境下运行，以验证 span 生成逻辑。

### 5.2 解决方案

```toml
# crates/visp-core/Cargo.toml

[dependencies]
tracing = "0.1"

[dev-dependencies]       # ← 关键：仅测试使用
tracing-subscriber = "0.3"
```

### 5.3 单测模板

```rust
// crates/visp-core/src/agent_loop.rs

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::{prelude::*, Registry};
    // TestLayer 定义同上（可在 test_utils 模块中复用）

    #[test]
    fn test_agent_span_creation() {
        let collected = Arc::new(Mutex::new(Vec::new()));

        // 临时 subscriber，仅在此测试作用域内生效
        let _guard = Registry::default()
            .with(TestLayer::new(collected.clone()))
            .set_default();

        // 执行被测代码
        let span = tracing::info_span!("agent_loop", visp.session.id = "sess_1");
        let _enter = span.enter();

        // 断言
        drop(_enter);
        let spans = collected.lock().unwrap();
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].span_name, "agent_loop");
    }
}
```

**关键点**：
- `[dev-dependencies]` 不会污染生产依赖
- `set_default()` 而非 `init()`，避免全局状态冲突
- 每个测试独立设置和清理 subscriber

---

## 6. 风险与陷阱

### 6.1 async + tracing 常见坑：忘记 .instrument() 导致 span 脱离父链

```rust
// ❌ 错误：task 不在任何 span 中
let span = info_span!("parent").entered();
tokio::spawn(async {
    tracing::info!("this event has NO parent span");  // 丢失父链
});

// ✅ 正确
tokio::spawn(async {
    tracing::info!("this IS inside parent");
}.instrument(Span::current()));   // 或 .in_current_span()
```

**根因**：`tokio::spawn` 会**切换线程**，执行上下文不继承当前 span。必须显式 `.instrument()` 把 span 传给 future。

### 6.2 Layer 顺序敏感性：EnvFilter 必须最先

```rust
// ❌ 错误：EnvFilter 在 fmt 之后
tracing_subscriber::registry()
    .with(fmt::layer())          // ← 先添加
    .with(EnvFilter::new("info")) // ← filter 后加
    .init();
// 结果：filter 对 fmt layer 仍然生效（因为 per-layer filtering）
// 但 filter 自己的 span 创建通知不会被之前的 layer 收到

// ✅ 推荐：EnvFilter 最先
tracing_subscriber::registry()
    .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
    .with(fmt::layer())
    .with(MyLayer)
    .init();
```

### 6.3 record 字段未提前声明导致静默丢失

```rust
// ❌ 静默丢失
let span = info_span!("op");
span.record("duration_ms", 42);   // 字段从未声明 → 丢弃

// ✅ 必须提前声明
let span = info_span!("op", duration_ms = field::Empty);
span.record("duration_ms", 42);   // OK
```

**没有警告、没有 panic、没有返回 Result**。这是最隐蔽的 bug。

解决方案：任何动态 record 的字段，都在 `#[instrument(fields(key = Empty))]` 或 `info_span!("name", key = field::Empty)` 中预先声明。

### 6.4 WorkerGuard drop 顺序

```rust
// ❌ 危险：non_blocking_writer 在 _guard 之后 alive
let (non_blocking, _guard) = non_blocking(file_appender);
{
    let _subscriber_guard = registry()
        .with(fmt::layer().with_writer(non_blocking.clone()))
        .set_default();
    // ... use ...
}  // _subscriber_guard drop 先发生（OK）
// _guard drop 后发生（OK）
// 但如果反过来顺序就不行

// ✅ 标准做法：_guard 绑定在 main() 顶层变量
#[tokio::main]
async fn main() {
    let (non_blocking, _guard) = non_blocking(file_appender);
    // _guard 存活到 main 返回
    setup_tracing(non_blocking);
    run_app().await;
    // main 返回 → _guard drop → flush 所有日志
}
```

### 6.5 异步代码中 Span::enter() guard 跨 await

```rust
async fn bad() {
    let span = info_span!("bad");
    let _guard = span.enter();    // 进入 span
    some_async_fn().await;        // ❌ guard 跨 await！
    // 另一个 task 可能在 _guard 仍存活时运行在此线程上，
    // 导致完全不相关的代码被错误地归属到此 span
}

// ✅ 正确：使用 .instrument()
async fn good() {
    async move {
        some_async_fn().await;
    }
    .instrument(info_span!("good"))
    .await;
}
```

---

## 验证清单

在 Step 3 / Step 4 / Step 5-sub 实现前检查：

- [ ] 所有 `record()` 调用的字段在 span 创建时已声明（`field::Empty`）
- [ ] 所有 `tokio::spawn` 的 future 已 `.instrument(span)` 或 `.in_current_span()`
- [ ] `EnvFilter` 是 `registry().with()` 链的**第一个** layer
- [ ] `WorkerGuard` 绑定在函数顶层变量，不提前 drop
- [ ] 无异步代码中使用 `span.enter()` / `span.entered()` 返回的 guard 跨 await
- [ ] visp-core 的 `tracing-subscriber` 仅在 `[dev-dependencies]` 中
- [ ] 测试使用 `set_default()` 返回的 `DefaultGuard`，测试结束后自动恢复
