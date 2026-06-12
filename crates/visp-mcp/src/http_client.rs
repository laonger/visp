//! HTTP POST MCP 客户端（Streamable HTTP）
//!
//! 用于支持 MCP Streamable HTTP 传输的服务器（如 context7）。
//! 所有通信通过单一的 POST 端点完成，无需 SSE 连接。
//!
//! 遵循 MCP Streamable HTTP 规范：
//! - 初始化：`POST {url}` 发送 initialize 请求
//! - 工具发现：`POST {url}` 发送 tools/list 请求
//! - 工具调用：`POST {url}` 发送 tools/call 请求

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use rmcp::model::{CallToolResult, Content};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::client::McpToolDefinition;
use crate::error::McpError;

/// HTTP POST 模式的 MCP 客户端
///
/// 通过单一的 POST 端点完成 MCP 协议的所有交互。
/// 不需要 SSE 连接，每个请求-响应周期独立。
#[derive(Debug, Clone)]
pub struct HttpPostClient {
    /// 服务器 URL
    url: String,
    /// HTTP 请求头
    #[allow(dead_code)]
    headers: HashMap<String, String>,
    /// reqwest HTTP 客户端
    http_client: reqwest::Client,
    /// 服务器名称
    server_name: String,
    /// 是否已完成初始化握手
    initialized: bool,
    /// JSON-RPC 请求 ID 生成器
    request_id: Arc<AtomicU64>,
    /// MCP session ID（由 initialize 响应提供，后续请求需要在 header 中回传）
    mcp_session_id: Arc<Mutex<Option<String>>>,
}

/// JSON-RPC 请求
#[derive(Debug, Serialize)]
struct JsonRpcRequest<P: Serialize> {
    jsonrpc: &'static str,
    id: u64,
    method: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<P>,
}

/// JSON-RPC 通知（无需响应）
#[derive(Debug, Serialize)]
struct JsonRpcNotification {
    jsonrpc: &'static str,
    method: &'static str,
}

/// JSON-RPC 响应
#[derive(Debug, Deserialize)]
struct JsonRpcResponse {
    #[allow(dead_code)]
    jsonrpc: Option<String>,
    #[allow(dead_code)]
    id: Option<Value>,
    result: Option<Value>,
    error: Option<JsonRpcError>,
}

/// JSON-RPC 错误
#[derive(Debug, Deserialize)]
struct JsonRpcError {
    code: i32,
    message: String,
    #[allow(dead_code)]
    data: Option<Value>,
}

/// Initialize 请求参数
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct InitializeParams {
    protocol_version: &'static str,
    capabilities: ClientCapabilities,
    client_info: ClientInfoParam,
}

/// 客户端能力
#[derive(Debug, Serialize)]
struct ClientCapabilities {}

/// 客户端信息参数
#[derive(Debug, Serialize)]
struct ClientInfoParam {
    name: String,
    version: String,
}

/// 从 SSE 响应体中提取第一条 `data:` 行的内容。
///
/// SSE 格式规范：
/// ```text
/// event: message
/// data: {"jsonrpc":"2.0","id":1,"result":{...}}
///
/// ```
/// 每个事件由空行分隔，`data:` 行包含实际数据负载。
/// 此函数返回第一个非空事件的 data 行内容。
fn extract_sse_data(body: &str) -> Option<String> {
    // 按空行分割事件
    for event in body.split("\n\n") {
        let event = event.trim();
        if event.is_empty() {
            continue;
        }
        // 查找 data: 行
        for line in event.lines() {
            if let Some(value) = line.strip_prefix("data:") {
                let value = value.trim();
                if !value.is_empty() {
                    return Some(value.to_string());
                }
            }
        }
    }
    None
}

