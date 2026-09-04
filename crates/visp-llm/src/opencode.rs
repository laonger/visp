//! Opencode Zen LLM 提供器。
//!
//! opencode zen 网关（https://opencode.ai/zen/go/）是 OpenAI 兼容 API，
//! 复用 OpenAiProvider 的完整实现（SSE 解析、reasoning_content、tool_calls、
//! finish_reason 容错）。已知差异：网关在每个流式 chunk 都附带增量 usage
//! 对象（违反 OpenAI 官方约定），openai.rs 的解析层已兼容该行为。

use async_trait::async_trait;
use futures::Stream;
use std::pin::Pin;

use visp_config::LlmConfig;
use visp_core::error::LlmError;
use visp_core::message::{Message, ToolDefinition};
use visp_core::provider::{ChatEvent, LlmProvider};

use crate::openai::OpenAiProvider;

/// opencode zen 默认 base_url。
/// 结尾非版本段（vN），OpenAiProvider 会自动拼接 `/v1/chat/completions`。
pub const DEFAULT_BASE_URL: &str = "https://opencode.ai/zen/go/";

/// Opencode Zen LLM 提供器。
pub struct OpencodeProvider {
    openai: OpenAiProvider,
}

impl OpencodeProvider {
    /// base_url 为 None 时使用 opencode zen 默认端点。
    pub fn new(api_key: String, base_url: Option<String>) -> Self {
        let base = base_url.unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
        Self {
            openai: OpenAiProvider::with_base_url(api_key, base).with_extra_headers(vec![(
                "x-opencode-session".to_string(),
                "{session}".to_string(),
            )]),
        }
    }
}

#[async_trait]
impl LlmProvider for OpencodeProvider {
    async fn chat_stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        config: &LlmConfig,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ChatEvent, LlmError>> + Send>>, LlmError> {
        self.openai
            .chat_stream(messages, tools, config, cancel)
            .await
    }
}

#[cfg(test)]
#[path = "opencode_tests.rs"]
mod tests;
