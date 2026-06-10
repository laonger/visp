//! MCP 客户端会话封装
//!
//! 管理单个 MCP 服务器的连接生命周期，提供工具发现、调用和关闭接口。
//! 底层使用 rmcp crate 处理 MCP 协议握手和 JSON-RPC 通信。

use rmcp::model::{CallToolRequestParam, CallToolResult, RawContent, Tool as McpToolModel};
use rmcp::service::{RoleClient, RunningService, ServiceExt};

use crate::config::{McpServerConfig, McpTransport};
use crate::error::McpError;
use crate::transport::{create_sse_transport, create_stdio_transport};

/// MCP 服务器返回的工具定义
#[derive(Debug, Clone)]
pub struct McpToolDefinition {
    /// 工具名称（原始名称，不含前缀）
    pub name: String,
    /// 工具描述
    pub description: Option<String>,
    /// 输入参数 JSON Schema
    pub input_schema: serde_json::Value,
}

/// MCP 客户端会话
///
/// 封装单个 MCP 服务器的完整生命周期：
/// - `connect()` — 创建 transport 并建立连接（自动完成 initialize 握手）
/// - `list_tools()` — 获取服务器支持的工具列表
/// - `call_tool()` — 调用指定的工具
/// - `shutdown()` — 优雅关闭连接
pub struct McpSession {
    /// 服务器名称
    name: String,
    /// 配置（保留用于重连）
    config: McpServerConfig,
    /// rmcp 运行服务（连接建立后才有值）
    running_service: Option<RunningService<RoleClient, ()>>,
    /// 子进程句柄（stdio 模式，用于进程管理）
    child: Option<tokio::process::Child>,
    /// 传输是否已连接
    connected: bool,
}

impl std::fmt::Debug for McpSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpSession")
            .field("name", &self.name)
            .field("connected", &self.connected)
            .field("config", &self.config)
            .finish()
    }
}

impl McpSession {
    /// 创建新的 MCP 会话（不建立连接）
    pub fn new(config: &McpServerConfig) -> Self {
        Self {
            name: config.name.clone(),
            config: config.clone(),
            running_service: None,
            child: None,
            connected: false,
        }
    }

    /// 建立连接
    ///
    /// 根据配置创建 transport 并执行 MCP 握手（initialize + initialized）。
    pub async fn connect(&mut self) -> Result<(), McpError> {
        match &self.config.transport {
            McpTransport::Stdio { .. } => {
                let transport = create_stdio_transport(&self.config.transport)?;
                // 提取 child 进程（用于后续进程管理）
                self.child = None; // TokioChildProcess 内部管理进程，无法直接取出 Child
                let service = ().serve(transport).await.map_err(|e: std::io::Error| {
                    McpError::Transport(format!("failed to serve stdio transport: {}", e))
                })?;
                self.running_service = Some(service);
                self.connected = true;
                Ok(())
            }
            McpTransport::Sse { url, headers } => {
                let transport = create_sse_transport(url, headers).await?;
                let service = ()
                    .serve(transport)
                    .await
                    .map_err(|e| McpError::Transport(format!("SSE serve failed: {}", e)))?;
                self.running_service = Some(service);
                self.connected = true;
                Ok(())
            }
        }
    }

    /// 获取服务器支持的工具列表
    pub async fn list_tools(&self) -> Result<Vec<McpToolDefinition>, McpError> {
        if !self.connected {
            return Err(McpError::NotConnected(self.name.clone()));
        }

        let peer = self
            .running_service
            .as_ref()
            .ok_or_else(|| McpError::NotConnected(self.name.clone()))?;

        let tools = peer
            .list_all_tools()
            .await
            .map_err(|e| McpError::Protocol(format!("list_tools failed: {}", e)))?;

        Ok(tools.into_iter().map(mcp_tool_to_definition).collect())
    }

    /// 调用指定的工具
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: serde_json::Value,
    ) -> Result<CallToolResult, McpError> {
        if !self.connected {
            return Err(McpError::NotConnected(self.name.clone()));
        }

        let peer = self
            .running_service
            .as_ref()
            .ok_or_else(|| McpError::NotConnected(self.name.clone()))?;

        // 将 arguments 转为 JsonObject
        let args_map = match arguments {
            serde_json::Value::Object(map) => Some(map),
            serde_json::Value::Null => None,
            _ => {
                // 如果是非 Object 类型，尝试包装
                let mut map = serde_json::Map::new();
                map.insert("value".into(), arguments);
                Some(map)
            }
        };

        let params = CallToolRequestParam {
            name: std::borrow::Cow::Owned(name.to_owned()),
            arguments: args_map,
        };

        let result = peer
            .call_tool(params)
            .await
            .map_err(|e| McpError::Protocol(format!("call_tool failed: {}", e)))?;

        Ok(result)
    }

    /// 关闭连接
    ///
    /// 优雅关闭：cancel RunningService 后自动发送 shutdown。
    pub async fn shutdown(&mut self) {
        if let Some(service) = self.running_service.take() {
            let _ = service.cancel().await;
        }
        self.connected = false;
        self.child = None;
    }

    /// 检查是否已连接
    pub fn is_connected(&self) -> bool {
        self.connected
    }

    /// 获取服务器名称
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl Drop for McpSession {
    fn drop(&mut self) {
        if self.connected {
            // 无法在 Drop 中执行 async 操作，但 RunningService 被 drop 时会自动关闭连接
            tracing::debug!("McpSession '{}' dropped while connected", self.name);
        }
    }
}

