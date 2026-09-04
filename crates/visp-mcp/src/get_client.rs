//! HTTP GET MCP 客户端
//!
//! 用于连接只支持 GET 请求的简易 MCP 服务器（如 context7）。
//! 不经过 MCP 协议握手，直接通过 GET 请求发现工具和调用工具。
//!
//! - 工具发现：`GET {url}{tools_endpoint}`（默认 `/tools`）
//! - 工具调用：`GET {url}{call_endpoint}?name={tool_name}&{query_params}`（默认 `/call`）

use std::collections::HashMap;

use rmcp::model::{CallToolResult, ContentBlock};
use serde::Deserialize;

use crate::client::McpToolDefinition;
use crate::error::McpError;

/// GET 模式的 MCP 客户端
///
/// 每个实例对应一个 MCP 服务器配置，通过 HTTP GET 请求与服务器通信。
/// 连接后通过 `list_tools()` 获取工具列表，通过 `call_tool()` 调用工具。
#[derive(Debug, Clone)]
pub struct GetClient {
    /// 基础 URL
    base_url: String,
    /// HTTP 请求头
    #[expect(dead_code)]
    headers: HashMap<String, String>,
    /// 工具列表端点
    tools_endpoint: String,
    /// 工具调用端点
    call_endpoint: String,
    /// reqwest HTTP 客户端
    http_client: reqwest::Client,
    /// 服务器名称（用于错误消息）
    server_name: String,
    /// 是否已连接（对于 GET 模式，只要配置有效就算是已连接）
    connected: bool,
}

/// 工具列表响应格式
#[derive(Debug, Deserialize)]
struct ListToolsResponse {
    tools: Vec<ToolEntry>,
}

/// 单个工具条目响应格式
#[derive(Debug, Deserialize)]
struct ToolEntry {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default, rename = "inputSchema")]
    input_schema: Option<serde_json::Value>,
}

/// 工具调用响应格式
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CallToolResponse {
    #[serde(default)]
    content: Vec<ContentEntry>,
    #[serde(default)]
    is_error: Option<bool>,
}

/// 内容条目响应格式
#[derive(Debug, Deserialize)]
struct ContentEntry {
    #[serde(rename = "type")]
    _type: Option<String>,
    #[serde(default)]
    text: String,
}

