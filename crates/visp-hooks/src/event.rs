//! 冻结事件模型（设计 §6.2）。
//!
//! 契约层单一来源：事件名、公共信封、逐事件载荷结构与取值域。
//! 仅含数据形态，不含任何匹配/执行/配置逻辑。
//!
//! # 未冻结取值域
//!
//! 设计 §6.2 仅显式冻结了 `source`/`origin`/`Stop.status`/`SessionEnd.exit_code`。
//! `AgentRunEnd.status`、`SubagentStop.status`、`PermissionRequest.kind`、
//! `PermissionResult.outcome` 的取值域**尚未冻结**，一期以 `String` 透传，
//! 待设计补齐后收紧（属契约加法/收紧，不影响字段集合）。

use serde::de::Error as DeError;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use crate::env::VISP_HOOK_SCHEMA;

/// 公共信封字段名（`seq` 可选；`hook_event_name` 由事件枚举提供）。
const ENVELOPE_KEYS: [&str; 7] = [
    "schema",
    "hook_event_name",
    "session_id",
    "cwd",
    "source",
    "origin",
    "seq",
];

/// 13 个冻结事件名（PascalCase，精确拼写）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HookEventName {
    SessionStart,
    UserPromptSubmit,
    AgentRunEnd,
    SubagentStop,
    Stop,
    StopFailure,
    ToolCallRequested,
    PreToolUse,
    PostToolUse,
    PostToolUseFailure,
    PermissionRequest,
    PermissionResult,
    SessionEnd,
}

impl HookEventName {
    /// 全部冻结事件（顺序与设计 §6.2 表一致）。
    pub const ALL: [HookEventName; 13] = [
        HookEventName::SessionStart,
        HookEventName::UserPromptSubmit,
        HookEventName::AgentRunEnd,
        HookEventName::SubagentStop,
        HookEventName::Stop,
        HookEventName::StopFailure,
        HookEventName::ToolCallRequested,
        HookEventName::PreToolUse,
        HookEventName::PostToolUse,
        HookEventName::PostToolUseFailure,
        HookEventName::PermissionRequest,
        HookEventName::PermissionResult,
        HookEventName::SessionEnd,
    ];

    /// 精确 PascalCase 事件名。
    pub const fn as_str(self) -> &'static str {
        match self {
            HookEventName::SessionStart => "SessionStart",
            HookEventName::UserPromptSubmit => "UserPromptSubmit",
            HookEventName::AgentRunEnd => "AgentRunEnd",
            HookEventName::SubagentStop => "SubagentStop",
            HookEventName::Stop => "Stop",
            HookEventName::StopFailure => "StopFailure",
            HookEventName::ToolCallRequested => "ToolCallRequested",
            HookEventName::PreToolUse => "PreToolUse",
            HookEventName::PostToolUse => "PostToolUse",
            HookEventName::PostToolUseFailure => "PostToolUseFailure",
            HookEventName::PermissionRequest => "PermissionRequest",
            HookEventName::PermissionResult => "PermissionResult",
            HookEventName::SessionEnd => "SessionEnd",
        }
    }
}

/// 会话来源：`{startup, resume}`（由绑定时刻 `history` 是否为空推导）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionSource {
    Startup,
    Resume,
}

/// 传输来源：`{tui, headless, other}`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    Tui,
    Headless,
    Other,
}

/// 会话级 `Stop` 状态：`{completed, cancelled}`（真错误走 `StopFailure`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StopStatus {
    Completed,
    Cancelled,
}

/// 公共信封：`schema`/`hook_event_name`/`session_id`/`cwd`/`source`/`origin`（+ 可选 `seq`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookContext {
    pub schema: u32,
    pub hook_event_name: HookEventName,
    pub session_id: String,
    pub cwd: String,
    pub source: SessionSource,
    pub origin: Origin,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
}

/// `SessionStart` 载荷。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionStartPayload {
    pub short_id: String,
    pub project_path: String,
    pub model: String,
    pub model_key: String,
    pub agent_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
}

/// `UserPromptSubmit` 载荷（`prompt` 默认脱敏）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserPromptSubmitPayload {
    pub prompt_chars: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
}

/// `AgentRunEnd` 载荷。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentRunEndPayload {
    pub status: String,
}

/// `SubagentStop` 载荷。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubagentStopPayload {
    pub parent_session_id: String,
    pub agent_name: String,
    pub status: String,
}

/// `Stop` 载荷。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StopPayload {
    pub status: StopStatus,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub tool_calls: u64,
}

/// `StopFailure` 载荷。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StopFailurePayload {
    pub code: String,
    pub message: String,
}

/// `ToolCallRequested` 载荷。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCallRequestedPayload {
    pub tool_use_id: String,
    pub tool_name: String,
}

/// `PreToolUse` 载荷。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreToolUsePayload {
    pub tool_use_id: String,
    pub tool_name: String,
    pub requires_approval: bool,
}

/// `PostToolUse` 载荷（`tool_input`/`tool_response` 默认脱敏）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PostToolUsePayload {
    pub tool_use_id: String,
    pub tool_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_input: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_response: Option<Value>,
}

/// `PostToolUseFailure` 载荷（`tool_input`/`tool_response` 默认脱敏）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PostToolUseFailurePayload {
    pub tool_use_id: String,
    pub tool_name: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_input: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_response: Option<Value>,
}

/// `PermissionRequest` 载荷（`message` 默认脱敏）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionRequestPayload {
    pub query_id: String,
    pub kind: String,
    pub options_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// `PermissionResult` 载荷。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionResultPayload {
    pub query_id: String,
    pub outcome: String,
    pub selected_index: i64,
}

