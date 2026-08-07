use async_trait::async_trait;
use futures::stream::Stream;
use std::pin::Pin;

use crate::error::LlmError;
use crate::message::{Message, ToolDefinition};

pub use visp_config::{LlmConfig, ModelInfo};

#[cfg(test)]
mod tests_llmconfig {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_llmconfig_default() {
        let config = LlmConfig::default();
        assert_eq!(config.model, "claude-3-7-sonnet-20250219");
        assert_eq!(config.temperature, 0.7);
        assert_eq!(config.max_tokens, 4096);
        assert!(config.extra.is_empty());
    }

    #[test]
    fn test_llmconfig_capture_defaults() {
        let config = LlmConfig::default();
        assert!(
            !config.langfuse_capture_input,
            "capture_input should default to false"
        );
        assert!(
            !config.langfuse_capture_output,
            "capture_output should default to false"
        );
        assert_eq!(config.langfuse_capture_max_chars, 20000);
        assert!(config.langfuse_redact_secrets);
    }

    #[test]
    fn test_llmconfig_capture_configured() {
        let config = LlmConfig {
            langfuse_capture_input: true,
            langfuse_capture_output: false,
            langfuse_capture_max_chars: 5000,
            langfuse_redact_secrets: false,
            ..Default::default()
        };
        assert!(config.langfuse_capture_input);
        assert!(!config.langfuse_capture_output);
        assert_eq!(config.langfuse_capture_max_chars, 5000);
        assert!(!config.langfuse_redact_secrets);
    }

    #[test]
    fn test_llmconfig_extra() {
        let mut config = LlmConfig::default();
        config.extra.insert("key".to_string(), "value".to_string());
        assert_eq!(config.extra.get("key").unwrap(), "value");
    }

    #[test]
    fn test_llmconfig_default_max_context_tokens() {
        let config = LlmConfig::default();
        assert_eq!(config.max_context_tokens, 128_000);
    }

    #[test]
    fn test_llmconfig_custom_max_context_tokens() {
        let config = LlmConfig {
            max_context_tokens: 64_000,
            ..Default::default()
        };
        assert_eq!(config.max_context_tokens, 64_000);
    }

    // ── Langfuse trace 级字段测试 ──────────────────────────────────────────

    #[test]
    fn test_llmconfig_langfuse_trace_defaults() {
        let config = LlmConfig::default();
        assert!(
            !config.langfuse_enabled,
            "langfuse_enabled should default to false"
        );
        assert_eq!(config.langfuse_session_id, None);
        assert_eq!(config.langfuse_trace_name, None);
        assert_eq!(config.langfuse_user_id, None);
        assert_eq!(config.langfuse_tags, None);
        assert_eq!(config.langfuse_environment, None);
        assert_eq!(config.langfuse_release, None);
        assert_eq!(config.langfuse_version, None);
        assert_eq!(config.langfuse_public, None);
        assert_eq!(config.langfuse_metadata, None);
    }