impl HttpPostClient {
    /// 创建新的 HTTP POST 客户端
    pub fn new(
        server_name: &str,
        url: &str,
        headers: &HashMap<String, String>,
    ) -> Result<Self, McpError> {
        let mut client_builder = reqwest::Client::builder();
        let mut default_headers = reqwest::header::HeaderMap::new();

        // 先设置用户自定义头
        for (key, value) in headers {
            let name = reqwest::header::HeaderName::from_bytes(key.as_bytes())
                .map_err(|e| McpError::Transport(format!("invalid header name '{key}': {e}")))?;
            let val = reqwest::header::HeaderValue::from_str(value).map_err(|e| {
                McpError::Transport(format!("invalid header value for '{key}': {e}"))
            })?;
            default_headers.insert(name, val);
        }

        // MCP Streamable HTTP 要求客户端声明接受两种响应类型
        // 参见 https://spec.modelcontextprotocol.io/specification/2025-03-26/basic/transports/#streamable-http
        if !default_headers.contains_key(reqwest::header::ACCEPT) {
            default_headers.insert(
                reqwest::header::ACCEPT,
                reqwest::header::HeaderValue::from_static("application/json, text/event-stream"),
            );
        }

        if !default_headers.is_empty() {
            client_builder = client_builder.default_headers(default_headers);
        }

        let http_client = client_builder
            .build()
            .map_err(|e| McpError::Transport(format!("failed to build reqwest client: {e}")))?;

        Ok(Self {
            url: url.to_string(),
            headers: headers.clone(),
            http_client,
            server_name: server_name.to_string(),
            initialized: false,
            request_id: Arc::new(AtomicU64::new(1)),
            mcp_session_id: Arc::new(Mutex::new(None)),
        })
    }

    /// 获取下一个请求 ID
    fn next_id(&self) -> u64 {
        self.request_id.fetch_add(1, Ordering::SeqCst)
    }

    /// 发送 POST 请求并解析 JSON-RPC 响应
    async fn post<P: Serialize>(
        &self,
        method: &'static str,
        params: Option<P>,
    ) -> Result<Value, McpError> {
        let id = self.next_id();
        let body = JsonRpcRequest {
            jsonrpc: "2.0",
            id,
            method,
            params,
        };

        let mut request = self.http_client.post(&self.url).json(&body);

        // MCP Streamable HTTP: 携带 session ID（如果已通过 initialize 获得）
        if let Some(ref sid) = *self.mcp_session_id.lock().unwrap() {
            request = request.header("mcp-session-id", sid.clone());
        }

        let response = request
            .send()
            .await
            .map_err(|e| McpError::Transport(format!("POST to '{}' failed: {}", self.url, e)))?;

        // 从响应中提取 session ID（由 initialize 返回，后续请求复用）
        if let Some(session_id) = response.headers().get("mcp-session-id")
            && let Ok(sid) = session_id.to_str()
        {
            *self.mcp_session_id.lock().unwrap() = Some(sid.to_string());
        }

        let status = response.status();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        // 先读取响应体（避免被消费后丢失）
        let body_bytes = response
            .bytes()
            .await
            .map_err(|e| McpError::Transport(format!("read response body failed: {e}")))?;

        if !status.is_success() {
            let body_text = String::from_utf8_lossy(&body_bytes);
            return Err(McpError::Transport(format!(
                "POST '{}' returned {status} (content-type: {content_type}): {body_text}",
                self.url
            )));
        }

        // 根据 Content-Type 解析响应体
        let json_str = if content_type.starts_with("text/event-stream") {
            // SSE 格式：从 data: 行中提取 JSON 负载
            let body_str = String::from_utf8_lossy(&body_bytes);
            extract_sse_data(&body_str).ok_or_else(|| {
                McpError::Protocol(format!(
                    "no SSE event data found (content-type: {content_type}, body: {body_str})",
                ))
            })?
        } else {
            // 默认按 JSON 解析
            String::from_utf8_lossy(&body_bytes).to_string()
        };

        let response_body: JsonRpcResponse = serde_json::from_str(&json_str).map_err(|e| {
            McpError::Protocol(format!(
                "parse JSON-RPC response failed: {e} (content-type: {content_type}, body: {json_str})",
            ))
        })?;

        if let Some(err) = response_body.error {
            return Err(McpError::Protocol(format!(
                "JSON-RPC error ({}): {}",
                err.code, err.message
            )));
        }

        response_body
            .result
            .ok_or_else(|| McpError::Protocol("JSON-RPC response missing 'result'".into()))
    }

    /// 执行 MCP 初始化握手
    pub async fn initialize(&mut self) -> Result<(), McpError> {
        if self.initialized {
            return Ok(());
        }

        let params = InitializeParams {
            protocol_version: "2024-11-05",
            capabilities: ClientCapabilities {},
            client_info: ClientInfoParam {
                name: "visp".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            },
        };

        let _result = self.post("initialize", Some(&params)).await?;

        // 发送 initialized 通知
        let notification = JsonRpcNotification {
            jsonrpc: "2.0",
            method: "notifications/initialized",
        };

        let mut notif_req = self.http_client.post(&self.url).json(&notification);
        if let Some(ref sid) = *self.mcp_session_id.lock().unwrap() {
            notif_req = notif_req.header("mcp-session-id", sid.clone());
        }

        // 通知不需要响应，忽略错误（服务器可能已关闭连接）
        let _ = notif_req.send().await;

        self.initialized = true;
        Ok(())
    }