/// `SessionEnd` 载荷（`exit_code`：删除会话为 `None`，正常关停为 `Some(0)`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionEndPayload {
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
}

/// 逐事件载荷联合。
#[derive(Debug, Clone, PartialEq)]
pub enum HookPayload {
    SessionStart(SessionStartPayload),
    UserPromptSubmit(UserPromptSubmitPayload),
    AgentRunEnd(AgentRunEndPayload),
    SubagentStop(SubagentStopPayload),
    Stop(StopPayload),
    StopFailure(StopFailurePayload),
    ToolCallRequested(ToolCallRequestedPayload),
    PreToolUse(PreToolUsePayload),
    PostToolUse(PostToolUsePayload),
    PostToolUseFailure(PostToolUseFailurePayload),
    PermissionRequest(PermissionRequestPayload),
    PermissionResult(PermissionResultPayload),
    SessionEnd(SessionEndPayload),
}

impl Serialize for HookPayload {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            HookPayload::SessionStart(p) => p.serialize(serializer),
            HookPayload::UserPromptSubmit(p) => p.serialize(serializer),
            HookPayload::AgentRunEnd(p) => p.serialize(serializer),
            HookPayload::SubagentStop(p) => p.serialize(serializer),
            HookPayload::Stop(p) => p.serialize(serializer),
            HookPayload::StopFailure(p) => p.serialize(serializer),
            HookPayload::ToolCallRequested(p) => p.serialize(serializer),
            HookPayload::PreToolUse(p) => p.serialize(serializer),
            HookPayload::PostToolUse(p) => p.serialize(serializer),
            HookPayload::PostToolUseFailure(p) => p.serialize(serializer),
            HookPayload::PermissionRequest(p) => p.serialize(serializer),
            HookPayload::PermissionResult(p) => p.serialize(serializer),
            HookPayload::SessionEnd(p) => p.serialize(serializer),
        }
    }
}

/// 一条完整 hook 事件 = 公共信封 + 事件专属载荷（线上为扁平 JSON 对象）。
#[derive(Debug, Clone, PartialEq)]
pub struct HookEvent {
    pub context: HookContext,
    pub payload: HookPayload,
}

impl HookEvent {
    /// 事件名（取自信封）。
    pub fn event_name(&self) -> HookEventName {
        self.context.hook_event_name
    }

    /// 序列化为扁平 JSON 对象。
    pub fn to_value(&self) -> Value {
        let context =
            serde_json::to_value(&self.context).expect("HookContext 必须序列化为 JSON 对象");
        let payload =
            serde_json::to_value(&self.payload).expect("HookPayload 必须序列化为 JSON 对象");
        let (Value::Object(mut merged), Value::Object(payload)) = (context, payload) else {
            unreachable!("信封与载荷均为 JSON 对象");
        };
        merged.extend(payload);
        Value::Object(merged)
    }

    /// 从扁平 JSON 对象反序列化；字段集合不被允许出现多余项。
    pub fn from_value(value: Value) -> Result<Self, serde_json::Error> {
        let object = value
            .as_object()
            .ok_or_else(|| DeError::custom("hook event 必须是 JSON 对象"))?;
        let name = object
            .get("hook_event_name")
            .cloned()
            .ok_or_else(|| DeError::custom("缺少 hook_event_name"))?;
        let hook_event_name: HookEventName = serde_json::from_value(name)?;
        let context: HookContext = serde_json::from_value(value.clone())?;
        if context.schema != VISP_HOOK_SCHEMA {
            return Err(DeError::custom(format!(
                "不支持的 schema 版本 {}（一期仅支持 {}）",
                context.schema, VISP_HOOK_SCHEMA
            )));
        }

        let mut rest = object.clone();
        for key in ENVELOPE_KEYS {
            rest.remove(key);
        }
        let rest = Value::Object(rest);

        let payload = match hook_event_name {
            HookEventName::SessionStart => HookPayload::SessionStart(serde_json::from_value(rest)?),
            HookEventName::UserPromptSubmit => {
                HookPayload::UserPromptSubmit(serde_json::from_value(rest)?)
            }
            HookEventName::AgentRunEnd => HookPayload::AgentRunEnd(serde_json::from_value(rest)?),
            HookEventName::SubagentStop => HookPayload::SubagentStop(serde_json::from_value(rest)?),
            HookEventName::Stop => HookPayload::Stop(serde_json::from_value(rest)?),
            HookEventName::StopFailure => HookPayload::StopFailure(serde_json::from_value(rest)?),
            HookEventName::ToolCallRequested => {
                HookPayload::ToolCallRequested(serde_json::from_value(rest)?)
            }
            HookEventName::PreToolUse => HookPayload::PreToolUse(serde_json::from_value(rest)?),
            HookEventName::PostToolUse => HookPayload::PostToolUse(serde_json::from_value(rest)?),
            HookEventName::PostToolUseFailure => {
                HookPayload::PostToolUseFailure(serde_json::from_value(rest)?)
            }
            HookEventName::PermissionRequest => {
                HookPayload::PermissionRequest(serde_json::from_value(rest)?)
            }
            HookEventName::PermissionResult => {
                HookPayload::PermissionResult(serde_json::from_value(rest)?)
            }
            HookEventName::SessionEnd => HookPayload::SessionEnd(serde_json::from_value(rest)?),
        };

        Ok(HookEvent { context, payload })
    }
}

impl Serialize for HookEvent {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.to_value().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for HookEvent {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        HookEvent::from_value(value).map_err(DeError::custom)
    }
}
