//! MCP 客户端会话封装
//!
//! 管理单个 MCP 服务器的连接生命周期，提供工具发现、调用和关闭接口。
//! 底层使用 rmcp crate 处理 MCP 协议握手和 JSON-RPC 通信（stdio/sse），
//! 或使用自定义 HTTP 客户端（get/http 传输）。

use rmcp::model::{CallToolRequestParam, CallToolResult, RawContent, Tool as McpToolModel};
use rmcp::service::{RoleClient, RunningService, ServiceExt};

use crate::config::{McpServerConfig, McpTransport};
use crate::error::McpError;
use crate::get_client::GetClient;
use crate::http_client::HttpPostClient;
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
    /// 内部会话
    session: Option<SessionInner>,
    /// 传输是否已连接
    connected: bool,
}

/// 内部会话变体
enum SessionInner {
    /// 标准 MCP 协议（stdio/sse），使用 rmcp
    StdioSse(RunningService<RoleClient, ()>),
    /// HTTP GET 简易 MCP，使用自定义 HTTP 客户端
    Get(GetClient),
    /// HTTP Streamable MCP（POST-only），使用自定义 HTTP 客户端
    Http(HttpPostClient),
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
            session: None,
            connected: false,
        }
    }

    /// 建立连接
    ///
    /// 根据配置创建 transport 并执行 MCP 握手（initialize + initialized）。
    /// - Stdio/Sse：使用 rmcp 创建 transport 并 serve（自动完成 initialize 握手）
    /// - Get：直接创建 HTTP GET 客户端，无需握手
    /// - Http：创建 HTTP POST 客户端并执行 initialize 握手
    pub async fn connect(&mut self) -> Result<(), McpError> {
        match &self.config.transport {
            McpTransport::Stdio { .. } => {
                let transport = create_stdio_transport(&self.config.transport)?;
                let service = ().serve(transport).await.map_err(|e: std::io::Error| {
                    McpError::Transport(format!("failed to serve stdio transport: {}", e))
                })?;
                self.session = Some(SessionInner::StdioSse(service));
                self.connected = true;
                Ok(())
            }
            McpTransport::Sse { url, headers } => {
                let transport = create_sse_transport(url, headers).await?;
                let service = ()
                    .serve(transport)
                    .await
                    .map_err(|e| McpError::Transport(format!("SSE serve failed: {}", e)))?;
                self.session = Some(SessionInner::StdioSse(service));
                self.connected = true;
                Ok(())
            }
            McpTransport::Get {
                url,
                headers,
                tools_endpoint,
                call_endpoint,
            } => {
                let client =
                    GetClient::new(&self.name, url, headers, tools_endpoint, call_endpoint)?;
                self.session = Some(SessionInner::Get(client));
                self.connected = true;
                Ok(())
            }
            McpTransport::Http { url, headers } => {
                let mut client = HttpPostClient::new(&self.name, url, headers)?;
                client.initialize().await?;
                self.session = Some(SessionInner::Http(client));
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

        match self
            .session
            .as_ref()
            .ok_or_else(|| McpError::NotConnected(self.name.clone()))?
        {
            SessionInner::StdioSse(peer) => {
                let tools = peer
                    .list_all_tools()
                    .await
                    .map_err(|e| McpError::Protocol(format!("list_tools failed: {}", e)))?;
                Ok(tools.into_iter().map(mcp_tool_to_definition).collect())
            }
            SessionInner::Get(client) => client.list_tools().await,
            SessionInner::Http(client) => client.list_tools().await,
        }
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

        match self
            .session
            .as_ref()
            .ok_or_else(|| McpError::NotConnected(self.name.clone()))?
        {
            SessionInner::StdioSse(peer) => {
                // 将 arguments 转为 JsonObject
                let args_map = match arguments {
                    serde_json::Value::Object(map) => Some(map),
                    serde_json::Value::Null => None,
                    _ => {
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
            SessionInner::Get(client) => client.call_tool(name, arguments).await,
            SessionInner::Http(client) => client.call_tool(name, arguments).await,
        }
    }

    /// 关闭连接
    ///
    /// - Stdio/Sse：cancel RunningService（自动发送 shutdown，kill 子进程）
    /// - Get/Http：标记为断开
    pub async fn shutdown(&mut self) {
        match self.session.take() {
            Some(SessionInner::StdioSse(service)) => {
                let _ = service.cancel().await;
            }
            Some(SessionInner::Get(mut client)) => {
                client.disconnect();
            }
            Some(SessionInner::Http(_)) => {
                // HttpPostClient 无需主动关闭
            }
            None => {}
        }
        self.connected = false;
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
pub(crate) fn call_tool_result_to_text(result: &CallToolResult) -> String {
    let mut texts: Vec<String> = Vec::new();
    for content in &result.content {
        match &content.raw {
            RawContent::Text(t) => texts.push(t.text.clone()),
            RawContent::Image(img) => {
                // Try to interpret image data as a UTF-8 file path
                if let Ok(path_str) = std::str::from_utf8(img.data.as_bytes()) {
                    let path = std::path::Path::new(path_str);
                    let image_exts = ["png", "jpg", "jpeg", "gif", "webp", "bmp", "ico"];
                    if path
                        .extension()
                        .and_then(|e| e.to_str())
                        .map(|e| image_exts.contains(&e.to_lowercase().as_str()))
                        .unwrap_or(false)
                        && path.exists()
                    {
                        texts.push(format!("<image: {}>", path.display()));
                        continue;
                    }
                }
                texts.push(format!("[Image: {} ({} bytes)]", img.mime_type, img.data.len()));
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

    fn test_get_config(name: &str) -> McpServerConfig {
        McpServerConfig {
            name: name.to_string(),
            transport: McpTransport::Get {
                url: "https://api.example.com".into(),
                headers: std::collections::HashMap::new(),
                tools_endpoint: "/tools".into(),
                call_endpoint: "/call".into(),
            },
            enabled: true,
            tool_prefix: None,
            tool_timeout_secs: 60,
        }
    }

    fn test_http_config(name: &str) -> McpServerConfig {
        McpServerConfig {
            name: name.to_string(),
            transport: McpTransport::Http {
                url: "https://api.example.com/mcp".into(),
                headers: std::collections::HashMap::new(),
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
    async fn test_session_new_get() {
        let config = test_get_config("get-server");
        let session = McpSession::new(&config);
        assert_eq!(session.name(), "get-server");
        assert!(!session.is_connected());
    }

    #[tokio::test]
    async fn test_session_new_http() {
        let config = test_http_config("http-server");
        let session = McpSession::new(&config);
        assert_eq!(session.name(), "http-server");
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

    #[tokio::test]
    async fn test_session_connect_get_not_reachable() {
        let mut session = McpSession::new(&test_get_config("get-test"));
        // 连接一个不存在的服务器，list_tools 会失败
        let result = session.connect().await;
        assert!(
            result.is_ok(),
            "get client should connect instantly: {:?}",
            result
        );
        assert!(session.is_connected());
        // list_tools 会因连不上服务器而失败
        let tools = session.list_tools().await;
        assert!(tools.is_err());
        session.shutdown().await;
        assert!(!session.is_connected());
    }

    #[tokio::test]
    async fn test_session_connect_http_not_reachable() {
        let mut session = McpSession::new(&test_http_config("http-test"));
        // 连接一个不存在的服务器，initialize 会失败
        let result = session.connect().await;
        assert!(result.is_err(), "http client should fail to initialize");
        assert!(!session.is_connected());
    }

    #[tokio::test]
    async fn test_session_shutdown_none() {
        let mut session = McpSession::new(&test_stdio_config("test"));
        // 未连接时 shutdown 不应 panic
        session.shutdown().await;
        assert!(!session.is_connected());
    }

    #[tokio::test]
    async fn test_session_shutdown_get_after_connect() {
        let mut session = McpSession::new(&test_get_config("get-test"));
        session.connect().await.unwrap();
        assert!(session.is_connected());
        session.shutdown().await;
        assert!(!session.is_connected());
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
