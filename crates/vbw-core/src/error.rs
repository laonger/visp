use thiserror::Error;

/// 顶层核心错误，所有子系统错误向上传播到这里
#[derive(Error, Debug)]
pub enum CoreError {
    #[error("LLM error: {0}")]
    Llm(String),

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
