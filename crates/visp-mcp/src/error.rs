use thiserror::Error;

#[derive(Debug, Error)]
pub enum McpError {
    #[error("Transport error: {0}")]
    Transport(String),

    #[error("Protocol error: {0}")]
    Protocol(String),

    #[error("Tool call timed out after {0}s")]
    Timeout(u64),

    #[error("Server not connected: {0}")]
    NotConnected(String),

    #[error("Tool not found: {0}")]
    ToolNotFound(String),
}

impl From<std::io::Error> for McpError {
    fn from(e: std::io::Error) -> Self {
        McpError::Transport(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mcp_error_display_transport() {
        let err = McpError::Transport("connection refused".into());
        assert_eq!(err.to_string(), "Transport error: connection refused");
    }

    #[test]
    fn test_mcp_error_display_protocol() {
        let err = McpError::Protocol("version mismatch".into());
        assert_eq!(err.to_string(), "Protocol error: version mismatch");
    }

    #[test]
    fn test_mcp_error_display_timeout() {
        let err = McpError::Timeout(30);
        assert_eq!(err.to_string(), "Tool call timed out after 30s");
    }

    #[test]
    fn test_mcp_error_display_not_connected() {
        let err = McpError::NotConnected("playwright".into());
        assert_eq!(err.to_string(), "Server not connected: playwright");
    }

    #[test]
    fn test_mcp_error_display_tool_not_found() {
        let err = McpError::ToolNotFound("click".into());
        assert_eq!(err.to_string(), "Tool not found: click");
    }
}
