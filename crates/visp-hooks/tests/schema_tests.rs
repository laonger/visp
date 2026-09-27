//! 0b 契约测试：JSON Schema 与 13 份 golden fixtures。
//!
//! 依据：设计 §6.2/§6.3、实施计划步骤 0b 测试用例表。

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use serde_json::Value;
use visp_hooks::{HookEvent, HookEventName};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn schema() -> Value {
    let path = manifest_dir().join("schema/visp-hook-event.v1.schema.json");
    let raw = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("读取 schema {} 失败: {e}", path.display()));
    serde_json::from_str(&raw).expect("schema 必须是合法 JSON")
}

fn fixture_path(file: &str) -> PathBuf {
    manifest_dir().join("tests/fixtures").join(file)
}

fn fixture(file: &str) -> Value {
    let path = fixture_path(file);
    let raw = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("读取 fixture {} 失败: {e}", path.display()));
    serde_json::from_str(&raw).expect("fixture 必须是合法 JSON")
}

/// 13 份 golden fixtures 及其顶层键的完整期望集合（信封 + 事件专属）。
const FIXTURES: [(&str, &[&str]); 13] = [
    (
        "session_start.json",
        &[
            "schema",
            "hook_event_name",
            "session_id",
            "cwd",
            "source",
            "origin",
            "seq",
            "short_id",
            "project_path",
            "model",
            "model_key",
            "agent_name",
        ],
    ),
    (
        "user_prompt_submit.json",
        &[
            "schema",
            "hook_event_name",
            "session_id",
            "cwd",
            "source",
            "origin",
            "seq",
            "prompt_chars",
        ],
    ),
    (
        "agent_run_end.json",
        &[
            "schema",
            "hook_event_name",
            "session_id",
            "cwd",
            "source",
            "origin",
            "seq",
            "status",
        ],
    ),
    (
        "subagent_stop.json",
        &[
            "schema",
            "hook_event_name",
            "session_id",
            "cwd",
            "source",
            "origin",
            "seq",
            "parent_session_id",
            "agent_name",
            "status",
        ],
    ),
    (
        "stop.json",
        &[
            "schema",
            "hook_event_name",
            "session_id",
            "cwd",
            "source",
            "origin",
            "seq",
            "status",
            "input_tokens",
            "output_tokens",
            "tool_calls",
        ],
    ),
    (
        "stop_failure.json",
        &[
            "schema",
            "hook_event_name",
            "session_id",
            "cwd",
            "source",
            "origin",
            "seq",
            "code",
            "message",
        ],
    ),
    (
        "tool_call_requested.json",
        &[
            "schema",
            "hook_event_name",
            "session_id",
            "cwd",
            "source",
            "origin",
            "seq",
            "tool_use_id",
            "tool_name",
        ],
    ),
    (
        "pre_tool_use.json",
        &[
            "schema",
            "hook_event_name",
            "session_id",
            "cwd",
            "source",
            "origin",
            "seq",
            "tool_use_id",
            "tool_name",
            "requires_approval",
        ],
    ),
    (
        "post_tool_use.json",
        &[
            "schema",
            "hook_event_name",
            "session_id",
            "cwd",
            "source",
            "origin",
            "seq",
            "tool_use_id",
            "tool_name",
        ],
    ),
    (
        "post_tool_use_failure.json",
        &[
            "schema",
            "hook_event_name",
            "session_id",
            "cwd",
            "source",
            "origin",
            "seq",
            "tool_use_id",
            "tool_name",
            "message",
        ],
    ),
    (
        "permission_request.json",
        &[
            "schema",
            "hook_event_name",
            "session_id",
            "cwd",
            "source",
            "origin",
            "seq",
            "query_id",
            "kind",
            "options_count",
        ],
    ),
    (
        "permission_result.json",
        &[
            "schema",
            "hook_event_name",
            "session_id",
            "cwd",
            "source",
            "origin",
            "seq",
            "query_id",
            "outcome",
            "selected_index",
        ],
    ),
    (
        "session_end.json",
        &[
            "schema",
            "hook_event_name",
            "session_id",
            "cwd",
            "source",
            "origin",
            "seq",
            "reason",
            "exit_code",
        ],
    ),
];

fn sorted(mut keys: Vec<String>) -> Vec<String> {
    keys.sort();
    keys
}

// 0b-1：13 份 golden fixture 各自通过 JSON Schema 校验。
#[test]
fn golden_fixtures_validate_against_schema() {
    let validator = jsonschema::validator_for(&schema()).expect("schema 可编译");
    for (file, _) in FIXTURES {
        let value = fixture(file);
        assert!(
            validator.is_valid(&value),
            "{file} 未通过 schema 校验: {:?}",
            validator.validate(&value)
        );
    }
}

