use thiserror::Error;

/// 顶层核心错误，所有子系统错误向上传播到这里
#[derive(Error, Debug)]
pub enum CoreError {
    #[error("LLM error: {0}")]
    Llm(#[from] LlmError),

    #[error("Tool error: {0}")]
    Tool(String),

    #[error("Session error: {0}")]
    Session(String),

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    Other(String),
}

/// LLM 服务相关错误
#[derive(Error, Debug)]
pub enum LlmError {
    #[error("Network error: {0}")]
    Network(String),

    #[error("Rate limited, retry after {retry_after_secs}s")]
    RateLimit { retry_after_secs: u64 },

    #[error("Authentication error: {0}")]
    Auth(String),

    #[error("API error (status {status}): {message}")]
    Api { status: u16, message: String },

    #[error("Stream error: {0}")]
    Stream(String),
}

#[cfg(test)]
mod tests_llmerror {
    use super::*;

    #[test]
    fn test_llmerror_network_display() {
        let err = LlmError::Network("timeout".into());
        assert_eq!(err.to_string(), "Network error: timeout");
    }

    #[test]
    fn test_llmerror_ratelimit_display() {
        let err = LlmError::RateLimit {
            retry_after_secs: 30,
        };
        assert_eq!(err.to_string(), "Rate limited, retry after 30s");
    }

    #[test]
    fn test_llmerror_auth_display() {
        let err = LlmError::Auth("invalid key".into());
        assert_eq!(err.to_string(), "Authentication error: invalid key");
    }

    #[test]
    fn test_llmerror_api_display() {
        let err = LlmError::Api {
            status: 500,
            message: "internal error".into(),
        };
        assert_eq!(err.to_string(), "API error (status 500): internal error");
    }

    #[test]
    fn test_llmerror_stream_display() {
        let err = LlmError::Stream("parse error".into());
        assert_eq!(err.to_string(), "Stream error: parse error");
    }
}

/// 会话相关错误
#[derive(Error, Debug)]
pub enum SessionError {
    #[error("Session not found: {0}")]
    NotFound(String),

    #[error("Session not found: {0}")]
    AlreadyExists(String),

    #[error("Session is not running: {0}")]
    NotRunning(String),

    #[error("{0}")]
    Other(String),
}
