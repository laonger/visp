//! MCP 工具适配器
//!
//! 将 MCP 服务器提供的工具（通过 McpSession 发现）包装为 visp 的 Tool trait 实现，
//! 使其可以无缝集成到 ToolRegistry 中，被 Agent 循环调用。

use std::sync::Arc;
use tokio::sync::Mutex;

use async_trait::async_trait;
use visp_core::tool::{Tool, ToolContext, ToolResult, ToolType};

use crate::client::{McpSession, McpToolDefinition, call_tool_result_to_text};
use crate::error::McpError;

/// 将 MCP 工具适配为 Tool trait 实现
///
/// 每个 McpToolAdapter 对应 MCP 服务器上的一个具体工具，
/// 执行时通过所属的 McpSession 发送 tools/call 请求。
pub struct McpToolAdapter {
    /// 工具名称（可能带前缀，如 "playwright_click"）
    name: String,
    /// 原始工具名称（不含前缀，用于 MCP 调用）
    original_name: String,
    /// 工具描述
    description: String,
    /// 参数定义（JSON Schema）
    parameters: serde_json::Value,
    /// 工具调用超时秒数
    timeout_secs: u64,
    /// 所属 MCP 会话
    session: Arc<Mutex<McpSession>>,
    /// 所属服务器名称（用于日志和错误消息）
    server_name: String,
}

impl std::fmt::Debug for McpToolAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpToolAdapter")
            .field("name", &self.name)
            .field("original_name", &self.original_name)
            .field("server_name", &self.server_name)
            .finish()
    }
}

impl McpToolAdapter {
    /// 创建新的 MCP 工具适配器
    pub fn new(
        name: String,
        original_name: String,
        description: String,
        parameters: serde_json::Value,
        timeout_secs: u64,
        session: Arc<Mutex<McpSession>>,
        server_name: String,
    ) -> Self {
        Self {
            name,
            original_name,
            description,
            parameters,
            timeout_secs,
            session,
            server_name,
        }
    }

    /// 从 McpToolDefinition 创建适配器
    pub fn from_definition(
        def: &McpToolDefinition,
        prefix: Option<&str>,
        timeout_secs: u64,
        session: Arc<Mutex<McpSession>>,
        server_name: &str,
    ) -> Self {
        let original_name = def.name.clone();
        let name = match prefix {
            Some(p) if !p.is_empty() => format!("{}{}", p, original_name),
            _ => original_name.clone(),
        };
        let description = def
            .description
            .clone()
            .unwrap_or_else(|| format!("MCP tool from {}", server_name));

        Self {
            name,
            original_name,
            description,
            parameters: def.input_schema.clone(),
            timeout_secs,
            session,
            server_name: server_name.to_string(),
        }
    }
}

#[async_trait]
impl Tool for McpToolAdapter {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters(&self) -> serde_json::Value {
        self.parameters.clone()
    }

    async fn execute(&self, arguments: serde_json::Value, _context: &ToolContext) -> ToolResult {
        let session = self.session.lock().await;

        if !session.is_connected() {
            return ToolResult::error(format!(
                "MCP server '{}' is not connected, tool '{}' unavailable",
                self.server_name, self.name
            ));
        }

        // 使用 tokio::time::timeout 控制调用超时
        let call_future = session.call_tool(&self.original_name, arguments);
        match tokio::time::timeout(
            std::time::Duration::from_secs(self.timeout_secs),
            call_future,
        )
        .await
        {
            Ok(Ok(result)) => {
                let text = call_tool_result_to_text(&result);
                let is_error = result.is_error.unwrap_or(false);
                if is_error {
                    ToolResult::error(text)
                } else {
                    ToolResult::success(text)
                }
            }
            Ok(Err(McpError::NotConnected(_))) => ToolResult::error(format!(
                "MCP server '{}' disconnected during tool call",
                self.server_name
            )),
            Ok(Err(e)) => ToolResult::error(format!("MCP tool '{}' call failed: {}", self.name, e)),
            Err(_elapsed) => {
                // 超时：session 标记为断开（为下次自动重连准备）
                // 注意：session 持有锁，我们无法在这里标记断开后重新连接
                // 返回超时错误，由 McpManager 的重连逻辑处理
                ToolResult::error(format!(
                    "MCP tool '{}' timed out after {}s",
                    self.name, self.timeout_secs
                ))
            }
        }
    }

    fn requires_approval(&self) -> bool {
        // 外部 MCP 工具默认需要用户审批
        true
    }

    fn category(&self) -> &str {
        "mcp"
    }

    fn tool_type(&self) -> ToolType {
        ToolType::Mcp
    }
}

