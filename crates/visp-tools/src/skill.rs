use async_trait::async_trait;
use serde_json::json;
use std::path::PathBuf;

use visp_core::tool::{Tool, ToolContext, ToolResult};

/// Skill tool: load a skill's detailed instructions.
///
/// Construct with [`SkillTool::new`] to embed the available-skills listing
/// (built-in + project + global) directly into the tool description, so the
/// LLM can discover skills without a system-prompt block.
pub struct SkillTool {
    /// Full description including the available-skills listing.
    description: String,
}

impl SkillTool {
    /// Build a SkillTool whose `description()` includes the list of
    /// available skills for `project_path`.
    pub fn new(project_path: &std::path::Path) -> Self {
        let skills_listing = visp_core::session::load_skills(project_path);
        let description = if skills_listing.is_empty() {
            "Load a specialized skill's detailed instructions into the current conversation. \
             No skills are currently available."
                .to_string()
        } else {
            format!(
                "Load a specialized skill's detailed instructions into the current conversation. \
                 Use this tool to inject the skill's instructions and workflow guidance.\n\
                 \n{skills_listing}"
            )
        };
        Self { description }
    }

    /// Construct a SkillTool with no skills listing (for tests).
    #[cfg(test)]
    pub fn empty() -> Self {
        Self {
            description: "Load a specialized skill's detailed instructions into the current \
                          conversation. No skills are currently available."
                .to_string(),
        }
    }
}

#[async_trait]
impl Tool for SkillTool {
    fn name(&self) -> &str {
        "skill"
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "The name of the skill from the available skills list"
                }
            },
            "required": ["name"]
        })
    }

    async fn execute(&self, arguments: serde_json::Value, context: &ToolContext) -> ToolResult {
        let name = arguments
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if name.is_empty() {
            return ToolResult::error("Skill name is required");
        }

        // 1. Try built-in skills
        if let Some(skill) = visp_core::skill::find_builtin_skill(&name) {
            return ToolResult::success(format_skill_output(skill.name, skill.content));
        }

        // 2. Try project filesystem (.visp/skills/<name>/SKILL.md)
        let project_skill = context
            .working_dir
            .join(".visp")
            .join("skills")
            .join(&name)
            .join("SKILL.md");
        if let Ok(content) = tokio::fs::read_to_string(&project_skill).await {
            let body = visp_core::session::strip_frontmatter(&content);
            return ToolResult::success(format_skill_output(&name, body));
        }

        // 3. Try global filesystem (~/.config/visp/skills/<name>/SKILL.md)
        if let Ok(home) = std::env::var("HOME") {
            let global_skill = PathBuf::from(home)
                .join(".config")
                .join("visp")
                .join("skills")
                .join(&name)
                .join("SKILL.md");
            if let Ok(content) = tokio::fs::read_to_string(&global_skill).await {
                let body = visp_core::session::strip_frontmatter(&content);
                return ToolResult::success(format_skill_output(&name, body));
            }
        }

        ToolResult::error(format!("Skill '{}' not found", name))
    }

    fn category(&self) -> &str {
        "agent"
    }
}

fn format_skill_output(name: &str, content: &str) -> String {
    format!(
        "<skill_content name=\"{}\">\n\
         # Skill: {}\n\n\
         {}\n\
         </skill_content>",
        name,
        name,
        content.trim()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
}
