use async_trait::async_trait;
use serde_json::json;

use visp_core::tool::{Tool, ToolContext, ToolResult};

/// Task 工具：启动一个子 Agent 处理复杂任务。
/// 实际执行由 agent loop 拦截（通过检查 tool name == "task"），
/// 此处的 `execute()` 不会在正常运行流程中被调用。
pub struct TaskTool;

#[async_trait]
impl Tool for TaskTool {
    fn name(&self) -> &str {
        "task"
    }

    fn description(&self) -> &str {
        "启动一个子 Agent 处理复杂任务。当任务适合某个专门的 Agent 时使用。"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "subagent_type": {
                    "type": "string",
                    "description": "子 Agent 的类型名称"
                },
                "description": {
                    "type": "string",
                    "description": "任务的详细描述"
                },
                "task_id": {
                    "type": "string",
                    "description": "任务 ID（可选，用于追踪）"
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
