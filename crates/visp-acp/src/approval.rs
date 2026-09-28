//! 审批与提问桥接(Step 7):纯逻辑层。
//!
//! - 7a 工具审批:合成权限选项、ACP 结果 → visp `selected_index` 映射(V4)。
//! - 7b LLM 提问:elicitation form schema 构造、响应反解、降级路径。
//!
//! 交互层(经 SDK `cx.send_request` 发出请求并等待响应)在 Step 8 组装,
//! 依赖本模块的纯函数;任何提问路径都必须回填(V12,visp 侧无超时)。

use std::collections::BTreeMap;

use agent_client_protocol::schema::v1::{
    ElicitationContentValue, ElicitationPropertySchema, ElicitationSchema, PermissionOption,
    PermissionOptionKind, ToolCallId, ToolCallUpdate, ToolCallUpdateFields,
};
use visp_proto::visp::UserQuery;

use crate::translate::tool_kind;

// ===== 7a 工具审批 =====

/// 合成 ACP 权限选项(固定四项,与 V4 索引严格对齐,§6.5)。
pub fn permission_options() -> Vec<PermissionOption> {
    vec![
        PermissionOption::new("allow_once", "允许", PermissionOptionKind::AllowOnce),
        PermissionOption::new(
            "allow_always",
            "始终允许",
            PermissionOptionKind::AllowAlways,
        ),
        PermissionOption::new("reject_once", "拒绝", PermissionOptionKind::RejectOnce),
        PermissionOption::new(
            "reject_always",
            "永久拒绝",
            PermissionOptionKind::RejectAlways,
        ),
    ]
}

/// ACP 权限结果 → visp `selected_index`。
///
/// V4 映射:0=允许、2=始终允许、其他一律拒绝。`reject_always` 降级为
/// `reject_once`(`selected_index = 1`,已知差异 §13.5);未知值一律 deny。
pub fn map_selected_option(option_id: &str) -> i32 {
    match option_id {
        "allow_once" => 0,
        "allow_always" => 2,
        "reject_once" | "reject_always" => 1,
        _ => -1,
    }
}

/// 从审批 message("Allow tool: bash(cmd)?")提取工具名,失败回退 "tool"。
pub fn tool_name_from_message(message: &str) -> &str {
    let Some(rest) = message.strip_prefix("Allow tool: ") else {
        return "tool";
    };
    let name = rest.split('(').next().unwrap_or(rest).trim();
    if name.is_empty() { "tool" } else { name }
}

/// 合成 `session/request_permission` 的 tool_call(以 `query_id` 作 toolCallId,
/// §6.5 关联标识;标题用 visp 原始审批 message,kind 按工具名映射)。
pub fn synthetic_tool_call(query: &UserQuery) -> ToolCallUpdate {
    let name = tool_name_from_message(&query.message);
    ToolCallUpdate::new(
        ToolCallId::new(query.query_id.clone()),
        ToolCallUpdateFields::new()
            .title(query.message.clone())
            .name(name)
            .kind(tool_kind(name)),
    )
}

// ===== 7b LLM 提问(elicitation)=====

/// 提问路径:client 支持 `elicitation.form` 走 elicitation,否则降级(§6.4 步骤 7)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuestionPath {
    /// 发 `elicitation/create`(form 模式)
    Elicitation,
    /// 降级:问题原文作为文本 chunk 下发 + 立即回填 `-1`
    Downgrade,
}

/// 依据 client 能力选择提问路径(`elicitation.form`,A8)。
pub fn question_path(client_supports_form: bool) -> QuestionPath {
    if client_supports_form {
        QuestionPath::Elicitation
    } else {
        QuestionPath::Downgrade
    }
}

/// 构造 form schema:choice 枚举(值 = 选项原文,响应按文本反查索引);
/// `allow_other` 时附加自由文本字段(§6.4 步骤 7 三轮补充的映射约定)。
pub fn question_schema(query: &UserQuery) -> ElicitationSchema {
    let mut schema = ElicitationSchema::default();
    schema.title = Some(query.message.clone());

    let mut choice = StringPropertySchema::default();
    choice.title = Some("选项".into());
    choice.enum_values = Some(query.options.clone());
    schema
        .properties
        .insert("choice".into(), ElicitationPropertySchema::String(choice));

    if query.allow_other {
        let mut other = StringPropertySchema::default();
        other.title = Some("其他(自定义输入)".into());
        schema
            .properties
            .insert("other".into(), ElicitationPropertySchema::String(other));
    }
    schema
}

/// 反解 elicitation `accept` 响应 → visp `(selected_index, text)`。
///
/// 约定(§6.4 步骤 7):
/// - `other`(自由文本)非空 → **优先**视为自定义输入,回填 `(-1, text)`;
/// - `choice` 为选项原文 → 文本反查得 `selected_index = i`,回填 `(i, "")`;
/// - 两者都无法命中 → `(-1, "")`(视为未选择)。
pub fn map_elicitation_accept(
    content: &BTreeMap<String, ElicitationContentValue>,
    options: &[String],
) -> (i32, String) {
    if let Some(ElicitationContentValue::String(text)) = content.get("other")
        && !text.is_empty()
    {
        return (-1, text.clone());
    }
    if let Some(ElicitationContentValue::String(choice)) = content.get("choice")
        && let Some(idx) = options.iter().position(|o| o == choice)
    {
        return (idx as i32, String::new());
    }
    (-1, String::new())
}

/// `decline` / `cancel` → `(-1, "")`:视为用户未选择,回填后 LLM 可感知(必须回填,V12)。
pub fn map_elicitation_reject() -> (i32, String) {
    (-1, String::new())
}

