use async_trait::async_trait;
use futures::stream::Stream;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::pin::Pin;

use crate::error::LlmError;
use crate::message::{Message, ToolDefinition};

/// LLM 配置参数
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmConfig {
    /// API model key（发送到 LLM API 的 model 字段，如 "deepseek-v4-flash"）
    pub model: String,
    /// Provider lookup key（格式 "{provider}/{name}"，如 "Opencode/DeepSeek v4 Flash"）。
    /// 用于从 providers HashMap 中查找正确的 provider 实例。
    /// None 时回退到 default_provider_key。
    pub model_key: Option<String>,
    /// Provider 名称（来自 daemon.toml [[llm.models]] provider 字段，缺省时为 protocol）。
    /// 用于 trace span 和 metrics summary 中记录 gen_ai.provider.name。
    pub provider: Option<String>,
    /// 温度（0.0-2.0）
    pub temperature: f64,
    /// 最大 token 数
    pub max_tokens: u32,
    /// 最大上下文 token 数（默认 128_000）
    pub max_context_tokens: u32,
    /// 扩展参数（provider 特定参数）
    pub extra: HashMap<String, String>,
    /// Langfuse 总开关（控制 gen_ai.client.operation span 上 trace 级字段记录）
    pub langfuse_enabled: bool,
    /// Langfuse session.id（若不设置则不记录）
    pub langfuse_session_id: Option<String>,
    /// Langfuse trace.name（若不设置则不记录）
    pub langfuse_trace_name: Option<String>,
    /// Langfuse user.id 字段值（None = 不设置）
    pub langfuse_user_id: Option<String>,
    /// Langfuse tags 字段值（JSON 字符串，None = 不设置）
    pub langfuse_tags: Option<String>,
    /// Langfuse environment 字段
    pub langfuse_environment: Option<String>,
    /// Langfuse release 字段
    pub langfuse_release: Option<String>,
    /// Langfuse version 字段
    pub langfuse_version: Option<String>,
    /// Langfuse public 开关（None = 不设置）
    pub langfuse_public: Option<bool>,
    /// Langfuse metadata（值均为字符串，记录到 span 时序列化为紧凑 JSON）
    pub langfuse_metadata: Option<HashMap<String, String>>,
    /// 是否在 Langfuse OTEL span 中记录 LLM generation input
    pub langfuse_capture_input: bool,
    /// 是否在 Langfuse OTEL span 中记录 LLM generation output
    pub langfuse_capture_output: bool,
    /// Langfuse capture 最大字符数（超出截断）
    pub langfuse_capture_max_chars: usize,
    /// 是否脱敏敏感字段（api_key/token/secret/password 等）
    pub langfuse_redact_secrets: bool,
    /// 是否在请求中携带工具定义（false 时请求不带 tools）
    pub use_tool: bool,
    /// 是否为文生图模型（true 时使用 /images/generations 端点）
    pub image_generation: bool,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            model: "claude-3-7-sonnet-20250219".to_string(),
            model_key: None,
            provider: None,
            temperature: 0.7,
            max_tokens: 4096,
            max_context_tokens: 128_000,
            extra: HashMap::new(),
            langfuse_enabled: false,
            langfuse_session_id: None,
            langfuse_trace_name: None,
            langfuse_user_id: None,
            langfuse_tags: None,
            langfuse_environment: None,
            langfuse_release: None,
            langfuse_version: None,
            langfuse_public: None,
            langfuse_metadata: None,
            langfuse_capture_input: false,
            langfuse_capture_output: false,
            langfuse_capture_max_chars: 20_000,
            langfuse_redact_secrets: true,
            use_tool: true,
            image_generation: false,
        }
    }
}

#[cfg(test)]
mod tests_llmconfig {
    use super::*;

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

/// 模型元信息，用于 agent 级别的模型覆盖。
///
/// 当 agent 定义中指定了 `model` key（如 `"Opencode/deepseek-v4-flash"`）时，
/// orchestrator 通过此结构解析出实际的 API model 字符串、provider 名称等，
/// 覆盖从父会话继承的 `LlmConfig`。
#[derive(Debug, Clone)]
pub struct ModelInfo {
    /// 发送到 LLM API 的 model 字段（如 "deepseek-v4-flash"）
    pub model: String,
    /// Provider 名称（来自 daemon.toml [[llm.models]] provider 字段）
    pub provider: Option<String>,
    /// 默认温度
    pub temperature: Option<f64>,
    /// 默认 max_tokens
    pub max_tokens: Option<u32>,
    /// 默认 max_context_tokens
    pub max_context_tokens: Option<u32>,
    /// 是否为文生图模型（使用 /images/generations 端点）
    pub image_generation: bool,
    /// 是否在请求中携带工具定义
    pub use_tool: Option<bool>,
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