    #[test]
    fn test_llmconfig_langfuse_trace_configured() {
        let mut meta = HashMap::new();
        meta.insert("env".into(), "prod".into());
        let config = LlmConfig {
            langfuse_enabled: true,
            langfuse_session_id: Some("sess_abc".into()),
            langfuse_trace_name: Some("visp.agent.run".into()),
            langfuse_user_id: Some("user_789".into()),
            langfuse_tags: Some(r#"["agent"]"#.into()),
            langfuse_environment: Some("staging".into()),
            langfuse_release: Some("1.0.0".into()),
            langfuse_version: Some("abc123".into()),
            langfuse_public: Some(true),
            langfuse_metadata: Some(meta.clone()),
            ..Default::default()
        };
        assert!(config.langfuse_enabled);
        assert_eq!(config.langfuse_session_id.as_deref(), Some("sess_abc"));
        assert_eq!(
            config.langfuse_trace_name.as_deref(),
            Some("visp.agent.run")
        );
        assert_eq!(config.langfuse_user_id.as_deref(), Some("user_789"));
        assert_eq!(config.langfuse_tags.as_deref(), Some(r#"["agent"]"#));
        assert_eq!(config.langfuse_environment.as_deref(), Some("staging"));
        assert_eq!(config.langfuse_release.as_deref(), Some("1.0.0"));
        assert_eq!(config.langfuse_version.as_deref(), Some("abc123"));
        assert_eq!(config.langfuse_public, Some(true));
        assert_eq!(config.langfuse_metadata, Some(meta));
    }

    #[test]
    fn test_llmconfig_langfuse_partial_trace() {
        let config = LlmConfig {
            langfuse_enabled: true,
            langfuse_user_id: Some("partial".into()),
            langfuse_environment: Some("default".into()),
            ..Default::default()
        };
        assert!(config.langfuse_enabled);
        assert_eq!(config.langfuse_user_id.as_deref(), Some("partial"));
        assert_eq!(config.langfuse_environment.as_deref(), Some("default"));
        // Other fields should remain None
        assert_eq!(config.langfuse_session_id, None);
        assert_eq!(config.langfuse_trace_name, None);
        assert_eq!(config.langfuse_tags, None);
        assert_eq!(config.langfuse_release, None);
        assert_eq!(config.langfuse_version, None);
        assert_eq!(config.langfuse_public, None);
        assert_eq!(config.langfuse_metadata, None);
    }

    #[test]
    fn test_llmconfig_langfuse_capture_defaults_unchanged() {
        // Verify existing capture defaults are not affected by new trace fields
        let config = LlmConfig::default();
        assert!(!config.langfuse_capture_input);
        assert!(!config.langfuse_capture_output);
        assert_eq!(config.langfuse_capture_max_chars, 20000);
        assert!(config.langfuse_redact_secrets);
    }
}

/// LLM 流式响应中的事件
#[derive(Debug, Clone)]
pub enum ChatEvent {
    /// 文本增量（流式输出的一块）
    TextDelta(String),
    /// 工具调用请求（LLM 要求执行某个工具）
    ToolCall {
        id: String,
        name: String,
        arguments: String, // JSON string
    },
    /// 思考块（如 DeepSeek thinking mode），原样 JSON
    ThinkingBlock(serde_json::Value),
    /// token 用量及工具调用次数
    UsageInfo {
        input_tokens: u32,
        output_tokens: u32,
        tool_calls: u32,
        cache_creation_input_tokens: u32,
        cache_read_input_tokens: u32,
    },
    /// LLM 响应完成时携带的 ProviderMetadata
    /// 由 provider 在响应全部接收完毕后发射，位于 UsageInfo 之后、Done 之前
    OutputMetadata(crate::ProviderMetadata),
    /// LLM 输出的图片内容块
    ImageBlock {
        /// 图片本地文件路径（base64 来源有值，URL 来源为空字符串）
        path: String,
        /// 图片 MIME 类型（URL 来源为空字符串）
        mime_type: String,
        /// 远程 URL（URL 来源有值，base64 来源为 None）
        remote_url: Option<String>,
    },
    /// 图片处理失败（base64 解码失败、文件写入失败等）
    ImageError {
        /// 失败原因
        reason: String,
    },
    /// 流结束
    Done,
}

/// LLM 提供器抽象 trait
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// 流式对话
    /// 发送消息列表，流式接收响应
    ///
    /// `cancel` 为取消令牌：实现方应在阻塞 IO 路径（如 HTTP send）使用
    /// `tokio::select!` 监听该 token，一旦触发立刻返回 `LlmError::Cancelled`，
    /// 让 agent loop 能快速跳出。轻量 mock 可忽略此参数。
    async fn chat_stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        config: &LlmConfig,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ChatEvent, LlmError>> + Send>>, LlmError>;
}