impl GetClient {
    /// 创建新的 GET 客户端
    pub fn new(
        server_name: &str,
        base_url: &str,
        headers: &HashMap<String, String>,
        tools_endpoint: &str,
        call_endpoint: &str,
    ) -> Result<Self, McpError> {
        // 构造 reqwest 客户端
        let mut client_builder = reqwest::Client::builder();
        let mut default_headers = reqwest::header::HeaderMap::new();

        for (key, value) in headers {
            let name = reqwest::header::HeaderName::from_bytes(key.as_bytes())
                .map_err(|e| McpError::Transport(format!("invalid header name '{key}': {e}")))?;
            let val = reqwest::header::HeaderValue::from_str(value).map_err(|e| {
                McpError::Transport(format!("invalid header value for '{key}': {e}"))
            })?;
            default_headers.insert(name, val);
        }

        if !default_headers.is_empty() {
            client_builder = client_builder.default_headers(default_headers);
        }

        let http_client = client_builder
            .build()
            .map_err(|e| McpError::Transport(format!("failed to build reqwest client: {e}")))?;

        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            headers: headers.clone(),
            tools_endpoint: tools_endpoint.to_string(),
            call_endpoint: call_endpoint.to_string(),
            http_client,
            server_name: server_name.to_string(),
            connected: true, // GET 模式建立即连接
        })
    }

    /// 获取工具列表
    ///
    /// 发送 `GET {base_url}{tools_endpoint}` 请求，解析返回的工具列表。
    pub async fn list_tools(&self) -> Result<Vec<McpToolDefinition>, McpError> {
        let url = format!("{}{}", self.base_url, self.tools_endpoint);

        let response = self
            .http_client
            .get(&url)
            .send()
            .await
            .map_err(|e| McpError::Transport(format!("GET '{url}' failed: {e}")))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(McpError::Transport(format!(
                "GET '{url}' returned {status}: {body}"
            )));
        }

        let text = response.text().await.map_err(|e| {
            McpError::Transport(format!("read response body from '{url}' failed: {e}"))
        })?;

        // 尝试解析为 ListToolsResponse（带 tools 数组的格式）
        if let Ok(parsed) = serde_json::from_str::<ListToolsResponse>(&text) {
            return Ok(parsed
                .tools
                .into_iter()
                .map(|t| {
                    let input_schema = t
                        .input_schema
                        .unwrap_or_else(|| serde_json::json!({"type": "object"}));
                    McpToolDefinition {
                        name: t.name,
                        description: if t.description.is_empty() {
                            None
                        } else {
                            Some(t.description)
                        },
                        input_schema,
                    }
                })
                .collect());
        }

        // 尝试解析为直接的工具数组
        if let Ok(tools) = serde_json::from_str::<Vec<ToolEntry>>(&text) {
            return Ok(tools
                .into_iter()
                .map(|t| {
                    let input_schema = t
                        .input_schema
                        .unwrap_or_else(|| serde_json::json!({"type": "object"}));
                    McpToolDefinition {
                        name: t.name,
                        description: if t.description.is_empty() {
                            None
                        } else {
                            Some(t.description)
                        },
                        input_schema,
                    }
                })
                .collect());
        }

        Err(McpError::Protocol(format!(
            "unrecognized tools list response format from '{url}': {text:.200}",
        )))
    }

    /// 调用工具
    ///
    /// 发送 `GET {base_url}{call_endpoint}?name={tool_name}&{query_params}` 请求。
    /// 参数中的 JSON 对象会被展开为 URL 查询参数（仅支持单层展开）。
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: serde_json::Value,
    ) -> Result<CallToolResult, McpError> {
        let mut url = format!("{}{}", self.base_url, self.call_endpoint);

        // 构建查询参数
        let mut query_params: Vec<(String, String)> = Vec::new();
        query_params.push(("name".into(), name.to_string()));

        if let serde_json::Value::Object(map) = &arguments {
            for (key, value) in map {
                let val_str = match value {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Number(n) => n.to_string(),
                    serde_json::Value::Bool(b) => b.to_string(),
                    serde_json::Value::Null => "null".to_string(),
                    // 嵌套对象/数组序列化为 JSON 字符串
                    other => other.to_string(),
                };
                query_params.push((key.clone(), val_str));
            }
        }

        // 拼接查询参数
        let query_string = query_params
            .iter()
            .map(|(k, v)| format!("{}={}", urlencoding(k), urlencoding(v)))
            .collect::<Vec<_>>()
            .join("&");
        url = format!("{url}?{query_string}");

        let response = self
            .http_client
            .get(&url)
            .send()
            .await
            .map_err(|e| McpError::Transport(format!("GET '{url}' failed: {e}")))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(McpError::Transport(format!(
                "GET '{}' returned {status}: {body}",
                self.call_endpoint
            )));
        }

        let text = response
            .text()
            .await
            .map_err(|e| McpError::Transport(format!("read response body failed: {e}")))?;

        // 尝试解析为 CallToolResponse 格式
        if let Ok(parsed) = serde_json::from_str::<CallToolResponse>(&text) {
            let contents: Vec<ContentBlock> = parsed
                .content
                .into_iter()
                .map(|c| ContentBlock::text(c.text))
                .collect();
            return Ok(if parsed.is_error.unwrap_or(false) {
                CallToolResult::error(contents)
            } else {
                CallToolResult::success(contents)
            });
        }

        // 退回纯文本模式：将整个响应体作为文本内容
        Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
    }

    /// 检查是否已连接
    pub fn is_connected(&self) -> bool {
        self.connected
    }

    /// 断开连接
    pub fn disconnect(&mut self) {
        self.connected = false;
    }

    /// 获取服务器名称
    pub fn server_name(&self) -> &str {
        &self.server_name
    }
}

