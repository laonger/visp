use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

use crate::agent_definition::PermissionRule;

/// 工具类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolType {
    /// 内置工具
    Builtin,
    /// MCP 工具
    Mcp,
    /// Agent 工具
    Agent,
    /// Skill 工具
    Skill,
}

/// 工具执行上下文
#[derive(Debug, Clone)]
pub struct ToolContext {
    /// 当前工作目录
    pub working_dir: PathBuf,
    /// 会话 ID（用于日志追踪和 bash 确认模式判断）
    pub session_id: Option<String>,
    /// 权限规则集（多 Agent 模式）
    pub permission_rules: Option<Arc<Vec<PermissionRule>>>,
}

#[cfg(test)]
mod tests_toolcontext {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_toolcontext_default_session_id() {
        let ctx = ToolContext {
            working_dir: PathBuf::from("/tmp"),
            session_id: None,
            permission_rules: None,
        };
        assert_eq!(ctx.session_id, None);
        assert_eq!(ctx.working_dir, Path::new("/tmp"));
    }
}

/// 工具执行结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    /// 结果内容（成功时为输出，失败时为错误描述）
    pub content: String,
    /// 是否为错误
    pub is_error: bool,
}

impl ToolResult {
    pub fn success(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self {
            content: message.into(),
            is_error: true,
        }
    }
}

/// 工具抽象 trait
/// 所有工具（内置 + MCP）都必须实现此 trait
#[async_trait]
pub trait Tool: Send + Sync {
    /// 工具名称
    fn name(&self) -> &str;

    /// 工具描述（给 LLM 看的说明）
    fn description(&self) -> &str;

    /// 参数定义（JSON Schema 格式，用于 LLM function calling）
    fn parameters(&self) -> serde_json::Value;

    /// 执行工具
    async fn execute(&self, arguments: serde_json::Value, context: &ToolContext) -> ToolResult;

    /// 是否需要用户确认才能执行（默认 false）
    fn requires_approval(&self) -> bool {
        false
    }

    /// 根据参数判断是否需要用户确认（默认调用 requires_approval()）
    /// 工具可覆盖此方法以根据执行参数动态决定是否需要审批
    fn requires_approval_for(&self, _arguments: &serde_json::Value) -> bool {
        self.requires_approval()
    }

    /// 工具分类（用于动态工具指南的分组展示）
    /// 默认分类为 "other"，各工具可根据功能覆盖
    fn category(&self) -> &str {
        "other"
    }

    /// 工具类型（用于区分工具的来源/种类）
    /// 默认返回 Builtin，各工具可根据需要覆盖
    fn tool_type(&self) -> ToolType {
        ToolType::Builtin
    }
}

#[cfg(test)]
mod tests_tool_approval {
    use super::*;

    struct NoApprovalTool;

    #[async_trait]
    impl Tool for NoApprovalTool {
        fn name(&self) -> &str {
            "no_approval"
        }
        fn description(&self) -> &str {
            "test tool"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        async fn execute(&self, _args: serde_json::Value, _ctx: &ToolContext) -> ToolResult {
            ToolResult::success("ok")
        }
    }

    #[test]
    fn test_tool_default_requires_approval() {
        let tool = NoApprovalTool;
        assert!(!tool.requires_approval());
    }

    #[test]
    fn test_requires_approval_for_default_matches_approval() {
        let tool = NoApprovalTool;
        let args = serde_json::json!({"url": "https://example.com"});
        assert_eq!(tool.requires_approval_for(&args), tool.requires_approval());
    }

    struct ApprovalForTool;

    #[async_trait]
    impl Tool for ApprovalForTool {
        fn name(&self) -> &str {
            "approval_for"
        }
        fn description(&self) -> &str {
            "test tool with dynamic approval"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        async fn execute(&self, _args: serde_json::Value, _ctx: &ToolContext) -> ToolResult {
            ToolResult::success("ok")
        }

        fn requires_approval_for(&self, arguments: &serde_json::Value) -> bool {
            // 模拟白名单：example.com 不需要确认，其他需要
            arguments
                .get("url")
                .and_then(|v| v.as_str())
                .map(|url| !url.contains("example.com"))
                .unwrap_or(true)
        }
    }

    #[test]
    fn test_requires_approval_for_override_allowed() {
        let tool = ApprovalForTool;
        let args = serde_json::json!({"url": "https://example.com/page"});
        assert!(!tool.requires_approval_for(&args));
    }

    #[test]
    fn test_requires_approval_for_override_denied() {
        let tool = ApprovalForTool;
        let args = serde_json::json!({"url": "https://evil.com"});
        assert!(tool.requires_approval_for(&args));
    }

    // ── category() tests ──────────────────────────────────────────────────────

    #[test]
    fn test_tool_default_category() {
        let tool = NoApprovalTool;
        assert_eq!(tool.category(), "other");
    }
}

#[cfg(test)]
mod tests_tool_type {
    use super::*;

    struct BuiltinMockTool;

    #[async_trait]
    impl Tool for BuiltinMockTool {
        fn name(&self) -> &str {
            "builtin_mock"
        }
        fn description(&self) -> &str {
            "Builtin mock tool"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        async fn execute(&self, _args: serde_json::Value, _ctx: &ToolContext) -> ToolResult {
            ToolResult::success("ok")
        }
    }

    struct McpMockTool;

    #[async_trait]
    impl Tool for McpMockTool {
        fn name(&self) -> &str {
            "mcp_mock"
        }
        fn description(&self) -> &str {
            "MCP mock tool"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        async fn execute(&self, _args: serde_json::Value, _ctx: &ToolContext) -> ToolResult {
            ToolResult::success("ok")
        }
        fn tool_type(&self) -> ToolType {
            ToolType::Mcp
        }
    }

    struct AgentMockTool;

    #[async_trait]
    impl Tool for AgentMockTool {
        fn name(&self) -> &str {
            "agent_mock"
        }
        fn description(&self) -> &str {
            "Agent mock tool"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        async fn execute(&self, _args: serde_json::Value, _ctx: &ToolContext) -> ToolResult {
            ToolResult::success("ok")
        }
        fn tool_type(&self) -> ToolType {
            ToolType::Agent
        }
    }

    struct SkillMockTool;

    #[async_trait]
    impl Tool for SkillMockTool {
        fn name(&self) -> &str {
            "skill_mock"
        }
        fn description(&self) -> &str {
            "Skill mock tool"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        async fn execute(&self, _args: serde_json::Value, _ctx: &ToolContext) -> ToolResult {
            ToolResult::success("ok")
        }
        fn tool_type(&self) -> ToolType {
            ToolType::Skill
        }
    }

    #[test]
    fn test_tool_type_default_is_builtin() {
        let tool = BuiltinMockTool;
        assert_eq!(tool.tool_type(), ToolType::Builtin);
    }

    #[test]
    fn test_tool_type_mcp_returns_mcp() {
        let tool = McpMockTool;
        assert_eq!(tool.tool_type(), ToolType::Mcp);
    }

    #[test]
    fn test_tool_type_agent_returns_agent() {
        let tool = AgentMockTool;
        assert_eq!(tool.tool_type(), ToolType::Agent);
    }

    #[test]
    fn test_tool_type_skill_returns_skill() {
        let tool = SkillMockTool;
        assert_eq!(tool.tool_type(), ToolType::Skill);
    }
}
