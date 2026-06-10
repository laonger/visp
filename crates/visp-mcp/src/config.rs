use std::collections::HashMap;

use serde::Deserialize;

/// MCP 配置（daemon.toml 中的 [mcp] section）
#[derive(Debug, Clone, Deserialize, Default)]
pub struct McpConfig {
    #[serde(default)]
    pub servers: Vec<McpServerConfig>,
}

/// 单个 MCP 服务器配置
#[derive(Debug, Clone, Deserialize)]
pub struct McpServerConfig {
    /// 唯一标识名
    pub name: String,
    /// 传输方式
    pub transport: McpTransport,
    /// 是否在启动时自动连接（默认 true）
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// 工具名称前缀，防止不同服务器的工具名冲突
    #[serde(default)]
    pub tool_prefix: Option<String>,
    /// 工具调用超时秒数（默认 60）
    #[serde(default = "default_tool_timeout")]
    pub tool_timeout_secs: u64,
}

fn default_enabled() -> bool {
    true
}

fn default_tool_timeout() -> u64 {
    60
}

/// MCP 传输方式
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum McpTransport {
    /// 子进程方式：daemon 启动并管理
    #[serde(rename = "stdio")]
    Stdio {
        /// 启动命令
        command: String,
        /// 命令参数
        #[serde(default)]
        args: Vec<String>,
        /// 环境变量（继承 daemon 环境，此处的为叠加/覆盖）
        #[serde(default)]
        env: HashMap<String, String>,
    },
    /// HTTP SSE 方式：连接已运行的 MCP 服务器
    #[serde(rename = "sse")]
    Sse {
        /// SSE 端点 URL
        url: String,
        /// HTTP 请求头（用于认证等）
        #[serde(default)]
        headers: HashMap<String, String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mcp_config_empty_servers() {
        let toml_str = "servers = []\n";
        let config: McpConfig = toml::from_str(toml_str).unwrap();
        assert!(config.servers.is_empty());
    }

    #[test]
    fn test_mcp_server_stdio_deserialize() {
        let toml_str = r#"
[[servers]]
name = "playwright"
transport = { type = "stdio", command = "npx", args = ["@anthropic-ai/mcp-playwright"] }
"#;
        let config: McpConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.servers.len(), 1);
        let server = &config.servers[0];
        assert_eq!(server.name, "playwright");
        assert!(server.enabled);
        assert_eq!(server.tool_timeout_secs, 60);
        assert!(server.tool_prefix.is_none());
        match &server.transport {
            McpTransport::Stdio { command, args, env } => {
                assert_eq!(command, "npx");
                assert_eq!(args, &vec!["@anthropic-ai/mcp-playwright"]);
                assert!(env.is_empty());
            }
            _ => panic!("expected Stdio"),
        }
    }

    #[test]
    fn test_mcp_server_stdio_with_env() {
        let toml_str = r#"
[[servers]]
name = "custom"
transport = { type = "stdio", command = "python", args = ["server.py"], env = { API_KEY = "sk-xxx" } }
"#;
        let config: McpConfig = toml::from_str(toml_str).unwrap();
        let server = &config.servers[0];
        match &server.transport {
            McpTransport::Stdio { command, args, env } => {
                assert_eq!(command, "python");
                assert_eq!(args, &vec!["server.py"]);
                assert_eq!(env.get("API_KEY"), Some(&"sk-xxx".to_string()));
            }
            _ => panic!("expected Stdio"),
        }
    }

    #[test]
    fn test_mcp_server_sse_deserialize() {
        let toml_str = r#"
[[servers]]
name = "custom-api"
transport = { type = "sse", url = "http://localhost:3000/mcp" }
enabled = true
tool_prefix = "my_"
"#;
        let config: McpConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.servers.len(), 1);
        let server = &config.servers[0];
        assert_eq!(server.name, "custom-api");
        assert_eq!(server.tool_prefix, Some("my_".to_string()));
        match &server.transport {
            McpTransport::Sse { url, headers } => {
                assert_eq!(url, "http://localhost:3000/mcp");
                assert!(headers.is_empty());
            }
            _ => panic!("expected Sse"),
        }
    }

    #[test]
    fn test_mcp_server_sse_with_headers() {
        let toml_str = r#"
[[servers]]
name = "auth-api"
transport = { type = "sse", url = "http://localhost:3000/mcp", headers = { Authorization = "Bearer sk-xxx" } }
"#;
        let config: McpConfig = toml::from_str(toml_str).unwrap();
        let server = &config.servers[0];
        match &server.transport {
            McpTransport::Sse { url, headers } => {
                assert_eq!(url, "http://localhost:3000/mcp");
                assert_eq!(
                    headers.get("Authorization"),
                    Some(&"Bearer sk-xxx".to_string())
                );
            }
            _ => panic!("expected Sse"),
        }
    }

    #[test]
    fn test_mcp_server_defaults() {
        let toml_str = r#"
[[servers]]
name = "defaults"
transport = { type = "stdio", command = "echo" }
"#;
        let config: McpConfig = toml::from_str(toml_str).unwrap();
        let server = &config.servers[0];
        // enabled defaults to true
        assert!(server.enabled);
        // tool_timeout_secs defaults to 60
        assert_eq!(server.tool_timeout_secs, 60);
        // tool_prefix defaults to None
        assert!(server.tool_prefix.is_none());
        // args defaults to empty
        match &server.transport {
            McpTransport::Stdio { args, .. } => {
                assert!(args.is_empty());
            }
            _ => panic!("expected Stdio"),
        }
    }
}