// 0b-2：fixture → 契约模型 → 序列化，与原文稳定一致（幂等）。
#[test]
fn fixtures_round_trip_idempotently() {
    for (file, _) in FIXTURES {
        let original = fixture(file);
        let event = HookEvent::from_value(original.clone())
            .unwrap_or_else(|e| panic!("{file} 反序列化失败: {e}"));
        assert_eq!(event.to_value(), original, "{file} 回环后与原 JSON 不一致");

        let via_serde = serde_json::to_value(&event).expect("事件可序列化");
        assert_eq!(via_serde, original, "{file} 经 serde 序列化后不一致");
    }
}

// 0b-3：schema 顶层事件名枚举恰好覆盖 13 个冻结事件（无多余、无缺失）。
#[test]
fn schema_covers_exactly_the_frozen_events() {
    let schema = schema();
    let entries = schema["oneOf"].as_array().expect("schema 顶层应有 oneOf");
    assert_eq!(entries.len(), 13, "oneOf 条目数应为 13");

    let declared: BTreeSet<String> = entries
        .iter()
        .map(|entry| {
            entry["properties"]["hook_event_name"]["const"]
                .as_str()
                .expect("每个事件应有 hook_event_name const")
                .to_string()
        })
        .collect();

    let frozen: BTreeSet<String> = HookEventName::ALL
        .iter()
        .map(|name| name.as_str().to_string())
        .collect();

    assert_eq!(declared, frozen, "schema 事件名集合与冻结契约不一致");
}

// 0b-4：非法 fixture（缺 hook_event_name / source 越界 / schema≠1）被 schema 拒绝。
#[test]
fn invalid_fixtures_are_rejected() {
    let validator = jsonschema::validator_for(&schema()).expect("schema 可编译");
    let base = fixture("stop.json");

    let mut missing_name = base.clone();
    missing_name
        .as_object_mut()
        .unwrap()
        .remove("hook_event_name");
    assert!(
        !validator.is_valid(&missing_name),
        "缺 hook_event_name 应被拒绝"
    );

    let mut bad_source = base.clone();
    bad_source["source"] = Value::String("bogus".to_string());
    assert!(!validator.is_valid(&bad_source), "source 越界应被拒绝");

    let mut bad_schema = base.clone();
    bad_schema["schema"] = Value::from(2);
    assert!(!validator.is_valid(&bad_schema), "schema≠1 应被拒绝");

    // 与模型层一致：三种非法形态同样无法反序列化。
    assert!(HookEvent::from_value(missing_name).is_err());
    assert!(HookEvent::from_value(bad_source).is_err());
    assert!(HookEvent::from_value(bad_schema).is_err());

    // 4 个收紧取值域：schema 与模型层都必须拒绝越界值。
    let domain_cases: [(&str, &str, &str); 4] = [
        ("agent_run_end.json", "status", "bogus"),
        ("subagent_stop.json", "status", "bogus"),
        ("permission_request.json", "kind", "bogus"),
        ("permission_result.json", "outcome", "bogus"),
    ];
    for (file, field, bad) in domain_cases {
        let mut value = fixture(file);
        value[field] = Value::String(bad.to_string());
        assert!(
            !validator.is_valid(&value),
            "{file} 的 {field}={bad} 应被 schema 拒绝"
        );
        assert!(
            HookEvent::from_value(value).is_err(),
            "{file} 的 {field}={bad} 应被模型拒绝"
        );
    }
}

// 0b-6：schema 对 4 个收紧取值域的 enum 集合与设计 §6.2 精确一致。
#[test]
fn schema_declares_exact_frozen_value_domains() {
    let schema = schema();
    let properties_of = |event: &str| {
        schema["oneOf"]
            .as_array()
            .expect("oneOf")
            .iter()
            .find(|entry| entry["properties"]["hook_event_name"]["const"] == event)
            .unwrap_or_else(|| panic!("缺少事件 {event}"))["properties"]
            .clone()
    };
    let enum_of = |event: &str, field: &str| -> Vec<String> {
        properties_of(event)[field]["enum"]
            .as_array()
            .unwrap_or_else(|| panic!("{event}.{field} 应为 enum"))
            .iter()
            .map(|v| v.as_str().expect("enum 值应为字符串").to_string())
            .collect()
    };

    assert_eq!(
        enum_of("AgentRunEnd", "status"),
        ["completed", "cancelled", "failed"]
    );
    assert_eq!(
        enum_of("SubagentStop", "status"),
        ["completed", "cancelled", "failed"]
    );
    assert_eq!(
        enum_of("PermissionRequest", "kind"),
        ["approval", "question"]
    );
    assert_eq!(
        enum_of("PermissionResult", "outcome"),
        ["selected", "cancelled"]
    );
}

// 0b-5：每个 fixture 的必填公共字段与事件专属字段均存在（结构化断言）。
#[test]
fn fixtures_have_expected_field_sets() {
    for (file, expected) in FIXTURES {
        let actual = sorted(
            fixture(file)
                .as_object()
                .expect("fixture 顶层必须是对象")
                .keys()
                .cloned()
                .collect(),
        );
        assert_eq!(
            actual,
            sorted(expected.iter().map(|k| k.to_string()).collect()),
            "{file} 字段集合不符"
        );
    }
}
