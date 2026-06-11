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

    #[error("Session busy: {session_id}")]
    SessionBusy { session_id: String },

    #[error("Protocol error: {message}")]
    ProtocolError { message: String },

    #[error("{0}")]
    Other(String),
}

/// Agent 内部错误码，用于 AgentEvent::Error
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentErrorCode {
    LlmAuth,
    LlmRateLimit,
    LlmApi,
    LlmNetwork,
    LlmStream,
    MaxIterations,
    StuckInLoop,
    Cancelled,
    Internal,
}

impl std::fmt::Display for AgentErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentErrorCode::LlmAuth => write!(f, "LLM authentication error"),
            AgentErrorCode::LlmRateLimit => write!(f, "LLM rate limit exceeded"),
            AgentErrorCode::LlmApi => write!(f, "LLM API error"),
            AgentErrorCode::LlmNetwork => write!(f, "LLM network error"),
            AgentErrorCode::LlmStream => write!(f, "LLM stream error"),
            AgentErrorCode::MaxIterations => write!(f, "Maximum iterations reached"),
            AgentErrorCode::StuckInLoop => write!(f, "Agent stuck in repeated tool call loop"),
            AgentErrorCode::Cancelled => write!(f, "Operation cancelled"),
            AgentErrorCode::Internal => write!(f, "Internal error"),
        }
    }
}

#[cfg(test)]
mod tests_session {
    use super::*;

    #[test]
    fn test_session_busy_display() {
        let err = SessionError::SessionBusy {
            session_id: "x".into(),
        };
        assert!(err.to_string().contains("x"));
    }

    #[test]
    fn test_session_protocol_error_display() {
        let err = SessionError::ProtocolError {
            message: "mismatch".into(),
        };
        assert!(err.to_string().contains("mismatch"));
    }

    #[test]
    fn test_agent_error_code_display() {
        let cases: Vec<(AgentErrorCode, &str)> = vec![
            (AgentErrorCode::LlmAuth, "LLM authentication error"),
            (AgentErrorCode::LlmRateLimit, "LLM rate limit exceeded"),
            (AgentErrorCode::LlmApi, "LLM API error"),
            (AgentErrorCode::LlmNetwork, "LLM network error"),
            (AgentErrorCode::LlmStream, "LLM stream error"),
            (AgentErrorCode::MaxIterations, "Maximum iterations reached"),
            (AgentErrorCode::Cancelled, "Operation cancelled"),
            (AgentErrorCode::Internal, "Internal error"),
            (
                AgentErrorCode::StuckInLoop,
                "Agent stuck in repeated tool call loop",
            ),
        ];
        for (code, expected) in cases {
            assert_eq!(code.to_string(), expected);
        }
    }

    #[test]
    fn test_stuck_in_loop_match() {
        let err = AgentErrorCode::StuckInLoop;
        match err {
            AgentErrorCode::StuckInLoop => {} // ok
            _ => panic!("StuckInLoop should match StuckInLoop"),
        }
    }
}
