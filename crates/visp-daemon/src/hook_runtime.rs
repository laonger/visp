//! hook 运行时：规则适配 + 信任门控 + 分发 + 有界 drain
//! （设计 §5 D4 执行语义 / §7.2–§7.3 规则与作用域 / §5 D8 herdr 绑定接口 / §5 D13 关停 drain）。
//!
//! 本模块把配置层（[`HookRule`]）与执行内核（[`visp_hooks::Executor`]）接线起来：
//!
//! - **规则适配**：`HookRule` → [`DispatchRule`]（匹配/排序/队列）与 `HookRule` → [`ProcessSpec`]
//!   （进程执行规格）；`cwd` 中的字面量 `project` 解析为会话/项目路径。
//! - **信任门控**（设计 §7.3 / §8.2）：全局规则全部进入执行集；项目规则**仅**在
//!   [`verify`] `== Trusted` 时进入，未信任则整体跳过。
//! - **分发**：`BusEvent::Hook` 直接转 [`DispatchInput`]；`BusEvent::Frame` 经
//!   [`crate::hook_map`] 映射，未映射的帧忽略。总线 `seq` 回填到事件信封。
//! - **关停 drain**（设计 D13）：实现 [`HookDrainHost`]，在 budget 内停止接收新工作并
//!   尽力排空执行队列。
//!
//! [`HookDrainHost`] 原定义于 daemon 的优雅关停管道；为让执行器在**库层**即可实现该
//! 契约（bin 无法被库反向依赖），契约定义迁入本模块，并由 `shutdown` 重新导出以保持
//! 既有引用（`service.rs`/`main.rs`）不变。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;

use visp_config::hooks::{HookRule, OnFull};
use visp_config::trust::{HookTrustStore, TrustStatus, verify};
use visp_core::bus::BusEvent;
use visp_hooks::{
    DispatchInput, DispatchRule, Executor, Handler, HookEvent, HookPayload, Origin, PermissionKind,
    ProcessSpec, QueuePolicy, SessionSource, SpawnHandler,
};

use crate::hook_map::{MapCtx, map_frame};

/// 关停期的 hook drain 宿主（设计 D13）。
///
/// 契约：`drain` 必须在 `budget` 内返回；真实执行器停止接收新 hook 工作、等待在飞
/// 执行完成后返回。调用方另有同长硬超时兜底，宿主即使超时也不会拖住关停。
#[async_trait]
pub trait HookDrainHost: Send + Sync {
    /// 在 `budget` 内排空在飞的 hook 执行。
    async fn drain(&self, budget: Duration);
}

/// 以会话/项目路径构造默认映射上下文（`cwd=project_path`、`source=Startup`、`origin=Tui`）。
pub fn session_ctx(project_path: impl Into<String>) -> MapCtx {
    let project_path = project_path.into();
    MapCtx {
        cwd: project_path.clone(),
        source: SessionSource::Startup,
        origin: Origin::Tui,
        project_path,
    }
}

/// 规则队列溢出策略适配：`OnFull` → [`QueuePolicy`]（同义枚举，crate 边界单向映射）。
pub fn queue_policy(on_full: OnFull) -> QueuePolicy {
    match on_full {
        OnFull::DropNew => QueuePolicy::DropNew,
        OnFull::DropOld => QueuePolicy::DropOld,
        OnFull::CoalesceLatest => QueuePolicy::CoalesceLatest,
    }
}

/// 规则适配：`HookRule` → [`DispatchRule`]（仅匹配/排序/队列字段）。
pub fn dispatch_rule(rule: &HookRule) -> DispatchRule {
    DispatchRule {
        id: rule.id.clone(),
        order: rule.order,
        event: rule.event.clone(),
        matcher: rule.matcher.clone(),
        enabled: rule.enabled,
        on_full: queue_policy(rule.on_full),
        parallel: rule.parallel,
        cooldown_ms: rule.cooldown_ms,
    }
}