    /// 获取工具列表
    pub async fn list_tools(&self) -> Result<Vec<McpToolDefinition>, McpError> {
        if !self.initialized {
            return Err(McpError::NotConnected(self.server_name.clone()));
        }

        let result: Value = self.post::<()>("tools/list", None).await?;

        // 解析 tools 数组
        let tools = result
            .get("tools")
            .and_then(|v| v.as_array())
            .ok_or_else(|| {
                McpError::Protocol("tools/list response missing 'tools' array".into())
            })?;

        let mut defs = Vec::with_capacity(tools.len());
        for tool in tools {
            let name = tool
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| McpError::Protocol("tool entry missing 'name'".into()))?
                .to_string();

            let description = tool
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            let input_schema = tool
                .get("inputSchema")
                .or_else(|| tool.get("input_schema"))
                .cloned()
                .unwrap_or_else(|| serde_json::json!({"type": "object"}));

            defs.push(McpToolDefinition {
                name,
                description: if description.is_empty() {
                    None
                } else {
                    Some(description)
                },
                input_schema,
            });
        }

        Ok(defs)
    }

    /// 调用工具
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: serde_json::Value,
    ) -> Result<CallToolResult, McpError> {
        if !self.initialized {
            return Err(McpError::NotConnected(self.server_name.clone()));
        }

        let args_map = match arguments {
            Value::Object(map) => Some(map),
            Value::Null => None,
            _ => {
                let mut map = serde_json::Map::new();
                map.insert("value".into(), arguments);
                Some(map)
            }
        };

        // 构造 tools/call 参数
        #[derive(Serialize)]
        struct CallToolParams<'a> {
            name: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            arguments: Option<serde_json::Map<String, Value>>,
        }

        let params = CallToolParams {
            name,
            arguments: args_map,
        };

        let result: Value = self.post("tools/call", Some(&params)).await?;

        // 解析为 CallToolResult
        let content = result
            .get("content")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .map(|c| {
                        let text = c.get("text").and_then(|v| v.as_str()).unwrap_or("");
                        Content::text(text.to_string())
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let is_error = result.get("isError").and_then(|v| v.as_bool());

        Ok(CallToolResult { content, is_error })
    }

    /// 检查是否已完成初始化
    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    /// 获取服务器名称
    pub fn server_name(&self) -> &str {
        &self.server_name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── HttpPostClient::new ─────────────────────────────────────────────────────

    #[test]
    fn test_http_post_client_new() {
        let client = HttpPostClient::new(
            "test-server",
            "https://api.example.com/mcp",
            &HashMap::new(),
        )
        .unwrap();
        assert!(!client.is_initialized());
        assert_eq!(client.server_name(), "test-server");
    }

    #[test]
    fn test_http_post_client_with_headers() {
        let mut headers = HashMap::new();
        headers.insert("CONTEXT7_API_KEY".into(), "sk-xxx".into());
        let client =
            HttpPostClient::new("context7", "https://mcp.context7.com/mcp", &headers).unwrap();
        assert_eq!(client.server_name(), "context7");
    }

    #[test]
    fn test_http_post_client_invalid_header() {
        let mut headers = HashMap::new();
        headers.insert("invalid\nheader".into(), "value".into());
        let result = HttpPostClient::new("bad", "https://example.com/mcp", &headers);
        assert!(result.is_err());
    }

    // ── next_id ──────────────────────────────────────────────────────────────────

    #[test]
    fn test_next_id_increments() {
        let client =
            HttpPostClient::new("test", "https://example.com/mcp", &HashMap::new()).unwrap();
        let id1 = client.next_id();
        let id2 = client.next_id();
        assert_eq!(id2, id1 + 1);
    }

    // ── JSON-RPC 序列化 ──────────────────────────────────────────────────────────

    #[test]
    fn test_json_rpc_request_serialization() {
        #[derive(Serialize)]
        struct TestParams {
            name: String,
        }

        let request = JsonRpcRequest {
            jsonrpc: "2.0",
            id: 1,
            method: "tools/list",
            params: None::<TestParams>,
        };

        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["jsonrpc"], "2.0");
        assert_eq!(json["id"], 1);
        assert_eq!(json["method"], "tools/list");
        assert!(json.get("params").is_none());
    }

    #[test]
    fn test_json_rpc_request_with_params() {
        #[derive(Serialize)]
        struct TestParams {
            foo: String,
        }

        let request = JsonRpcRequest {
            jsonrpc: "2.0",
            id: 2,
            method: "tools/call",
            params: Some(TestParams { foo: "bar".into() }),
        };

        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["params"]["foo"], "bar");
    }

    #[test]
    fn test_json_rpc_notification_serialization() {
        let notification = JsonRpcNotification {
            jsonrpc: "2.0",
            method: "notifications/initialized",
        };

        let json = serde_json::to_value(&notification).unwrap();
        assert_eq!(json["jsonrpc"], "2.0");
        assert_eq!(json["method"], "notifications/initialized");
        assert!(json.get("params").is_none());
        assert!(json.get("id").is_none()); // notification 无 id
    }

    // ── Response 解析 ────────────────────────────────────────────────────────────

    #[test]
    fn test_json_rpc_response_success() {
        let json = r#"{"jsonrpc":"2.0","id":1,"result":{"tools":[]}}"#;
        let resp: JsonRpcResponse = serde_json::from_str(json).unwrap();
        assert!(resp.error.is_none());
        assert!(resp.result.is_some());
    }

    #[test]
    fn test_json_rpc_response_error() {
        let json =
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"Method not found"}}"#;
        let resp: JsonRpcResponse = serde_json::from_str(json).unwrap();
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, -32601);
    }

    // ── Tool parsing ────────────────────────────────────────────────────────────

    #[test]
    fn test_parse_tools_response() {
        let json = r#"{
            "tools": [
                {"name": "search", "description": "Search tool", "inputSchema": {"type": "object"}},
                {"name": "fetch", "description": "Fetch URL"}
            ]
        }"#;
        let result: Value = serde_json::from_str(json).unwrap();
        let tools = result["tools"].as_array().unwrap();

        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0]["name"], "search");
        assert!(tools[0].get("inputSchema").is_some());
        assert!(tools[1].get("inputSchema").is_none());
    }

    #[test]
    fn test_parse_call_tool_response() {
        let json = r#"{
            "content": [{"type": "text", "text": "Hello!"}],
            "isError": false
        }"#;
        let result: Value = serde_json::from_str(json).unwrap();
        let content = result["content"].as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["text"], "Hello!");
        assert_eq!(result["isError"], false);
    }

    // ── Initialize params ────────────────────────────────────────────────────────

    #[test]
    fn test_initialize_params_serialization() {
        let params = InitializeParams {
            protocol_version: "2024-11-05",
            capabilities: ClientCapabilities {},
            client_info: ClientInfoParam {
                name: "visp".into(),
                version: "0.2.0".into(),
            },
        };

        let json = serde_json::to_value(&params).unwrap();
        assert_eq!(json["protocolVersion"], "2024-11-05");
        assert_eq!(json["clientInfo"]["name"], "visp");
        assert_eq!(json["clientInfo"]["version"], "0.2.0");
        assert!(json["capabilities"].is_object());
    }

    // ── SSE 解析 ──────────────────────────────────────────────────────────────────

    #[test]
    fn test_extract_sse_data_basic() {
        let body =
            "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"tools\":[]}}\n\n";
        assert_eq!(
            extract_sse_data(body).as_deref(),
            Some(r#"{"jsonrpc":"2.0","id":1,"result":{"tools":[]}}"#),
        );
    }

    #[test]
    fn test_extract_sse_data_with_prefix() {
        let body = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n\n";
        assert!(extract_sse_data(body).is_some());
    }

    #[test]
    fn test_extract_sse_data_multiple_events() {
        // 两个 SSE 事件，返回第一个 data 行
        let body = "event: ping\ndata: {\"event\":\"ping\"}\n\nevent: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"serverInfo\":{\"name\":\"test\"}}}\n\n";
        let data = extract_sse_data(body);
        assert!(data.is_some());
        let parsed: Value = serde_json::from_str(&data.unwrap()).unwrap();
        assert_eq!(parsed["event"], "ping");
    }

    #[test]
    fn test_extract_sse_data_no_data_line() {
        let body = "event: message\n\n";
        assert!(extract_sse_data(body).is_none());
    }

    #[test]
    fn test_extract_sse_data_empty() {
        assert!(extract_sse_data("").is_none());
    }

    #[test]
    fn test_extract_sse_data_only_comments() {
        let body = ":comment\nevent: message\n\n";
        assert!(extract_sse_data(body).is_none());
    }

    #[test]
    fn test_extract_sse_data_empty_data() {
        let body = "event: message\ndata:\n\n";
        assert!(extract_sse_data(body).is_none());
    }
}
