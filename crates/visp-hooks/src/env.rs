//! hook 执行时注入的环境变量名常量（设计 §6.3）。
//!
//! 契约层只冻结「名字与语义」，不涉及注入实现；约定使用 `ENV_` 前缀区分
//! 「变量名」与「变量值」（如 [`VISP_HOOK_SCHEMA`] 是 schema 版本号本身）。

/// hook 事件 JSON schema 版本（设计 §6.3；一期固定为 `1`）。
pub const VISP_HOOK_SCHEMA: u32 = 1;

/// 事件名（PascalCase）。
pub const ENV_VISP_HOOK_EVENT: &str = "VISP_HOOK_EVENT";
/// 单调序号（**可选**，面向复用器/严格序消费者）。
pub const ENV_VISP_HOOK_SEQ: &str = "VISP_HOOK_SEQ";
/// schema 版本（值为 [`VISP_HOOK_SCHEMA`]）。
pub const ENV_VISP_HOOK_SCHEMA: &str = "VISP_HOOK_SCHEMA";
/// 命中规则 id（内置为 `builtin:...`）。
pub const ENV_VISP_HOOK_RULE_ID: &str = "VISP_HOOK_RULE_ID";
/// 会话标识。
pub const ENV_VISP_SESSION_ID: &str = "VISP_SESSION_ID";
/// 会话短标识。
pub const ENV_VISP_SESSION_SHORT_ID: &str = "VISP_SESSION_SHORT_ID";
/// canonical 项目路径。
pub const ENV_VISP_PROJECT_PATH: &str = "VISP_PROJECT_PATH";
/// agent 名。
pub const ENV_VISP_AGENT_NAME: &str = "VISP_AGENT_NAME";
/// 父会话标识。
pub const ENV_VISP_PARENT_SESSION_ID: &str = "VISP_PARENT_SESSION_ID";
/// 防递归标记。
pub const ENV_VISP_IN_HOOK: &str = "VISP_IN_HOOK";
