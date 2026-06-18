use async_trait::async_trait;
use futures::stream::Stream;
use serde::{Deserialize, Serialize};
use std::pin::Pin;

use crate::error::LlmError;
use crate::message::{Message, ToolDefinition};

/// LLM 配置参数
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmConfig {
    /// API model key（发送到 LLM API 的 model 字段，如 "deepseek-v4-flash"）
    pub model: String,
    /// 温度（0.0-2.0）
    pub temperature: f64,
    /// 最大 token 数
    pub max_tokens: u32,
    /// 最大上下文 token 数（默认 128_000）
    pub max_context_tokens: u32,
    /// 扩展参数（provider 特定参数）
    pub extra: std::collections::HashMap<String, String>,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            model: "claude-3-7-sonnet-20250219".to_string(),
            temperature: 0.7,
            max_tokens: 4096,
            max_context_tokens: 128_000,
            extra: std::collections::HashMap::new(),
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
