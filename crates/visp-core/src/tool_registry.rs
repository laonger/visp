use crate::message::ToolDefinition;
use crate::tool::{Tool, ToolContext, ToolResult};

#[derive(Default)]
pub struct ToolRegistry {
    tools: Vec<Box<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }
}

impl ToolRegistry {
    pub fn register(&mut self, tool: Box<dyn Tool>) -> Result<(), String> {
        let name = tool.name().to_string();
        if self.tools.iter().any(|t| t.name() == name) {
            return Err(format!("duplicate tool name: {name}"));
        }
        self.tools.push(tool);
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools
            .iter()
            .find(|t| t.name() == name)
            .map(|t| t.as_ref())
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools
            .iter()
            .map(|t| ToolDefinition {
                name: t.name().to_string(),
                description: t.description().to_string(),
                parameters: t.parameters(),
                category: t.category().to_string(),
            })
            .collect()
    }

    pub fn names(&self) -> Vec<String> {
        self.tools.iter().map(|t| t.name().to_string()).collect()
    }

    pub async fn execute(
        &self,
        name: &str,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Option<ToolResult> {
        match self.get(name) {
            Some(tool) => Some(tool.execute(args, ctx).await),
            None => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::path::PathBuf;

    struct MockTool {
        name: String,
        description: String,
        parameters: serde_json::Value,
    }

    #[async_trait]
    impl Tool for MockTool {
        fn name(&self) -> &str {
            &self.name
        }
        fn description(&self) -> &str {
            &self.description
        }
        fn parameters(&self) -> serde_json::Value {
            self.parameters.clone()
        }
        async fn execute(&self, args: serde_json::Value, _ctx: &ToolContext) -> ToolResult {
            let result = format!("executed {} with {}", self.name, args);
            ToolResult::success(result)
        }
    }

    fn mock_tool(name: &str, desc: &str) -> Box<dyn Tool> {
        Box::new(MockTool {
            name: name.to_string(),
            description: desc.to_string(),
            parameters: serde_json::json!({}),
        })
    }

    fn test_context() -> ToolContext {
        ToolContext {
            working_dir: PathBuf::from("/tmp"),
            session_id: None,
        }
    }

    #[test]
    fn test_register_and_get() {
        let mut registry = ToolRegistry::new();
        registry.register(mock_tool("echo", "Echo tool")).unwrap();
        let tool = registry.get("echo");
        assert!(tool.is_some());
        assert_eq!(tool.unwrap().name(), "echo");
    }

    #[test]
    fn test_definitions() {
        let mut registry = ToolRegistry::new();
        registry.register(mock_tool("tool_a", "Tool A")).unwrap();
        registry.register(mock_tool("tool_b", "Tool B")).unwrap();

        let defs = registry.definitions();
        assert_eq!(defs.len(), 2);

        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"tool_a"));
        assert!(names.contains(&"tool_b"));
    }

    #[tokio::test]
    async fn test_execute() {
        let mut registry = ToolRegistry::new();
        registry
            .register(mock_tool("greet", "Greeting tool"))
            .unwrap();

        let ctx = test_context();
        let result = registry
            .execute("greet", serde_json::json!({"name": "world"}), &ctx)
            .await;
        assert!(result.is_some());
        let r = result.unwrap();
        assert!(!r.is_error);
        assert!(r.content.contains("executed greet"));
    }

    #[test]
    fn test_duplicate_name() {
        let mut registry = ToolRegistry::new();
        registry.register(mock_tool("echo", "Echo tool")).unwrap();
        let err = registry.register(mock_tool("echo", "Another echo"));
        assert!(err.is_err());
        assert!(err.unwrap_err().contains("duplicate tool name: echo"));
    }

    #[test]
    fn test_get_not_found() {
        let registry = ToolRegistry::new();
        let tool = registry.get("nonexistent");
        assert!(tool.is_none());
    }
}
