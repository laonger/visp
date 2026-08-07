use super::*;
use serde_json::json;
use std::path::PathBuf;

#[test]
fn test_name() {
    let tool = SkillTool::empty();
    assert_eq!(tool.name(), "skill");
}

#[test]
fn test_parameters_has_name() {
    let tool = SkillTool::empty();
    let params = tool.parameters();
    let props = params["properties"].as_object().unwrap();
    assert!(props.contains_key("name"));
    assert_eq!(props["name"]["type"], "string");
}

#[test]
fn test_parameters_required_contains_name() {
    let tool = SkillTool::empty();
    let params = tool.parameters();
    let required = params["required"].as_array().unwrap();
    let required_strs: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();
    assert!(required_strs.contains(&"name"));
}

#[test]
fn test_format_skill_output() {
    let output = format_skill_output("test-skill", "## Instructions\n\nDo this.");
    assert!(output.contains("<skill_content name=\"test-skill\">"));
    assert!(output.contains("# Skill: test-skill"));
    assert!(output.contains("Do this."));
    assert!(output.contains("</skill_content>"));
}

#[test]
fn test_category() {
    let tool = SkillTool::empty();
    assert_eq!(tool.category(), "agent");
}

#[tokio::test]
async fn test_execute_empty_name() {
    let tool = SkillTool::empty();
    let ctx = ToolContext {
        working_dir: PathBuf::from("/tmp"),
        session_id: None,
        permission_rules: None,
        global_tx: None,
        visp_trace_id: None,
        iter_span_w3c_id: None,
    };
    let result = tool.execute(json!({"name": ""}), &ctx).await;
    assert!(result.is_error);
    assert!(result.content.contains("required"));
}

#[tokio::test]
async fn test_execute_not_found() {
    let tool = SkillTool::empty();
    let ctx = ToolContext {
        working_dir: PathBuf::from("/nonexistent"),
        session_id: None,
        permission_rules: None,
        global_tx: None,
        visp_trace_id: None,
        iter_span_w3c_id: None,
    };
    let result = tool
        .execute(json!({"name": "nonexistent-skill"}), &ctx)
        .await;
    assert!(result.is_error);
    assert!(result.content.contains("not found"));
}