/// 将 rmcp 的 Tool 模型转为 McpToolDefinition
fn mcp_tool_to_definition(tool: McpToolModel) -> McpToolDefinition {
    McpToolDefinition {
        name: tool.name.to_string(),
        description: if tool.description.is_empty() {
            None
        } else {
            Some(tool.description.to_string())
        },
        input_schema: serde_json::Value::Object(tool.input_schema.as_ref().clone()),
    }
}

/// 将 MCP CallToolResult 转为纯文本（用于 ToolResult）
pub fn call_tool_result_to_text(result: &CallToolResult) -> String {
    let mut texts: Vec<String> = Vec::new();
    for content in &result.content {
        match &content.raw {
            RawContent::Text(t) => texts.push(t.text.clone()),
            RawContent::Image(img) => {
                texts.push(format!(
                    "[Image: {} ({} bytes)]",
                    img.mime_type,
                    img.data.len()
                ));
            }
            RawContent::Resource(_) => {
                texts.push("[Resource]".into());
            }
        }
    }
    texts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::model::Content;

    /// 创建一个测试用的 stdio config（echo 命令，用于验证连接）
    fn test_stdio_config(name: &str) -> McpServerConfig {
        McpServerConfig {
            name: name.to_string(),
            transport: McpTransport::Stdio {
                command: "echo".into(),
                args: vec![],
                env: std::collections::HashMap::new(),
            },
            enabled: true,
            tool_prefix: None,
            tool_timeout_secs: 60,
        }
    }

    #[tokio::test]
    async fn test_session_new() {
        let config = test_stdio_config("test-server");
        let session = McpSession::new(&config);
        assert_eq!(session.name(), "test-server");
        assert!(!session.is_connected());
    }

    #[tokio::test]
    async fn test_session_connect_stdio() {
        let mut session = McpSession::new(&test_stdio_config("echo-test"));
        // echo 命令作为 MCP 服务器会失败（因为 stdin/stdout 格式不对），
        // 但 TokioChildProcess 本身能成功创建
        let result = session.connect().await;
        // echo is not a valid MCP server, so initialize will fail
        assert!(result.is_err(), "echo should fail as MCP server");
        assert!(!session.is_connected());
    }

    #[tokio::test]
    async fn test_session_connect_invalid_command() {
        let config = McpServerConfig {
            name: "invalid".into(),
            transport: McpTransport::Stdio {
                command: "nonexistent-command-xyz".into(),
                args: vec![],
                env: std::collections::HashMap::new(),
            },
            enabled: true,
            tool_prefix: None,
            tool_timeout_secs: 60,
        };
        let mut session = McpSession::new(&config);
        let result = session.connect().await;
        assert!(result.is_err());
        match result {
            Err(McpError::Transport(msg)) => {
                assert!(msg.contains("nonexistent"), "msg: {}", msg);
            }
            _ => panic!("expected Transport error"),
        }
    }

    #[test]
    fn test_mcp_tool_to_definition() {
        let tool = McpToolModel::new(
            "test_tool",
            "A test tool",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string"}
                }
            })
            .as_object()
            .cloned()
            .unwrap(),
        );
        let def = mcp_tool_to_definition(tool);
        assert_eq!(def.name, "test_tool");
        assert_eq!(def.description, Some("A test tool".into()));
        assert_eq!(
            def.input_schema.get("type").and_then(|v| v.as_str()),
            Some("object")
        );
    }

    #[test]
    fn test_mcp_tool_to_definition_empty_description() {
        let tool = McpToolModel::new(
            "no_desc",
            "",
            serde_json::json!({"type": "object"})
                .as_object()
                .cloned()
                .unwrap(),
        );
        let def = mcp_tool_to_definition(tool);
        assert_eq!(def.name, "no_desc");
        assert!(def.description.is_none());
    }

    #[test]
    fn test_call_tool_result_to_text() {
        let result = CallToolResult {
            content: vec![Content::text("hello"), Content::text("world")],
            is_error: Some(false),
        };
        let text = call_tool_result_to_text(&result);
        assert_eq!(text, "hello\nworld");
    }

    #[test]
    fn test_call_tool_result_to_text_empty() {
        let result = CallToolResult {
            content: vec![],
            is_error: Some(false),
        };
        let text = call_tool_result_to_text(&result);
        assert_eq!(text, "");
    }
}
