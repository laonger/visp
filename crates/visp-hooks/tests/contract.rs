//! 0a 契约测试：冻结事件名、公共信封、取值域、逐事件字段集合、默认脱敏、schema 版本。
//!
//! 依据：设计 §6.2/§6.3、实施计划步骤 0a 测试用例表。

use serde_json::{Value, json};
use visp_hooks::*;

/// 13 个冻结事件的 (枚举, 精确 PascalCase) 名单。
const NAMES: [(HookEventName, &str); 13] = [
    (HookEventName::SessionStart, "SessionStart"),
    (HookEventName::UserPromptSubmit, "UserPromptSubmit"),
    (HookEventName::AgentRunEnd, "AgentRunEnd"),
    (HookEventName::SubagentStop, "SubagentStop"),
    (HookEventName::Stop, "Stop"),
    (HookEventName::StopFailure, "StopFailure"),
    (HookEventName::ToolCallRequested, "ToolCallRequested"),
    (HookEventName::PreToolUse, "PreToolUse"),
    (HookEventName::PostToolUse, "PostToolUse"),
    (HookEventName::PostToolUseFailure, "PostToolUseFailure"),
    (HookEventName::PermissionRequest, "PermissionRequest"),
    (HookEventName::PermissionResult, "PermissionResult"),
    (HookEventName::SessionEnd, "SessionEnd"),
];

const ENVELOPE: [&str; 6] = [
    "schema",
    "hook_event_name",
    "session_id",
    "cwd",
    "source",
    "origin",
];

fn keys(value: &Value) -> Vec<String> {
    let mut out: Vec<String> = value
        .as_object()
        .expect("hook event 序列化后必须是 JSON 对象")
        .keys()
        .cloned()
        .collect();
    out.sort();
    out
}

fn collect(keys: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = keys.iter().map(|k| (*k).to_string()).collect();
    out.sort();
    out
}

fn context(name: HookEventName) -> HookContext {
    HookContext {
        schema: VISP_HOOK_SCHEMA,
        hook_event_name: name,
        session_id: "sess-1".to_string(),
        cwd: "/tmp/proj".to_string(),
        source: SessionSource::Startup,
        origin: Origin::Tui,
        seq: None,
    }
}

fn event(name: HookEventName, payload: HookPayload) -> HookEvent {
    HookEvent {
        context: context(name),
        payload,
    }
}

fn sample_stop() -> HookEvent {
    event(
        HookEventName::Stop,
        HookPayload::Stop(StopPayload {
            status: StopStatus::Completed,
            input_tokens: 10,
            output_tokens: 20,
            tool_calls: 3,
        }),
    )
}

