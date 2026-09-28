//! 1b-2a 决策内核测试（实施计划步骤 1b-2 测试表第 1–6 条）。
//!
//! 依据：设计 D4 / §9 执行语义。全程不 spawn 进程，仅用内存 [`RecordingHandler`]。

use std::sync::Arc;
use std::time::Duration;

use tokio::time::timeout;
use visp_hooks::*;

/// 单条规则构造快捷方式（其余字段取默认）。
fn rule(id: &str) -> DispatchRule {
    DispatchRule {
        id: id.to_string(),
        event: vec![HookEventName::Stop],
        ..Default::default()
    }
}

fn input(name: &str) -> DispatchInput {
    DispatchInput::new(HookEventName::PostToolUse).with_tool_name(name)
}

/// 5 秒上限的等待，避免内核缺陷导致测试悬挂。
async fn wait(handler: &RecordingHandler, n: usize) {
    timeout(Duration::from_secs(5), handler.wait_for(n))
        .await
        .expect("等待 handler 调用超时");
}

// 1. 匹配：event 命中/未命中；matcher 命中 tool_name/source/kind；空 matcher 全匹配。
#[test]
fn matching_event_targets_and_empty_matcher() {
    let tool = DispatchRule {
        id: "tool".to_string(),
        event: vec![HookEventName::PostToolUse],
        matcher: Some("^Bash$".to_string()),
        ..Default::default()
    };
    assert!(matches(
        &tool,
        &DispatchInput::new(HookEventName::PostToolUse).with_tool_name("Bash")
    ));
    assert!(!matches(
        &tool,
        &DispatchInput::new(HookEventName::PostToolUse).with_tool_name("Read")
    ));
    // 事件名未命中。
    assert!(!matches(
        &tool,
        &DispatchInput::new(HookEventName::Stop).with_tool_name("Bash")
    ));
    // matcher 存在但事件不携带任一匹配目标 → 不命中。
    assert!(!matches(
        &tool,
        &DispatchInput::new(HookEventName::PostToolUse)
    ));

    // matcher 匹配 source。
    let source = DispatchRule {
        id: "source".to_string(),
        event: vec![HookEventName::SessionStart],
        matcher: Some("^resume$".to_string()),
        ..Default::default()
    };
    assert!(matches(
        &source,
        &DispatchInput::new(HookEventName::SessionStart).with_source("resume")
    ));
    assert!(!matches(
        &source,
        &DispatchInput::new(HookEventName::SessionStart).with_source("startup")
    ));

    // matcher 匹配 kind。
    let kind = DispatchRule {
        id: "kind".to_string(),
        event: vec![HookEventName::PermissionRequest],
        matcher: Some("^approval$".to_string()),
        ..Default::default()
    };
    assert!(matches(
        &kind,
        &DispatchInput::new(HookEventName::PermissionRequest).with_kind("approval")
    ));
    assert!(!matches(
        &kind,
        &DispatchInput::new(HookEventName::PermissionRequest).with_kind("question")
    ));

    // 空 matcher（None）= 全匹配。
    assert!(matches(
        &rule("any"),
        &DispatchInput::new(HookEventName::Stop)
    ));

    // 未启用规则不命中。
    let disabled = DispatchRule {
        id: "off".to_string(),
        event: vec![HookEventName::Stop],
        enabled: false,
        ..Default::default()
    };
    assert!(!matches(
        &disabled,
        &DispatchInput::new(HookEventName::Stop)
    ));
}

// 2. 排序：同事件多规则按 id 字典序；order 覆盖；内置 builtin: 前缀参与排序。
#[test]
fn ordering_is_by_id_with_order_override() {
    let event = DispatchInput::new(HookEventName::Stop);
    let mk = |id: &str, order: Option<i64>| DispatchRule {
        id: id.to_string(),
        order,
        event: vec![HookEventName::Stop],
        ..Default::default()
    };

    let rules = vec![mk("c", None), mk("a", None), mk("b", None)];
    let ids: Vec<&str> = select_matches(&rules, &event)
        .iter()
        .map(|r| r.id.as_str())
        .collect();
    assert_eq!(ids, ["a", "b", "c"]);

    // order 覆盖：负值提前，正值靠后。
    let rules = vec![mk("a", None), mk("z", Some(-1)), mk("b", Some(5))];
    let ids: Vec<&str> = select_matches(&rules, &event)
        .iter()
        .map(|r| r.id.as_str())
        .collect();
    assert_eq!(ids, ["z", "a", "b"]);

    // builtin: 前缀按普通字典序参与。
    let rules = vec![mk("builtin:herdr.session_start", None), mk("alpha", None)];
    let ids: Vec<&str> = select_matches(&rules, &event)
        .iter()
        .map(|r| r.id.as_str())
        .collect();
    assert_eq!(ids, ["alpha", "builtin:herdr.session_start"]);

    // 未命中事件的规则被排除。
    let other = DispatchRule {
        id: "x".to_string(),
        event: vec![HookEventName::SessionEnd],
        ..Default::default()
    };
    let rules = vec![mk("a", None), other];
    let ids: Vec<&str> = select_matches(&rules, &event)
        .iter()
        .map(|r| r.id.as_str())
        .collect();
    assert_eq!(ids, ["a"]);
}

