use std::path::PathBuf;

use crate::error::AgentErrorCode;
use crate::message::Message;
use crate::provider::LlmConfig;

/// Agent 事件，用于流式通知外部（TUI/WS）
pub enum AgentEvent {
    /// 文本增量
    TextDelta(String),
    /// 工具调用请求
    ToolCallRequest {
        call_id: String,
        tool_name: String,
        arguments: String,
    },
    /// 工具调用结果
    ToolCallResult {
        call_id: String,
        content: String,
        is_error: bool,
    },
    /// 状态更新
    StatusUpdate(String),
    /// 发生错误
    Error {
        code: AgentErrorCode,
        message: String,
    },
    /// 完成
    Done,
    /// 需要用户输入
    UserQuery {
        query_id: String,
        message: String,
        respond: tokio::sync::oneshot::Sender<bool>,
    },
}

/// Agent 循环上下文
pub struct AgentLoopContext {
    /// 会话 ID
    pub session_id: String,
    /// 对话历史
    pub history: Vec<Message>,
    /// 工作目录
    pub working_dir: PathBuf,
    /// LLM 配置
    pub config: LlmConfig,
    /// 取消令牌
    pub cancel_token: tokio_util::sync::CancellationToken,
}

/// Agent 执行配置
#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// 最大迭代轮数
    pub max_iterations: u32,
    /// LLM 调用重试次数
    pub llm_retry_attempts: u32,
    /// LLM 重试基础延迟（毫秒）
    pub llm_retry_base_delay_ms: u64,
    /// bash 工具确认模式（执行高危命令前是否需要用户确认）
    pub bash_confirm_mode: bool,
    /// 文件读取/写入的最大字节数
    pub file_max_size_bytes: u64,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_iterations: 50,
            llm_retry_attempts: 3,
            llm_retry_base_delay_ms: 1000,
            bash_confirm_mode: true,
            file_max_size_bytes: 1048576,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_agent_config_default() {
        let cfg = AgentConfig::default();
        assert_eq!(cfg.max_iterations, 50);
        assert_eq!(cfg.llm_retry_attempts, 3);
        assert_eq!(cfg.llm_retry_base_delay_ms, 1000);
        assert_eq!(cfg.bash_confirm_mode, true);
        assert_eq!(cfg.file_max_size_bytes, 1048576);
    }

    #[test]
    fn test_agent_event_text_delta() {
        let evt = AgentEvent::TextDelta("hello".into());
        match evt {
            AgentEvent::TextDelta(content) => assert_eq!(content, "hello"),
            _ => panic!("expected TextDelta"),
        }
    }

    #[test]
    fn test_agent_event_tool_call() {
        let evt = AgentEvent::ToolCallRequest {
            call_id: "call-1".into(),
            tool_name: "bash".into(),
            arguments: "{}".into(),
        };
        match evt {
            AgentEvent::ToolCallRequest {
                call_id,
                tool_name,
                arguments,
            } => {
                assert_eq!(call_id, "call-1");
                assert_eq!(tool_name, "bash");
                assert_eq!(arguments, "{}");
            }
            _ => panic!("expected ToolCallRequest"),
        }
    }

    #[test]
    fn test_agent_event_user_query() {
        let (tx, _rx) = tokio::sync::oneshot::channel::<bool>();
        let evt = AgentEvent::UserQuery {
            query_id: "q1".into(),
            message: "confirm?".into(),
            respond: tx,
        };
        match evt {
            AgentEvent::UserQuery {
                query_id, message, ..
            } => {
                assert_eq!(query_id, "q1");
                assert_eq!(message, "confirm?");
            }
            _ => panic!("expected UserQuery"),
        }
    }

    #[test]
    fn test_agent_loop_context_fields() {
        let cancel = tokio_util::sync::CancellationToken::new();
        let ctx = AgentLoopContext {
            session_id: "sess-1".into(),
            history: vec![],
            working_dir: PathBuf::from("/tmp"),
            config: crate::provider::LlmConfig::default(),
            cancel_token: cancel.clone(),
        };
        assert_eq!(ctx.session_id, "sess-1");
        assert!(ctx.history.is_empty());
        assert_eq!(ctx.working_dir, Path::new("/tmp"));
        assert_eq!(ctx.config.model, "claude-sonnet-4-20250514");
    }
}
