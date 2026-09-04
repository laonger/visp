//! Transport 工厂函数
//!
//! 封装 rmcp transport 的创建逻辑，根据配置创建 stdio 或 Streamable HTTP transport。

use std::collections::HashMap;

use rmcp::transport::TokioChildProcess;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;

use crate::config::McpTransport;
use crate::error::McpError;

/// 创建 stdio transport（子进程方式）
///
/// daemon 以子进程方式启动 MCP 服务器，通过 stdin/stdout 通信。
pub fn create_stdio_transport(config: &McpTransport) -> Result<TokioChildProcess, McpError> {
    match config {
        McpTransport::Stdio { command, args, env } => {
            let mut cmd = tokio::process::Command::new(command);
            if !args.is_empty() {
                cmd.args(args);
            }
            // 设置环境变量（继承当前环境 + 叠加配置中的 env）
            if !env.is_empty() {
                cmd.envs(env);
            }

            TokioChildProcess::new(cmd).map_err(|e| {
                McpError::Transport(format!(
                    "failed to spawn child process '{}': {}",
                    command, e
                ))
            })
        }
        _ => Err(McpError::Transport(
            "expected Stdio transport config".into(),
        )),
    }
}

/// 创建 Streamable HTTP transport（连接已有 MCP 服务器）
///
/// 通过 HTTP Streamable 连接已运行的 MCP 服务器。
/// 注：rmcp 3.x 已移除旧版 SSE-only 客户端，`type = "sse"` 的配置同样
/// 走 Streamable HTTP（主流 MCP 服务器均兼容）。
pub fn create_http_transport(
    url: &str,
    headers: &HashMap<String, String>,
) -> Result<rmcp::transport::StreamableHttpClientTransport<reqwest::Client>, McpError> {
    let mut config = StreamableHttpClientTransportConfig::with_uri(url);

    if !headers.is_empty() {
        let mut custom_headers = HashMap::new();
        for (key, value) in headers {
            let name = http::HeaderName::from_bytes(key.as_bytes())
                .map_err(|e| McpError::Transport(format!("invalid header name '{key}': {e}")))?;
            let val = http::HeaderValue::from_str(value).map_err(|e| {
                McpError::Transport(format!("invalid header value for '{key}': {e}"))
            })?;
            custom_headers.insert(name, val);
        }
        config = config.custom_headers(custom_headers);
    }

    Ok(rmcp::transport::StreamableHttpClientTransport::from_config(
        config,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::McpTransport;

    #[tokio::test]
    async fn test_create_stdio_transport_simple_command() {
        let transport = McpTransport::Stdio {
            command: "echo".into(),
            args: vec!["hello".into()],
            env: HashMap::new(),
        };
        let result = create_stdio_transport(&transport);
        assert!(
            result.is_ok(),
            "should create TokioChildProcess: {:?}",
            result.err()
        );
    }

    #[tokio::test]
    async fn test_create_stdio_transport_with_env() {
        let mut env = HashMap::new();
        env.insert("MCP_KEY".into(), "secret".into());
        let transport = McpTransport::Stdio {
            command: "echo".into(),
            args: vec![],
            env,
        };
        let result = create_stdio_transport(&transport);
        assert!(result.is_ok());
    }

    #[test]
    fn test_create_stdio_transport_from_sse_config() {
        let transport = McpTransport::Sse {
            url: "http://localhost:3000/mcp".into(),
            headers: HashMap::new(),
        };
        let result = create_stdio_transport(&transport);
        assert!(result.is_err());
        match result {
            Err(McpError::Transport(msg)) => {
                assert!(msg.contains("expected Stdio"));
            }
            _ => panic!("expected Transport error"),
        }
    }

    #[test]
    fn test_create_stdio_transport_nonexistent_command() {
        let transport = McpTransport::Stdio {
            command: "nonexistent-binary-xyz".into(),
            args: vec![],
            env: HashMap::new(),
        };
        let result = create_stdio_transport(&transport);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_create_http_transport_simple() {
        let result = create_http_transport("http://localhost:3000/mcp", &HashMap::new());
        assert!(result.is_ok(), "transport creation should succeed lazily");
    }

    #[tokio::test]
    async fn test_create_http_transport_with_headers() {
        let mut headers = HashMap::new();
        headers.insert("Authorization".into(), "Bearer sk-test".into());
        let result = create_http_transport("http://localhost:3000/mcp", &headers);
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_create_http_transport_invalid_header_name() {
        let mut headers = HashMap::new();
        headers.insert("invalid header name with spaces".into(), "value".into());
        let result = create_http_transport("http://localhost:3000/mcp", &headers);
        assert!(result.is_err());
        match result {
            Err(McpError::Transport(msg)) => {
                assert!(msg.contains("invalid header name"));
            }
            _ => panic!("expected Transport error"),
        }
    }

    #[test]
    fn test_create_http_transport_invalid_header_value() {
        let mut headers = HashMap::new();
        headers.insert("X-Custom".into(), "bad\nvalue".into());
        let result = create_http_transport("http://localhost:3000/mcp", &headers);
        assert!(result.is_err());
    }
}