// 3. 串行：同规则连续事件按到达顺序执行，不并发交错。
#[tokio::test]
async fn serial_rule_preserves_arrival_order_without_interleave() {
    let handler = Arc::new(RecordingHandler::with_delay(Duration::from_millis(20)));
    let spec = DispatchRule {
        id: "serial".to_string(),
        event: vec![HookEventName::PostToolUse],
        ..Default::default()
    };
    let executor = Executor::new(vec![spec], handler.clone());

    for name in ["first", "second", "third"] {
        executor.dispatch(input(name));
    }
    wait(&handler, 3).await;

    let order: Vec<String> = handler
        .records()
        .iter()
        .map(|r| r.event.tool_name.clone().unwrap())
        .collect();
    assert_eq!(order, ["first", "second", "third"]);
    assert_eq!(handler.max_concurrency(), 1);
}

// 4. parallel = true 放开同规则并发。
#[tokio::test]
async fn parallel_rule_runs_concurrently() {
    let handler = Arc::new(RecordingHandler::blocking());
    let spec = DispatchRule {
        id: "parallel".to_string(),
        event: vec![HookEventName::PostToolUse],
        parallel: true,
        ..Default::default()
    };
    let executor = Executor::new(vec![spec], handler.clone());

    for name in ["a", "b", "c"] {
        executor.dispatch(input(name));
    }
    wait(&handler, 3).await;
    assert_eq!(handler.max_concurrency(), 3);

    handler.release();
}

// 5. on_full：drop_new 丢新 / drop_old 丢旧 / coalesce_latest 合并为最新。
#[tokio::test]
async fn on_full_policies_drop_and_coalesce() {
    assert_eq!(run_on_full(QueuePolicy::DropNew).await, ["A", "B", "C"]);
    assert_eq!(run_on_full(QueuePolicy::DropOld).await, ["A", "C", "D"]);
    assert_eq!(run_on_full(QueuePolicy::CoalesceLatest).await, ["A", "D"]);
}

/// 队列容量 2，先派发 A（阻塞在飞行中）再灌 B/C/D 制造溢出，返回实际执行顺序。
async fn run_on_full(policy: QueuePolicy) -> Vec<String> {
    let handler = Arc::new(RecordingHandler::blocking());
    let spec = DispatchRule {
        id: "queue".to_string(),
        event: vec![HookEventName::PostToolUse],
        on_full: policy,
        ..Default::default()
    };
    let executor = Executor::with_capacity(vec![spec], handler.clone(), 2);

    executor.dispatch(input("A"));
    wait(&handler, 1).await; // A 已在飞行中（已出队）
    executor.dispatch(input("B"));
    executor.dispatch(input("C"));
    executor.dispatch(input("D")); // 触发溢出策略
    handler.release();

    let expected = match policy {
        QueuePolicy::DropNew | QueuePolicy::DropOld => 3,
        QueuePolicy::CoalesceLatest => 2,
    };
    wait(&handler, expected).await;
    handler
        .records()
        .iter()
        .map(|r| r.event.tool_name.clone().unwrap())
        .collect()
}

// 6. cooldown_ms：窗口内同规则事件被丢弃。
#[tokio::test]
async fn cooldown_suppresses_events_within_window() {
    let handler = Arc::new(RecordingHandler::new());
    let spec = DispatchRule {
        id: "cooldown".to_string(),
        event: vec![HookEventName::Stop],
        cooldown_ms: 60_000,
        ..Default::default()
    };
    let executor = Executor::new(vec![spec], handler.clone());

    executor.dispatch(DispatchInput::new(HookEventName::Stop));
    wait(&handler, 1).await;
    executor.dispatch(DispatchInput::new(HookEventName::Stop));
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(handler.records().len(), 1);

    // cooldown = 0 时全部放行（回归）。
    let handler0 = Arc::new(RecordingHandler::new());
    let spec0 = DispatchRule {
        id: "no-cooldown".to_string(),
        event: vec![HookEventName::Stop],
        ..Default::default()
    };
    let executor0 = Executor::new(vec![spec0], handler0.clone());
    executor0.dispatch(DispatchInput::new(HookEventName::Stop));
    executor0.dispatch(DispatchInput::new(HookEventName::Stop));
    wait(&handler0, 2).await;
    assert_eq!(handler0.records().len(), 2);
}