/// 根据 McpToolDefinition 列表批量创建 McpToolAdapter
pub fn create_tool_adapters(
    tools: &[McpToolDefinition],
    prefix: Option<&str>,
    timeout_secs: u64,
    session: Arc<Mutex<McpSession>>,
    server_name: &str,
) -> Vec<Box<dyn Tool>> {
    tools
        .iter()
        .map(|def| {
            Box::new(McpToolAdapter::from_definition(
                def,
                prefix,
                timeout_secs,
                session.clone(),
                server_name,
            )) as Box<dyn Tool>
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::McpToolDefinition;
    use crate::config::{McpServerConfig, McpTransport};
    use std::collections::HashMap;

    fn create_mock_session() -> Arc<Mutex<McpSession>> {
        let config = McpServerConfig {
            name: "mock-server".into(),
            transport: McpTransport::Stdio {
                command: "echo".into(),
                args: vec![],
                env: HashMap::new(),
            },
            enabled: true,
            tool_prefix: None,
            tool_timeout_secs: 60,
        };
        Arc::new(Mutex::new(McpSession::new(&config)))
    }

    fn make_tool_def(name: &str, desc: &str) -> McpToolDefinition {
        McpToolDefinition {
            name: name.to_string(),
            description: if desc.is_empty() {
                None
            } else {
                Some(desc.to_string())
            },
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "param": {"type": "string"}
                }
            }),
        }
    }

    #[test]
    fn test_adapter_name_with_prefix() {
        let def = make_tool_def("click", "Click an element");
        let session = create_mock_session();
        let adapter =
            McpToolAdapter::from_definition(&def, Some("playwright_"), 60, session, "playwright");
        assert_eq!(adapter.name(), "playwright_click");
        assert_eq!(adapter.description(), "Click an element");
    }

    #[test]
    fn test_adapter_name_without_prefix() {
        let def = make_tool_def("click", "Click an element");
        let session = create_mock_session();
        let adapter = McpToolAdapter::from_definition(&def, None, 60, session, "playwright");
        assert_eq!(adapter.name(), "click");
    }

    #[test]
    fn test_adapter_name_with_empty_prefix() {
        let def = make_tool_def("click", "");
        let session = create_mock_session();
        let adapter = McpToolAdapter::from_definition(&def, Some(""), 60, session, "playwright");
        assert_eq!(adapter.name(), "click");
        assert!(adapter.description().contains("MCP tool from"));
    }

    #[test]
    fn test_adapter_fallback_description() {
        let def = make_tool_def("click", "");
        let session = create_mock_session();
        let adapter = McpToolAdapter::from_definition(&def, None, 60, session, "playwright");
        assert!(adapter.description().contains("MCP tool from playwright"));
    }

    #[test]
    fn test_adapter_parameters() {
        let def = make_tool_def("click", "Click");
        let session = create_mock_session();
        let adapter = McpToolAdapter::from_definition(&def, None, 60, session, "server");
        assert_eq!(
            adapter.parameters().get("type").and_then(|v| v.as_str()),
            Some("object")
        );
    }

    #[test]
    fn test_adapter_approval_default() {
        let def = make_tool_def("click", "Click");
        let session = create_mock_session();
        let adapter = McpToolAdapter::from_definition(&def, None, 60, session, "server");
        assert!(adapter.requires_approval());
    }

    #[test]
    fn test_adapter_category() {
        let def = make_tool_def("click", "Click");
        let session = create_mock_session();
        let adapter = McpToolAdapter::from_definition(&def, None, 60, session, "server");
        assert_eq!(adapter.category(), "mcp");
    }

    #[tokio::test]
    async fn test_adapter_execute_not_connected() {
        let def = make_tool_def("click", "Click");
        let session = create_mock_session();
        let adapter = McpToolAdapter::from_definition(&def, None, 60, session.clone(), "server");

        // session not connected → should return error
        {
            let sess = session.lock().await;
            assert!(!sess.is_connected());
        }

        let ctx = ToolContext {
            working_dir: std::path::PathBuf::from("/tmp"),
            session_id: None,
            permission_rules: None,
            global_tx: None,
            visp_trace_id: None,
            iter_span_w3c_id: None,
        };
        let result = adapter
            .execute(serde_json::json!({"param": "value"}), &ctx)
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("not connected"));
    }

    #[test]
    fn test_create_tool_adapters() {
        let defs = vec![
            make_tool_def("tool_a", "Tool A"),
            make_tool_def("tool_b", "Tool B"),
        ];
        let session = create_mock_session();
        let adapters = create_tool_adapters(&defs, Some("pre_"), 60, session, "server");
        assert_eq!(adapters.len(), 2);
        assert_eq!(adapters[0].name(), "pre_tool_a");
        assert_eq!(adapters[1].name(), "pre_tool_b");
    }

    #[test]
    fn test_create_tool_adapters_no_prefix() {
        let defs = vec![make_tool_def("tool_a", "Tool A")];
        let session = create_mock_session();
        let adapters = create_tool_adapters(&defs, None, 60, session, "server");
        assert_eq!(adapters.len(), 1);
        assert_eq!(adapters[0].name(), "tool_a");
    }
}