/// 降级路径(client 不支持 `elicitation.form`):立即回填 `(-1, 说明文本)`,
/// 问题原文由适配器作为文本 chunk 下发(§6.4 步骤 7)。
pub fn map_downgrade(query: &UserQuery) -> (i32, String) {
    (
        -1,
        format!("[当前客户端不支持交互式提问,已自动跳过] {}", query.message),
    )
}

// ===== StringPropertySchema 需要 crate 内构造辅助(non_exhaustive,字段赋值) =====

use agent_client_protocol::schema::v1::StringPropertySchema;

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::schema::v1::ToolKind;

    fn query(options: Vec<String>, allow_other: bool) -> UserQuery {
        UserQuery {
            query_id: "q-9".into(),
            message: "Allow tool: bash(rm -rf /tmp/x)?".into(),
            session_id: "visp-1".into(),
            options,
            allow_other,
        }
    }

    // ===== 7a =====

    #[test]
    fn permission_options_four_kinds() {
        let opts = permission_options();
        assert_eq!(opts.len(), 4);
        let kinds: Vec<_> = opts.iter().map(|o| o.kind).collect();
        assert_eq!(
            kinds,
            vec![
                PermissionOptionKind::AllowOnce,
                PermissionOptionKind::AllowAlways,
                PermissionOptionKind::RejectOnce,
                PermissionOptionKind::RejectAlways,
            ]
        );
    }

    #[test]
    fn selected_option_index_mapping() {
        assert_eq!(map_selected_option("allow_once"), 0);
        assert_eq!(map_selected_option("allow_always"), 2);
        assert_eq!(map_selected_option("reject_once"), 1);
        assert_eq!(
            map_selected_option("reject_always"),
            1,
            "reject_always 降级为 reject_once(§13.5)"
        );
        assert_eq!(map_selected_option("bogus"), -1, "未知值一律 deny");
    }

    #[test]
    fn tool_name_extracted_from_message() {
        assert_eq!(tool_name_from_message("Allow tool: bash(rm)?"), "bash");
        assert_eq!(
            tool_name_from_message("Allow tool: read_file(a.rs)?"),
            "read_file"
        );
        assert_eq!(
            tool_name_from_message("weird message"),
            "tool",
            "解析失败回退"
        );
    }

    #[test]
    fn synthetic_tool_call_uses_query_id_and_kind() {
        let q = query(vec![], false);
        let tc = synthetic_tool_call(&q);
        assert_eq!(
            tc.tool_call_id.0.to_string(),
            "q-9",
            "toolCallId = query_id"
        );
        assert_eq!(tc.fields.title.as_deref(), Some(q.message.as_str()));
        assert_eq!(tc.fields.name.as_deref(), Some("bash"));
        assert_eq!(tc.fields.kind, Some(ToolKind::Execute));
    }

    // ===== 7b =====

    #[test]
    fn question_path_by_client_capability() {
        assert_eq!(question_path(true), QuestionPath::Elicitation);
        assert_eq!(question_path(false), QuestionPath::Downgrade);
    }

    #[test]
    fn question_schema_enum_uses_option_text_and_optional_other() {
        let q = query(vec!["方案 A".into(), "方案 B".into()], true);
        let schema = question_schema(&q);
        assert_eq!(schema.title.as_deref(), Some(q.message.as_str()));
        assert_eq!(schema.properties.len(), 2, "allow_other 应附自由文本字段");
        match schema.properties.get("choice") {
            Some(ElicitationPropertySchema::String(s)) => {
                assert_eq!(
                    s.enum_values.as_ref().unwrap(),
                    &vec!["方案 A".to_string(), "方案 B".to_string()]
                );
            }
            other => panic!("expected choice string property, got {other:?}"),
        }
        assert!(schema.properties.contains_key("other"));

        let q2 = query(vec!["方案 A".into()], false);
        assert_eq!(
            question_schema(&q2).properties.len(),
            1,
            "无 allow_other 不附自由文本"
        );
    }

    #[test]
    fn accept_content_maps_by_enum_text_reverse_lookup() {
        let options = vec!["方案 A".into(), "方案 B".into()];
        let mut content = BTreeMap::new();
        content.insert(
            "choice".into(),
            ElicitationContentValue::String("方案 B".into()),
        );
        assert_eq!(
            map_elicitation_accept(&content, &options),
            (1, String::new())
        );
    }

    #[test]
    fn free_text_takes_priority_over_enum() {
        let options = vec!["方案 A".into()];
        let mut content = BTreeMap::new();
        content.insert(
            "choice".into(),
            ElicitationContentValue::String("方案 A".into()),
        );
        content.insert(
            "other".into(),
            ElicitationContentValue::String("自定义说明".into()),
        );
        assert_eq!(
            map_elicitation_accept(&content, &options),
            (-1, "自定义说明".into()),
            "自由文本优先于枚举字段(§6.4 约定)"
        );
    }

    #[test]
    fn unmatched_accept_falls_back_to_deny() {
        let options = vec!["方案 A".into()];
        let mut content = BTreeMap::new();
        content.insert(
            "choice".into(),
            ElicitationContentValue::String("不存在的选项".into()),
        );
        assert_eq!(
            map_elicitation_accept(&content, &options),
            (-1, String::new())
        );
    }

    #[test]
    fn reject_and_downgrade_always_backfill() {
        assert_eq!(map_elicitation_reject(), (-1, String::new()));
        let q = query(vec!["A".into()], false);
        let (idx, text) = map_downgrade(&q);
        assert_eq!(idx, -1);
        assert!(text.contains(q.message.as_str()), "降级说明应含问题原文");
    }
}