/// 返回 (样本, 该事件 JSON 顶层键的完整期望集合)。
fn all_samples() -> Vec<(HookEvent, Vec<&'static str>)> {
    vec![
        (
            event(
                HookEventName::SessionStart,
                HookPayload::SessionStart(SessionStartPayload {
                    short_id: "short".to_string(),
                    project_path: "/tmp/proj".to_string(),
                    model: "model".to_string(),
                    model_key: "key".to_string(),
                    agent_name: "main".to_string(),
                    parent_session_id: Some("parent".to_string()),
                }),
            ),
            vec![
                "short_id",
                "project_path",
                "model",
                "model_key",
                "agent_name",
                "parent_session_id",
            ],
        ),
        (
            event(
                HookEventName::UserPromptSubmit,
                HookPayload::UserPromptSubmit(UserPromptSubmitPayload {
                    prompt_chars: 42,
                    prompt: None,
                }),
            ),
            vec!["prompt_chars"],
        ),
        (
            event(
                HookEventName::AgentRunEnd,
                HookPayload::AgentRunEnd(AgentRunEndPayload {
                    status: "completed".to_string(),
                }),
            ),
            vec!["status"],
        ),
        (
            event(
                HookEventName::SubagentStop,
                HookPayload::SubagentStop(SubagentStopPayload {
                    parent_session_id: "parent".to_string(),
                    agent_name: "child".to_string(),
                    status: "completed".to_string(),
                }),
            ),
            vec!["parent_session_id", "agent_name", "status"],
        ),
        (
            sample_stop(),
            vec!["status", "input_tokens", "output_tokens", "tool_calls"],
        ),
        (
            event(
                HookEventName::StopFailure,
                HookPayload::StopFailure(StopFailurePayload {
                    code: "Internal".to_string(),
                    message: "boom".to_string(),
                }),
            ),
            vec!["code", "message"],
        ),
        (
            event(
                HookEventName::ToolCallRequested,
                HookPayload::ToolCallRequested(ToolCallRequestedPayload {
                    tool_use_id: "t1".to_string(),
                    tool_name: "read".to_string(),
                }),
            ),
            vec!["tool_use_id", "tool_name"],
        ),
        (
            event(
                HookEventName::PreToolUse,
                HookPayload::PreToolUse(PreToolUsePayload {
                    tool_use_id: "t1".to_string(),
                    tool_name: "read".to_string(),
                    requires_approval: true,
                }),
            ),
            vec!["tool_use_id", "tool_name", "requires_approval"],
        ),
        (
            event(
                HookEventName::PostToolUse,
                HookPayload::PostToolUse(PostToolUsePayload {
                    tool_use_id: "t1".to_string(),
                    tool_name: "read".to_string(),
                    tool_input: None,
                    tool_response: None,
                }),
            ),
            vec!["tool_use_id", "tool_name"],
        ),
        (
            event(
                HookEventName::PostToolUseFailure,
                HookPayload::PostToolUseFailure(PostToolUseFailurePayload {
                    tool_use_id: "t1".to_string(),
                    tool_name: "read".to_string(),
                    message: "failed".to_string(),
                    tool_input: None,
                    tool_response: None,
                }),
            ),
            vec!["tool_use_id", "tool_name", "message"],
        ),
        (
            event(
                HookEventName::PermissionRequest,
                HookPayload::PermissionRequest(PermissionRequestPayload {
                    query_id: "q1".to_string(),
                    kind: "approval".to_string(),
                    options_count: 0,
                    message: None,
                }),
            ),
            vec!["query_id", "kind", "options_count"],
        ),
        (
            event(
                HookEventName::PermissionResult,
                HookPayload::PermissionResult(PermissionResultPayload {
                    query_id: "q1".to_string(),
                    outcome: "allowed".to_string(),
                    selected_index: 0,
                }),
            ),
            vec!["query_id", "outcome", "selected_index"],
        ),
        (
            event(
                HookEventName::SessionEnd,
                HookPayload::SessionEnd(SessionEndPayload {
                    reason: "shutdown".to_string(),
                    exit_code: Some(0),
                }),
            ),
            vec!["reason", "exit_code"],
        ),
    ]
}

// 0a-1：13 个冻结事件名逐一序列化为精确 PascalCase 字符串。
#[test]
fn event_names_serialize_to_exact_pascal_case() {
    assert_eq!(HookEventName::ALL.len(), 13);
    for (name, expected) in NAMES {
        assert_eq!(name.as_str(), expected);
        let serialized = serde_json::to_string(&name).expect("event name 可序列化");
        assert_eq!(serialized, format!("\"{expected}\""));
    }
}

// 0a-2：公共信封字段必现；seq 为 None 时省略（不输出 null）。
#[test]
fn envelope_fields_always_present_and_seq_omitted_when_none() {
    let value = sample_stop().to_value();
    for field in ENVELOPE {
        assert!(value.get(field).is_some(), "缺少信封字段 {field}");
    }
    assert!(value.get("seq").is_none(), "seq=None 时不应输出");

    let mut with_seq = sample_stop();
    with_seq.context.seq = Some(7);
    assert_eq!(with_seq.to_value().get("seq"), Some(&json!(7)));
}

// 0a-3：枚举取值域与 Option 字段省略。
#[test]
fn enum_domains_and_optional_fields() {
    assert_eq!(
        serde_json::to_value(SessionSource::Startup).unwrap(),
        json!("startup")
    );
    assert_eq!(
        serde_json::to_value(SessionSource::Resume).unwrap(),
        json!("resume")
    );
    assert_eq!(serde_json::to_value(Origin::Tui).unwrap(), json!("tui"));
    assert_eq!(
        serde_json::to_value(Origin::Headless).unwrap(),
        json!("headless")
    );
    assert_eq!(serde_json::to_value(Origin::Other).unwrap(), json!("other"));
    assert_eq!(
        serde_json::to_value(StopStatus::Completed).unwrap(),
        json!("completed")
    );
    assert_eq!(
        serde_json::to_value(StopStatus::Cancelled).unwrap(),
        json!("cancelled")
    );

    let none = event(
        HookEventName::SessionEnd,
        HookPayload::SessionEnd(SessionEndPayload {
            reason: "delete".to_string(),
            exit_code: None,
        }),
    )
    .to_value();
    assert!(none.get("exit_code").is_none(), "exit_code=None 时不应输出");

    let some = event(
        HookEventName::SessionEnd,
        HookPayload::SessionEnd(SessionEndPayload {
            reason: "shutdown".to_string(),
            exit_code: Some(0),
        }),
    )
    .to_value();
    assert_eq!(some.get("exit_code"), Some(&json!(0)));
}

