use std::collections::HashSet;
use std::sync::{Arc, RwLock};

use crate::error::CoreError;
use crate::message::ToolDefinition;
use crate::tool::{Tool, ToolContext, ToolResult};

/// 大小写不敏感的名称比较
fn name_eq(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

#[derive(Default)]
pub struct ToolRegistry {
    tools: RwLock<Vec<Arc<dyn Tool>>>,
    core_tool_names: RwLock<HashSet<String>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }
}

impl ToolRegistry {
    pub fn register(&self, tool: Arc<dyn Tool>) -> Result<(), CoreError> {
        let name = tool.name().to_string();
        let mut tools = self.tools.write().unwrap();
        if tools.iter().any(|t| name_eq(t.name(), &name)) {
            return Err(CoreError::Tool(format!("duplicate tool name: {name}")));
        }
        tools.push(tool);
        Ok(())
    }

    /// 注册 MCP 工具，跳过与核心工具名称冲突的工具
    pub fn register_mcp(&self, tool: Arc<dyn Tool>) -> Result<(), CoreError> {
        let name = tool.name().to_string();
        let core_names: HashSet<String> = self
            .core_tool_names
            .read()
            .unwrap()
            .iter()
            .map(|n| n.to_lowercase())
            .collect();
        if core_names.contains(&name.to_lowercase()) {
            tracing::warn!("MCP tool '{name}' conflicts with built-in tool, skipping");
            return Ok(());
        }
        drop(core_names); // release read lock before taking write lock

        let mut tools = self.tools.write().unwrap();
        if tools.iter().any(|t| name_eq(t.name(), &name)) {
            tracing::warn!("MCP tool '{name}' conflicts with another registered tool, skipping");
            return Ok(());
        }
        tools.push(tool);
        Ok(())
    }

    /// 锁定当前已注册的所有工具名为核心工具
    /// 之后通过 register_mcp 注册的工具不能覆盖这些名称
    pub fn seal_core_tools(&self) {
        let tools = self.tools.read().unwrap();
        let mut core_names = self.core_tool_names.write().unwrap();
        for t in tools.iter() {
            core_names.insert(t.name().to_string());
        }
    }

    /// 移除已注册的工具
    pub fn remove(&self, name: &str) -> Result<(), CoreError> {
        let mut tools = self.tools.write().unwrap();
        let pos = tools.iter().position(|t| name_eq(t.name(), name));
        match pos {
            Some(i) => {
                tools.remove(i);
                Ok(())
            }
            None => Err(CoreError::Tool(format!("tool '{name}' not found"))),
        }
    }

    /// 更新/替换同名工具
    pub fn update(&self, name: &str, tool: Arc<dyn Tool>) -> Result<(), CoreError> {
        let mut tools = self.tools.write().unwrap();
        let pos = tools.iter().position(|t| name_eq(t.name(), name));
        match pos {
            Some(i) => {
                tools[i] = tool;
                Ok(())
            }
            None => Err(CoreError::Tool(format!("tool '{name}' not found"))),
        }
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        let tools = self.tools.read().unwrap();
        tools.iter().find(|t| name_eq(t.name(), name)).cloned()
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        let tools = self.tools.read().unwrap();
        tools
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
        let tools = self.tools.read().unwrap();
        tools.iter().map(|t| t.name().to_string()).collect()
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

    fn mock_tool(name: &str, desc: &str) -> Arc<dyn Tool> {
        Arc::new(MockTool {
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
        let registry = ToolRegistry::new();
        registry.register(mock_tool("echo", "Echo tool")).unwrap();
        let tool = registry.get("echo");
        assert!(tool.is_some());
        assert_eq!(tool.unwrap().name(), "echo");
    }

    #[test]
    fn test_definitions() {
        let registry = ToolRegistry::new();
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
        let registry = ToolRegistry::new();
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
        let registry = ToolRegistry::new();
        registry.register(mock_tool("echo", "Echo tool")).unwrap();
        let err = registry.register(mock_tool("echo", "Another echo"));
        assert!(err.is_err());
        assert!(
            err.unwrap_err()
                .to_string()
                .contains("duplicate tool name: echo")
        );
    }

    #[test]
    fn test_get_not_found() {
        let registry = ToolRegistry::new();
        let tool = registry.get("nonexistent");
        assert!(tool.is_none());
    }

    // ── remove tests ───────────────────────────────────────────────────────

    #[test]
    fn test_remove_existing() {
        let registry = ToolRegistry::new();
        registry.register(mock_tool("echo", "Echo tool")).unwrap();
        assert!(registry.get("echo").is_some());

        registry.remove("echo").unwrap();
        assert!(registry.get("echo").is_none());
    }

    #[test]
    fn test_remove_not_found() {
        let registry = ToolRegistry::new();
        let err = registry.remove("nonexistent");
        assert!(err.is_err());
    }

    // ── update tests ───────────────────────────────────────────────────────

    #[test]
    fn test_update_existing() {
        let registry = ToolRegistry::new();
        registry.register(mock_tool("echo", "Echo tool")).unwrap();

        let updated = mock_tool("echo", "Updated echo");
        registry.update("echo", updated).unwrap();

        let tool = registry.get("echo").unwrap();
        assert_eq!(tool.description(), "Updated echo");
    }

    #[test]
    fn test_update_not_found() {
        let registry = ToolRegistry::new();
        let err = registry.update("nonexistent", mock_tool("x", "X"));
        assert!(err.is_err());
    }

    // ── seal + register_mcp tests ──────────────────────────────────────────

    #[test]
    fn test_seal_core_tools() {
        let registry = ToolRegistry::new();
        registry.register(mock_tool("core_a", "Core A")).unwrap();
        registry.register(mock_tool("core_b", "Core B")).unwrap();

        registry.seal_core_tools();

        // MCP tools with same names as core tools should be skipped
        registry
            .register_mcp(mock_tool("core_a", "MCP Core A"))
            .unwrap();
        // The tool should still be the original one
        let tool = registry.get("core_a").unwrap();
        assert_eq!(tool.description(), "Core A");
    }

    #[test]
    fn test_register_mcp_no_conflict() {
        let registry = ToolRegistry::new();
        registry.register(mock_tool("core_a", "Core A")).unwrap();
        registry.seal_core_tools();

        // MCP tool with non-core name should be registered
        registry
            .register_mcp(mock_tool("mcp_tool", "MCP Tool"))
            .unwrap();
        let tool = registry.get("mcp_tool").unwrap();
        assert_eq!(tool.description(), "MCP Tool");
    }

    #[test]
    fn test_register_mcp_after_seal_without_core_conflict() {
        let registry = ToolRegistry::new();
        registry.seal_core_tools(); // no core tools

        registry
            .register_mcp(mock_tool("any_tool", "Any tool"))
            .unwrap();
        assert!(registry.get("any_tool").is_some());
    }

    #[test]
    fn test_register_mcp_concurrent_reads() {
        let registry = ToolRegistry::new();
        registry.register(mock_tool("tool", "Tool")).unwrap();

        // definitions and names should work concurrently
        let defs = registry.definitions();
        let names = registry.names();
        assert_eq!(defs.len(), 1);
        assert_eq!(names, vec!["tool"]);
    }
}