/// 简单的 URL 编码（避免添加额外依赖）
fn urlencoding(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                result.push(byte as char);
            }
            b' ' => result.push_str("%20"),
            _ => {
                result.push_str(&format!("%{:02X}", byte));
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── GetClient::new ──────────────────────────────────────────────────────────

    #[test]
    fn test_get_client_new() {
        let client = GetClient::new(
            "test-server",
            "https://api.example.com/v1",
            &HashMap::new(),
            "/tools",
            "/call",
        )
        .unwrap();
        assert!(client.is_connected());
        assert_eq!(client.server_name(), "test-server");
    }

    #[test]
    fn test_get_client_new_trailing_slash() {
        let client = GetClient::new(
            "test",
            "https://api.example.com/v1/",
            &HashMap::new(),
            "/tools",
            "/call",
        )
        .unwrap();
        // 尾部斜杠被去掉，URL 拼接后不应有双斜杠
        assert!(client.is_connected());
    }

    #[test]
    fn test_get_client_new_with_headers() {
        let mut headers = HashMap::new();
        headers.insert("Authorization".into(), "Bearer sk-xxx".into());
        let client = GetClient::new(
            "auth-server",
            "https://api.example.com",
            &headers,
            "/tools",
            "/call",
        )
        .unwrap();
        assert!(client.is_connected());
    }

    #[test]
    fn test_get_client_invalid_header() {
        let mut headers = HashMap::new();
        headers.insert("invalid\nheader".into(), "value".into());
        let result = GetClient::new(
            "bad",
            "https://api.example.com",
            &headers,
            "/tools",
            "/call",
        );
        assert!(result.is_err());
    }

    // ── list_tools response parsing ─────────────────────────────────────────────

    #[test]
    fn test_parse_tools_list_response() {
        let json = r#"{
            "tools": [
                {"name": "search", "description": "Search the web", "inputSchema": {"type": "object", "properties": {"q": {"type": "string"}}}},
                {"name": "fetch", "description": "Fetch a URL", "inputSchema": {"type": "object"}}
            ]
        }"#;
        let parsed: ListToolsResponse = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.tools.len(), 2);
        assert_eq!(parsed.tools[0].name, "search");
        assert_eq!(parsed.tools[0].description, "Search the web");
        assert!(parsed.tools[0].input_schema.is_some());
        assert_eq!(parsed.tools[1].name, "fetch");
    }

    #[test]
    fn test_parse_tools_list_response_minimal() {
        // 没有 inputSchema 和 description 的情况
        let json = r#"{"tools": [{"name": "ping"}]}"#;
        let parsed: ListToolsResponse = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.tools.len(), 1);
        assert_eq!(parsed.tools[0].name, "ping");
        assert!(parsed.tools[0].description.is_empty());
        assert!(parsed.tools[0].input_schema.is_none());
    }

    #[test]
    fn test_parse_tools_array_direct() {
        // 直接数组格式
        let json = r#"[
            {"name": "tool_a", "description": "Tool A"},
            {"name": "tool_b"}
        ]"#;
        let parsed: Vec<ToolEntry> = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].name, "tool_a");
        assert_eq!(parsed[0].description, "Tool A");
        assert!(parsed[1].description.is_empty());
    }

    // ── call_tool response parsing ──────────────────────────────────────────────

    #[test]
    fn test_parse_call_tool_response() {
        let json = r#"{
            "content": [{"type": "text", "text": "Hello, world!"}],
            "isError": false
        }"#;
        let parsed: CallToolResponse = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.content.len(), 1);
        assert_eq!(parsed.content[0].text, "Hello, world!");
        assert_eq!(parsed.is_error, Some(false));
    }

    #[test]
    fn test_parse_call_tool_response_error() {
        let json = r#"{
            "content": [{"type": "text", "text": "Something went wrong"}],
            "isError": true
        }"#;
        let parsed: CallToolResponse = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.is_error, Some(true));
    }

    #[test]
    fn test_parse_call_tool_response_no_content() {
        let json = r#"{"isError": false}"#;
        let parsed: CallToolResponse = serde_json::from_str(json).unwrap();
        assert!(parsed.content.is_empty());
        // isError 显式指定为 false
        assert_eq!(parsed.is_error, Some(false));
    }

    // ── urlencoding ─────────────────────────────────────────────────────────────

    #[test]
    fn test_urlencoding_basic() {
        assert_eq!(urlencoding("hello"), "hello");
        assert_eq!(urlencoding("hello world"), "hello%20world");
        assert_eq!(urlencoding("a/b?c"), "a%2Fb%3Fc");
        assert_eq!(urlencoding(""), "");
    }

    #[test]
    fn test_urlencoding_special_chars() {
        assert_eq!(urlencoding("foo&bar=baz"), "foo%26bar%3Dbaz");
        assert_eq!(urlencoding("你好"), "%E4%BD%A0%E5%A5%BD");
    }

    // ── McpToolDefinition 转换 ──────────────────────────────────────────────────

    #[test]
    fn test_tool_entry_to_definition() {
        let entry = ToolEntry {
            name: "search".into(),
            description: "Search tool".into(),
            input_schema: Some(serde_json::json!({"type": "object"})),
        };
        let def = McpToolDefinition {
            name: entry.name,
            description: if entry.description.is_empty() {
                None
            } else {
                Some(entry.description)
            },
            input_schema: entry
                .input_schema
                .unwrap_or_else(|| serde_json::json!({"type": "object"})),
        };
        assert_eq!(def.name, "search");
        assert_eq!(def.description, Some("Search tool".into()));
        assert_eq!(def.input_schema, serde_json::json!({"type": "object"}));
    }
}
