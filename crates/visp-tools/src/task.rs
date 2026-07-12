use async_trait::async_trait;
use serde_json::json;

use visp_core::tool::{Tool, ToolContext, ToolResult};

/// Task tool: delegate a task to a specialized sub-agent.
/// Actual execution is intercepted by the agent loop (via tool name == "task" check),
/// so this `execute()` is never called in normal flow.
pub struct TaskTool;

#[async_trait]
impl Tool for TaskTool {
    fn name(&self) -> &str {
        "task"
    }

    fn description(&self) -> &str {
        "Delegate a task to a specialized sub-agent for better precision and cost efficiency. \
         The `prompt` field MUST be a detailed, self-contained task description (goal + relevant \
         context/paths + constraints + expected output) so the sub-agent can act autonomously \
         without re-analyzing the whole conversation. NEVER just copy the user's original request \
         into `prompt` - rewrite a focused task. See Delegation Guidelines in the system prompt."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "subagent_type": {
                    "type": "string",
                    "description": "Name of the sub-agent to invoke. Available sub-agents are listed in the system prompt."
                },
                "description": {
                    "type": "string",
                    "description": "Short summary of the task (used for display/logging)."
                },
                "prompt": {
                    "type": "string",
                    "description": "Detailed, self-contained task description for the sub-agent. Must include: the goal, relevant context/file paths, constraints, and what the sub-agent should return. Do NOT forward the user's original request - rewrite a focused task the sub-agent can act on autonomously."
                },
                "task_id": {
                    "type": "string",
                    "description": "Optional task ID for tracking"
                }
            },
            "required": ["subagent_type", "prompt"]
        })
    }

    async fn execute(&self, _arguments: serde_json::Value, _context: &ToolContext) -> ToolResult {
        unreachable!("task tool execution is handled by agent loop")
    }

    fn category(&self) -> &str {
        "agent"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_name() {
        let tool = TaskTool;
        assert_eq!(tool.name(), "task");
    }

    #[test]
    fn test_parameters_has_subagent_type() {
        let tool = TaskTool;
        let params = tool.parameters();
        let props = params["properties"].as_object().unwrap();
        assert!(props.contains_key("subagent_type"));
        assert_eq!(props["subagent_type"]["type"], "string");
    }

    #[test]
    fn test_parameters_has_description() {
        let tool = TaskTool;
        let params = tool.parameters();
        let props = params["properties"].as_object().unwrap();
        assert!(props.contains_key("description"));
        assert_eq!(props["description"]["type"], "string");
    }

    #[test]
    fn test_parameters_has_task_id_optional() {
        let tool = TaskTool;
        let params = tool.parameters();
        let props = params["properties"].as_object().unwrap();
        assert!(props.contains_key("task_id"));
        assert_eq!(props["task_id"]["type"], "string");
    }

    #[test]
    fn test_parameters_required_fields() {
        let tool = TaskTool;
        let params = tool.parameters();
        let required = params["required"].as_array().unwrap();
        let required_strs: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(required_strs.contains(&"subagent_type"));
        assert!(required_strs.contains(&"prompt"));
        assert!(!required_strs.contains(&"description"));
        assert_eq!(required.len(), 2);
    }

    #[test]
    fn test_parameters_has_prompt() {
        let tool = TaskTool;
        let params = tool.parameters();
        let props = params["properties"].as_object().unwrap();
        assert!(props.contains_key("prompt"));
        assert_eq!(props["prompt"]["type"], "string");
    }

    #[test]
    fn test_category() {
        let tool = TaskTool;
        assert_eq!(tool.category(), "agent");
    }
}