// 0a-4：每事件载荷字段集合精确（不缺不多）。
#[test]
fn per_event_payload_field_set_is_exact() {
    for (sample, extra) in all_samples() {
        let mut expected: Vec<&str> = ENVELOPE.to_vec();
        expected.extend(extra);
        assert_eq!(
            keys(&sample.to_value()),
            collect(&expected),
            "事件 {} 字段集合不符",
            sample.event_name().as_str()
        );
    }
}

// 0a-5：默认脱敏——原文/工具载荷默认不出现。
#[test]
fn sensitive_fields_redacted_by_default() {
    let samples = all_samples();

    let prompt = &samples[1].0.to_value();
    assert!(
        prompt.get("prompt").is_none(),
        "UserPromptSubmit 默认不得携带 prompt"
    );

    let permission = &samples[10].0.to_value();
    assert!(
        permission.get("message").is_none(),
        "PermissionRequest 默认不得携带 message"
    );

    for (sample, _) in &samples {
        let value = sample.to_value();
        assert!(
            value.get("tool_input").is_none(),
            "工具事件默认不得携带 tool_input"
        );
        assert!(
            value.get("tool_response").is_none(),
            "工具事件默认不得携带 tool_response"
        );
    }
}

// 0a-6：schema 版本常量 = 1 且随每条事件输出；env 变量名常量符合设计 §6.3。
#[test]
fn schema_version_and_env_constant_names() {
    assert_eq!(VISP_HOOK_SCHEMA, 1);
    for (sample, _) in all_samples() {
        assert_eq!(sample.to_value().get("schema"), Some(&json!(1)));
    }

    assert_eq!(ENV_VISP_HOOK_EVENT, "VISP_HOOK_EVENT");
    assert_eq!(ENV_VISP_HOOK_SEQ, "VISP_HOOK_SEQ");
    assert_eq!(ENV_VISP_HOOK_SCHEMA, "VISP_HOOK_SCHEMA");
    assert_eq!(ENV_VISP_HOOK_RULE_ID, "VISP_HOOK_RULE_ID");
    assert_eq!(ENV_VISP_SESSION_ID, "VISP_SESSION_ID");
    assert_eq!(ENV_VISP_SESSION_SHORT_ID, "VISP_SESSION_SHORT_ID");
    assert_eq!(ENV_VISP_PROJECT_PATH, "VISP_PROJECT_PATH");
    assert_eq!(ENV_VISP_AGENT_NAME, "VISP_AGENT_NAME");
    assert_eq!(ENV_VISP_PARENT_SESSION_ID, "VISP_PARENT_SESSION_ID");
    assert_eq!(ENV_VISP_IN_HOOK, "VISP_IN_HOOK");
}

// 0a-7：未冻结事件不属契约模型（不可构造/不可反序列化）。
#[test]
fn unfrozen_events_are_unreachable() {
    for frozen in HookEventName::ALL {
        assert!(
            !matches!(
                frozen.as_str(),
                "PreCompact" | "PostCompact" | "SubagentStart"
            ),
            "未冻结事件不得进入模型"
        );
    }

    for bad in ["PreCompact", "PostCompact", "SubagentStart"] {
        let encoded = format!("\"{bad}\"");
        assert!(
            serde_json::from_str::<HookEventName>(&encoded).is_err(),
            "{bad} 不应能反序列化为事件名"
        );
    }

    // hook_event_name 指向未冻结事件 → 整事件反序列化失败。
    let mut value = sample_stop().to_value();
    value["hook_event_name"] = json!("PreCompact");
    assert!(HookEvent::from_value(value).is_err());

    // turn_id 已删除，不得作为字段被接受。
    let mut value = sample_stop().to_value();
    value["turn_id"] = json!("turn-1");
    assert!(
        HookEvent::from_value(value).is_err(),
        "turn_id 字段不得被接受"
    );
}