/// 规则适配：`HookRule` → [`ProcessSpec`]（进程执行规格）。
///
/// `env` 按 key 排序以确定性；`cwd` 的字面量 `project` 解析为 `project_path`，
/// 缺省保留 `None`（由 [`SpawnHandler`] 取 `project_path`）。
pub fn process_spec(rule: &HookRule, project_path: &str) -> ProcessSpec {
    let mut env: Vec<(String, String)> = rule
        .env
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    env.sort();
    ProcessSpec {
        command: rule.command.clone(),
        args: rule.args.clone(),
        env,
        timeout_ms: rule.timeout_ms,
        cwd: resolve_cwd(rule.cwd.as_deref(), project_path),
    }
}

/// 解析规则 `cwd`：字面量 `project` → 会话/项目路径；其余原样。
fn resolve_cwd(cwd: Option<&str>, project_path: &str) -> Option<PathBuf> {
    match cwd {
        None => None,
        Some("project") => Some(PathBuf::from(project_path)),
        Some(path) => Some(PathBuf::from(path)),
    }
}

/// 便捷入口：以信任存储判定项目级规则是否可运行（`trust::verify(...) == Trusted`）。
///
/// 无项目级规则时 [`verify`] 返回 `NotRequired`，此处返回 `false`（无需门控，交由
/// 调用方按「无项目规则」处理）。
pub fn project_rules_trusted(project: &Path, rules: &[HookRule], store: &HookTrustStore) -> bool {
    matches!(verify(project, rules, store), TrustStatus::Trusted)
}

/// hook 运行时：持有执行器与映射上下文，向总线消费方提供分发与 drain 入口。
pub struct HookRuntime {
    executor: Executor,
    map_ctx: MapCtx,
    /// 是否接收新派发；drain 后置 `false`。
    accepting: AtomicBool,
}

impl HookRuntime {
    /// 构建运行时（进程 Handler 为 [`SpawnHandler`]）。
    ///
    /// 全局规则全部进入执行集；项目规则**仅**在 `trusted` 为真时进入。**零规则**时
    /// 返回一个「空运行时不派发」的实例（不返回 `None`）——是否构建由调用方决定。
    /// `trusted` 由调用方经 [`project_rules_trusted`] / [`verify`] 得出。
    pub fn build(
        global_rules: &[HookRule],
        project_rules: &[HookRule],
        trusted: bool,
        map_ctx: MapCtx,
    ) -> Self {
        let project_path = map_ctx.project_path.clone();
        let selected = select_rules(global_rules, project_rules, trusted);
        let specs: HashMap<String, ProcessSpec> = selected
            .iter()
            .map(|rule| (rule.id.clone(), process_spec(rule, &project_path)))
            .collect();
        let handler: Arc<dyn Handler> = Arc::new(SpawnHandler::new(project_path, specs));
        Self::assemble(&selected, map_ctx, handler)
    }

    /// 以可注入 [`Handler`] 构建运行时（测试/内嵌 consumers 用，如 herdr D8）。
    pub fn build_with_handler(
        global_rules: &[HookRule],
        project_rules: &[HookRule],
        trusted: bool,
        map_ctx: MapCtx,
        handler: Arc<dyn Handler>,
    ) -> Self {
        let selected = select_rules(global_rules, project_rules, trusted);
        Self::assemble(&selected, map_ctx, handler)
    }

    /// 由已选规则集与 Handler 组装。
    fn assemble(selected: &[&HookRule], map_ctx: MapCtx, handler: Arc<dyn Handler>) -> Self {
        let rules: Vec<DispatchRule> = selected.iter().map(|rule| dispatch_rule(rule)).collect();
        Self {
            executor: Executor::new(rules, handler),
            map_ctx,
            accepting: AtomicBool::new(true),
        }
    }

