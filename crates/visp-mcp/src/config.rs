pub use visp_config::{McpConfig, McpServerConfig, McpTransport};
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

    #[test]
    fn test_mcp_server_get_deserialize() {
        let toml_str = r#"
[[servers]]
name = "context7"
transport = { type = "get", url = "https://api.context7.com/v1" }
"#;
        let config: McpConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.servers.len(), 1);
        let server = &config.servers[0];
        assert_eq!(server.name, "context7");
        match &server.transport {
            McpTransport::Get {
                url,
                headers,
                tools_endpoint,
                call_endpoint,
            } => {
                assert_eq!(url, "https://api.context7.com/v1");
                assert!(headers.is_empty());
                assert_eq!(tools_endpoint, "/tools");
                assert_eq!(call_endpoint, "/call");
            }
            _ => panic!("expected Get"),
        }
    }

    #[test]
    fn test_mcp_server_get_custom_endpoints() {
        let toml_str = r#"
[[servers]]
name = "custom-get"
transport = { type = "get", url = "https://api.example.com", tools_endpoint = "/list-tools", call_endpoint = "/run" }
"#;
        let config: McpConfig = toml::from_str(toml_str).unwrap();
        let server = &config.servers[0];
        match &server.transport {
            McpTransport::Get {
                url,
                tools_endpoint,
                call_endpoint,
                ..
            } => {
                assert_eq!(url, "https://api.example.com");
                assert_eq!(tools_endpoint, "/list-tools");
                assert_eq!(call_endpoint, "/run");
            }
            _ => panic!("expected Get"),
        }
    }

    #[test]
    fn test_mcp_server_get_with_headers() {
        let toml_str = r#"
[[servers]]
name = "auth-get"
transport = { type = "get", url = "https://api.example.com", headers = { Authorization = "Bearer sk-xxx", X-Custom = "value" } }
"#;
        let config: McpConfig = toml::from_str(toml_str).unwrap();
        let server = &config.servers[0];
        match &server.transport {
            McpTransport::Get { url, headers, .. } => {
                assert_eq!(url, "https://api.example.com");
                assert_eq!(
                    headers.get("Authorization"),
                    Some(&"Bearer sk-xxx".to_string())
                );
                assert_eq!(headers.get("X-Custom"), Some(&"value".to_string()));
            }
            _ => panic!("expected Get"),
        }
    }

    #[test]
    fn test_mcp_server_http_deserialize() {
        let toml_str = r#"
[[servers]]
name = "context7"
transport = { type = "http", url = "https://mcp.context7.com/mcp" }
"#;
        let config: McpConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.servers.len(), 1);
        let server = &config.servers[0];
        assert_eq!(server.name, "context7");
        match &server.transport {
            McpTransport::Http { url, headers } => {
                assert_eq!(url, "https://mcp.context7.com/mcp");
                assert!(headers.is_empty());
            }
            _ => panic!("expected Http"),
        }
    }

    #[test]
    fn test_mcp_server_http_with_headers() {
        let toml_str = r#"
[[servers]]
name = "context7"
transport = { type = "http", url = "https://mcp.context7.com/mcp", headers = { CONTEXT7_API_KEY = "sk-xxx" } }
enabled = true
tool_prefix = "ctx_"
"#;
        let config: McpConfig = toml::from_str(toml_str).unwrap();
        let server = &config.servers[0];
        assert_eq!(server.name, "context7");
        assert_eq!(server.tool_prefix, Some("ctx_".to_string()));
        match &server.transport {
            McpTransport::Http { url, headers } => {
                assert_eq!(url, "https://mcp.context7.com/mcp");
                assert_eq!(headers.get("CONTEXT7_API_KEY"), Some(&"sk-xxx".to_string()));
            }
            _ => panic!("expected Http"),
        }
    }
}
