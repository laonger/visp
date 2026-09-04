//! OpenCode Go LLM 提供器（protocol = "opencode-go"）。
//!
//! `OpencodeProvider` 的别名包装：models.dev 上 OpenCode Zen 有两个通道，
//! 主通道 `opencode`（https://opencode.ai/zen/v1）与 Go 通道 `opencode-go`
//! （https://opencode.ai/zen/go/v1），两者都是 OpenAI 兼容 API、共用
//! OPENCODE_API_KEY。Go 通道在每个流式 chunk 附带增量 usage（openai.rs 已兼容）。
//!
//! visp 最初的 "opencode" 协议默认端点即 Go 通道（zen/go/），因此
//! "opencode-go" 是更准确的命名；二者仅默认 base_url 不同，行为一致。

use async_trait::async_trait;
use futures::Stream;
use std::pin::Pin;

use visp_config::LlmConfig;
use visp_core::error::LlmError;
use visp_core::message::{Message, ToolDefinition};
use visp_core::provider::{ChatEvent, LlmProvider};

use crate::openai::OpenAiProvider;

/// opencode Go 通道默认 base_url（models.dev: opencode-go.api）。
pub const DEFAULT_BASE_URL: &str = "https://opencode.ai/zen/go/v1";

/// OpenCode Go LLM 提供器。
pub struct OpencodeGoProvider {
    openai: OpenAiProvider,
}

impl OpencodeGoProvider {
    /// base_url 为 None 时使用 opencode Go 通道默认端点。
    pub fn new(api_key: String, base_url: Option<String>) -> Self {
        let base = base_url.unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
        Self {
            openai: OpenAiProvider::with_base_url(api_key, base),
        }
    }
}

#[async_trait]
impl LlmProvider for OpencodeGoProvider {
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
#[path = "opencode_go_tests.rs"]
mod tests;
