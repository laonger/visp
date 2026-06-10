//! Transport 工厂函数
//!
//! 封装 rmcp transport 的创建逻辑，根据配置创建 stdio 或 SSE transport。

use std::collections::HashMap;

use rmcp::transport::{SseTransport, TokioChildProcess};

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

            TokioChildProcess::new(&mut cmd).map_err(|e| {
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

/// 创建 SSE transport（连接已有 MCP 服务器）
///
/// 通过 HTTP SSE 连接已运行的 MCP 服务器。
pub async fn create_sse_transport(
    url: &str,
    headers: &HashMap<String, String>,
) -> Result<SseTransport, McpError> {
    // 如果有自定义 headers，使用自定义 client
    if !headers.is_empty() {
        let mut client_builder = reqwest::Client::builder();
        let mut default_headers = reqwest::header::HeaderMap::new();
        for (key, value) in headers {
            if let (Ok(name), Ok(val)) = (
                reqwest::header::HeaderName::from_bytes(key.as_bytes()),
                reqwest::header::HeaderValue::from_str(value),
            ) {
                default_headers.insert(name, val);
            } else {
                tracing::warn!("invalid header: '{key}' with value '{value}', skipping");
            }
        }
        client_builder = client_builder.default_headers(default_headers);
        let client = client_builder
            .build()
            .map_err(|e| McpError::Transport(format!("failed to build reqwest client: {}", e)))?;
        SseTransport::start_with_client(url, client)
            .await
            .map_err(|e| McpError::Transport(format!("SSE connect to '{}' failed: {}", url, e)))
    } else {
        SseTransport::start(url)
            .await
            .map_err(|e| McpError::Transport(format!("SSE connect to '{}' failed: {}", url, e)))
    }
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
    async fn test_create_sse_transport_invalid_url() {
        let headers = HashMap::new();
        let result = create_sse_transport("http://localhost:1/mcp", &headers).await;
        // Should fail because nothing is listening on port 1
        assert!(result.is_err());
    }
}
