use async_trait::async_trait;
use serde_json::json;

use visp_core::tool::{Tool, ToolContext, ToolResult};

/// Task tool: delegate a complex task to a sub-agent.
/// Actual execution is intercepted by the agent loop (via tool name == "task" check),
/// so this `execute()` is never called in normal flow.
pub struct TaskTool;

#[async_trait]
impl Tool for TaskTool {
    fn name(&self) -> &str {
        "task"
    }

    fn description(&self) -> &str {
        "Delegate a complex or specialized task to a dedicated sub-agent. \
         Use this when the task requires focused expertise (e.g. reading code, \
         reviewing changes, testing) and can benefit from a separate agent."
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
                    "description": "Detailed description of the task to delegate"
                },
                "task_id": {
                    "type": "string",
                    "description": "Optional task ID for tracking"
                }
            },
            "required": ["subagent_type", "description"]
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
        assert!(required_strs.contains(&"description"));
        assert_eq!(required.len(), 2);
    }

    #[test]
    fn test_category() {
        let tool = TaskTool;
        assert_eq!(tool.category(), "agent");
    }
}