    /// 分发一个总线事件。
    ///
    /// `Hook` 事件直接转为 [`DispatchInput`]（总线 `seq` 回填信封）；`Frame` 事件经
    /// [`map_frame`] 映射，未映射的帧忽略。`drain` 之后不再接收新派发。
    pub fn dispatch_event(&self, event: BusEvent, seq: u64) {
        if !self.accepting.load(Ordering::Acquire) {
            return;
        }
        let hook_event = match event {
            BusEvent::Hook(event) => event,
            BusEvent::Frame(frame) => match map_frame(&frame, &self.map_ctx) {
                Some(event) => event,
                None => return,
            },
        };
        self.executor.dispatch(dispatch_input(hook_event, seq));
    }

    /// 停止接收新派发，并在 `budget` 内尽力排空执行队列；返回是否在 budget 内排空。
    pub async fn drain(&self, budget: Duration) -> bool {
        self.accepting.store(false, Ordering::Release);
        self.executor.drain(budget).await
    }
}

#[async_trait]
impl HookDrainHost for HookRuntime {
    async fn drain(&self, budget: Duration) {
        let _ = HookRuntime::drain(self, budget).await;
    }
}

/// 规则收集 + 信任门控：全局全部，项目仅在受信任时并入。
fn select_rules<'a>(
    global_rules: &'a [HookRule],
    project_rules: &'a [HookRule],
    trusted: bool,
) -> Vec<&'a HookRule> {
    let mut selected: Vec<&HookRule> = global_rules.iter().collect();
    if trusted {
        selected.extend(project_rules.iter());
    } else if !project_rules.is_empty() {
        tracing::warn!(
            count = project_rules.len(),
            "项目 hook 规则未受信任，本次不加载"
        );
    }
    selected
}

/// [`HookEvent`] → [`DispatchInput`]：回填总线 `seq`，提取 `matcher` 维度与 JSON 载荷。
fn dispatch_input(mut event: HookEvent, seq: u64) -> DispatchInput {
    event.context.seq = Some(seq);
    let mut input =
        DispatchInput::new(event.event_name()).with_source(source_str(event.context.source));
    if let Some(tool_name) = payload_tool_name(&event.payload) {
        input = input.with_tool_name(tool_name);
    }
    if let Some(kind) = payload_kind(&event.payload) {
        input = input.with_kind(kind);
    }
    input.with_payload(event.to_value().to_string())
}

/// 会话来源的 `matcher` 视图（与序列化拼写一致）。
fn source_str(source: SessionSource) -> &'static str {
    match source {
        SessionSource::Startup => "startup",
        SessionSource::Resume => "resume",
    }
}

/// 工具类事件携带的 `tool_name`。
fn payload_tool_name(payload: &HookPayload) -> Option<&str> {
    match payload {
        HookPayload::ToolCallRequested(p) => Some(p.tool_name.as_str()),
        HookPayload::PreToolUse(p) => Some(p.tool_name.as_str()),
        HookPayload::PostToolUse(p) => Some(p.tool_name.as_str()),
        HookPayload::PostToolUseFailure(p) => Some(p.tool_name.as_str()),
        _ => None,
    }
}

/// `PermissionRequest.kind` 的 `matcher` 视图。
fn payload_kind(payload: &HookPayload) -> Option<&'static str> {
    match payload {
        HookPayload::PermissionRequest(p) => Some(match p.kind {
            PermissionKind::Approval => "approval",
            PermissionKind::Question => "question",
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap as Map;
    use std::time::Instant;

    use visp_config::hooks::{DEFAULT_HOOK_TIMEOUT_MS, HookScope};
    use visp_core::agent::{AgentEvent, AgentEventFrame};
    use visp_hooks::{
        HookContext, HookEventName, RecordingHandler, StopPayload, StopStatus, VISP_HOOK_SCHEMA,
    };

    const PROJECT: &str = "/tmp/visp-project";

    fn rule(id: &str) -> HookRule {
        HookRule {
            id: id.to_string(),
            order: None,
            event: vec![HookEventName::Stop],
            matcher: None,
            command: "/bin/true".to_string(),
            args: Vec::new(),
            env: Map::new(),
            cwd: None,
            timeout_ms: DEFAULT_HOOK_TIMEOUT_MS,
            enabled: true,
            on_full: OnFull::DropNew,
            parallel: false,
            cooldown_ms: 0,
            include: Vec::new(),
            scope: HookScope::Global,
        }
    }

    fn frame(event: AgentEvent) -> AgentEventFrame {
        AgentEventFrame {
            event,
            session_id: "s1".to_string(),
            agent_name: "agent".to_string(),
            parent_session_id: None,
            parent_session_name: None,
        }
    }

    fn stop_event() -> BusEvent {
        BusEvent::Hook(HookEvent {
            context: HookContext {
                schema: VISP_HOOK_SCHEMA,
                hook_event_name: HookEventName::Stop,
                session_id: "s1".to_string(),
                cwd: PROJECT.to_string(),
                source: SessionSource::Startup,
                origin: Origin::Tui,
                seq: None,
            },
            payload: HookPayload::Stop(StopPayload {
                status: StopStatus::Completed,
                input_tokens: 3,
                output_tokens: 5,
                tool_calls: 1,
            }),
        })
    }

    fn tool_frame() -> BusEvent {
        BusEvent::Frame(frame(AgentEvent::ToolCallRequest {
            call_id: "c1".to_string(),
            tool_name: "Bash".to_string(),
            arguments: "{}".to_string(),
        }))
    }

    fn runtime_with(
        global: &[HookRule],
        project: &[HookRule],
        trusted: bool,
        handler: Arc<RecordingHandler>,
    ) -> HookRuntime {
        HookRuntime::build_with_handler(global, project, trusted, session_ctx(PROJECT), handler)
    }

    /// 1. `HookRule` → `DispatchRule` 字段逐一映射。
    #[test]
    fn dispatch_rule_maps_every_field() {
        let mut rule = rule("r1");
        rule.order = Some(7);
        rule.event = vec![HookEventName::Stop, HookEventName::SessionEnd];
        rule.matcher = Some("^Bash$".to_string());
        rule.enabled = false;
        rule.on_full = OnFull::DropOld;
        rule.parallel = true;
        rule.cooldown_ms = 250;

        let dispatch = dispatch_rule(&rule);
        assert_eq!(dispatch.id, "r1");
        assert_eq!(dispatch.order, Some(7));
        assert_eq!(
            dispatch.event,
            vec![HookEventName::Stop, HookEventName::SessionEnd]
        );
        assert_eq!(dispatch.matcher.as_deref(), Some("^Bash$"));
        assert!(!dispatch.enabled);
        assert_eq!(dispatch.on_full, QueuePolicy::DropOld);
        assert!(dispatch.parallel);
        assert_eq!(dispatch.cooldown_ms, 250);
    }

    /// 2. `OnFull` ↔ `QueuePolicy` 逐一往返（含缺省 `DropNew`）。
    #[test]
    fn on_full_round_trips_to_queue_policy() {
        for (on_full, policy) in [
            (OnFull::DropNew, QueuePolicy::DropNew),
            (OnFull::DropOld, QueuePolicy::DropOld),
            (OnFull::CoalesceLatest, QueuePolicy::CoalesceLatest),
        ] {
            assert_eq!(queue_policy(on_full), policy);
            let mut rule = rule("r");
            rule.on_full = on_full;
            assert_eq!(dispatch_rule(&rule).on_full, policy);
        }
    }

    /// 3. `HookRule` → `ProcessSpec` 字段映射与 `cwd` 解析。
    #[test]
    fn process_spec_maps_fields_and_resolves_cwd() {
        let mut rule = rule("r");
        rule.command = "/bin/echo".to_string();
        rule.args = vec!["hi".to_string()];
        rule.env = Map::from([
            ("B".to_string(), "2".to_string()),
            ("A".to_string(), "1".to_string()),
        ]);
        rule.cwd = Some("project".to_string());
        rule.timeout_ms = 1234;

        let spec = process_spec(&rule, PROJECT);
        assert_eq!(spec.command, "/bin/echo");
        assert_eq!(spec.args, vec!["hi".to_string()]);
        assert_eq!(
            spec.env,
            vec![
                ("A".to_string(), "1".to_string()),
                ("B".to_string(), "2".to_string())
            ]
        );
        assert_eq!(spec.cwd, Some(PathBuf::from(PROJECT)));
        assert_eq!(spec.timeout_ms, 1234);

        // 缺省 cwd → None（执行器回退 project_path）。
        rule.cwd = None;
        assert_eq!(process_spec(&rule, PROJECT).cwd, None);

        // 显式路径原样透传。
        rule.cwd = Some("/elsewhere".to_string());
        assert_eq!(
            process_spec(&rule, PROJECT).cwd,
            Some(PathBuf::from("/elsewhere"))
        );
    }

    /// 4a. 项目规则未受信任 → 不进入执行集（只有全局规则被执行）。
    #[tokio::test]
    async fn untrusted_project_rules_are_excluded() {
        let handler = Arc::new(RecordingHandler::new());
        let global = vec![rule("global")];
        let project = vec![rule("project-rule")];
        let runtime = runtime_with(&global, &project, false, handler.clone());

        runtime.dispatch_event(stop_event(), 1);
        tokio::time::timeout(Duration::from_secs(1), handler.wait_for(1))
            .await
            .expect("全局规则应被执行");

        let records = handler.records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].rule_id, "global");
    }

    /// 4b. 项目规则受信任 → 与全局规则一并进入执行集。
    #[tokio::test]
    async fn trusted_project_rules_are_included() {
        let handler = Arc::new(RecordingHandler::new());
        let global = vec![rule("global")];
        let project = vec![rule("project-rule")];
        let runtime = runtime_with(&global, &project, true, handler.clone());

        runtime.dispatch_event(stop_event(), 1);
        tokio::time::timeout(Duration::from_secs(1), handler.wait_for(2))
            .await
            .expect("全局与项目规则均应被执行");

        let ids: Vec<String> = handler
            .records()
            .into_iter()
            .map(|record| record.rule_id)
            .collect();
        assert_eq!(ids, vec!["global".to_string(), "project-rule".to_string()]);
    }

    /// 4c. `project_rules_trusted` 走 `trust::verify`：未信任 false、信任后 true。
    #[test]
    fn project_rules_trusted_follows_verify() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path();
        std::fs::create_dir_all(project.join(".visp/hooks")).unwrap();
        std::fs::write(project.join(".visp/hooks/a.sh"), "#!/bin/sh\n").unwrap();

        let mut rule = rule("p");
        rule.scope = HookScope::Project;
        rule.command = ".visp/hooks/a.sh".to_string();
        let rules = vec![rule];

        let mut store = HookTrustStore::default();
        assert!(!project_rules_trusted(project, &rules, &store));
        store.trust(project, &rules).unwrap();
        assert!(project_rules_trusted(project, &rules, &store));
    }

    /// 5. `Hook` 事件 → Handler 被调用一次，事件/信号与 seq 回填正确。
    #[tokio::test]
    async fn hook_event_dispatches_once_with_payload_and_seq() {
        let handler = Arc::new(RecordingHandler::new());
        let runtime = runtime_with(&[rule("r")], &[], false, handler.clone());

        runtime.dispatch_event(stop_event(), 42);
        tokio::time::timeout(Duration::from_secs(1), handler.wait_for(1))
            .await
            .expect("handler 应被调用一次");

        let records = handler.records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].rule_id, "r");
        assert_eq!(records[0].event.event, HookEventName::Stop);
        assert_eq!(records[0].event.source.as_deref(), Some("startup"));

        let payload: serde_json::Value =
            serde_json::from_str(records[0].event.payload.as_deref().unwrap()).unwrap();
        assert_eq!(payload["hook_event_name"], "Stop");
        assert_eq!(payload["seq"], 42);
    }

    /// 6. `Frame` 事件经 `hook_map` 映射后派发（tool_name 参与 matcher）。
    #[tokio::test]
    async fn mapped_frame_dispatches_tool_event() {
        let mut rule = rule("r");
        rule.event = vec![HookEventName::ToolCallRequested];
        rule.matcher = Some("^Bash$".to_string());
        let handler = Arc::new(RecordingHandler::new());
        let runtime = runtime_with(&[rule], &[], false, handler.clone());

        runtime.dispatch_event(tool_frame(), 3);
        tokio::time::timeout(Duration::from_secs(1), handler.wait_for(1))
            .await
            .expect("映射后的事件应被派发");

        let records = handler.records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].event.event, HookEventName::ToolCallRequested);
        assert_eq!(records[0].event.tool_name.as_deref(), Some("Bash"));
    }

    /// 7. `Frame` 映射为 `None` → 不调用（规则命中全部事件仍不触发）。
    #[tokio::test]
    async fn unmapped_frame_is_ignored() {
        let mut rule = rule("r");
        rule.event = HookEventName::ALL.to_vec();
        let handler = Arc::new(RecordingHandler::new());
        let runtime = runtime_with(&[rule], &[], false, handler.clone());

        runtime.dispatch_event(
            BusEvent::Frame(frame(AgentEvent::TextDelta("hi".to_string()))),
            1,
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(handler.records().is_empty());
    }

    /// 8. 零规则 → 无派发（返回空运行时，不返回 None）。
    #[tokio::test]
    async fn zero_rules_dispatch_nothing() {
        let handler = Arc::new(RecordingHandler::new());
        let runtime = runtime_with(&[], &[], false, handler.clone());

        runtime.dispatch_event(stop_event(), 1);
        runtime.dispatch_event(tool_frame(), 2);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(handler.records().is_empty());
    }

    /// 9a. 忙碌时 `drain` 在 budget 内返回（有界，不悬挂），并停止接收新派发。
    #[tokio::test]
    async fn drain_is_bounded_when_busy() {
        let handler = Arc::new(RecordingHandler::blocking());
        let runtime = runtime_with(&[rule("r")], &[], false, handler.clone());

        runtime.dispatch_event(stop_event(), 1);
        tokio::time::timeout(Duration::from_secs(1), handler.wait_for(1))
            .await
            .expect("worker 应已进入执行");

        let budget = Duration::from_millis(80);
        let started = Instant::now();
        let drained = runtime.drain(budget).await;
        let elapsed = started.elapsed();

        assert!(!drained, "阻塞执行时不应判为排空");
        assert!(elapsed >= budget, "应在 budget 用尽后返回");
        assert!(
            elapsed < budget + Duration::from_secs(1),
            "应尽快返回，不悬挂"
        );

        runtime.dispatch_event(stop_event(), 2);
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(handler.records().len(), 1, "drain 后不应再派发");

        handler.release();
    }

    /// 9b. 空闲时 `drain` 立即判为排空。
    #[tokio::test]
    async fn drain_returns_true_when_idle() {
        let handler = Arc::new(RecordingHandler::new());
        let runtime = runtime_with(&[rule("r")], &[], false, handler.clone());

        runtime.dispatch_event(stop_event(), 1);
        tokio::time::timeout(Duration::from_secs(1), handler.wait_for(1))
            .await
            .expect("worker 应已执行");
        tokio::time::sleep(Duration::from_millis(20)).await;

        assert!(runtime.drain(Duration::from_millis(500)).await);
    }

    /// 9c. `HookDrainHost` 可经 `Arc<dyn>` 注入（关停管道契约）。
    #[tokio::test]
    async fn drain_host_trait_object_works() {
        let handler = Arc::new(RecordingHandler::new());
        let runtime = runtime_with(&[], &[], false, handler);
        let host: Arc<dyn HookDrainHost> = Arc::new(runtime);
        host.drain(Duration::from_millis(10)).await;
    }
}
