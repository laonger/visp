//! BigModel（智谱 AI）LLM 提供器。
//!
//! 智谱 bigmodel 的 API 是 OpenAI 兼容的（base_url 指向
//! https://open.bigmodel.cn/api/paas/v4），因此复用 OpenAiProvider 的完整
//! 实现（SSE 解析、reasoning_content、tool_calls、finish_reason 容错）。
//! 唯一差异：temperature 范围是 [0.0, 1.0]（OpenAI 是 0~2），委托前 clamp。

use async_trait::async_trait;
use futures::Stream;
use std::pin::Pin;

use visp_config::LlmConfig;
use visp_core::error::LlmError;
use visp_core::message::{Message, ToolDefinition};
use visp_core::provider::{ChatEvent, LlmProvider};

use crate::openai::OpenAiProvider;

/// 智谱 bigmodel 默认 base_url。
pub const DEFAULT_BASE_URL: &str = "https://open.bigmodel.cn/api/paas/v4";

/// 智谱 temperature 范围 [0.0, 1.0]（OpenAI 是 0~2），clamp 到合法范围。
fn clamp_temperature(t: f64) -> f64 {
    t.clamp(0.0, 1.0)
}

/// BigModel（智谱 AI）LLM 提供器。
pub struct BigModelProvider {
    openai: OpenAiProvider,
}

impl BigModelProvider {
    /// base_url 为 None 时使用智谱默认端点。
    pub fn new(api_key: String, base_url: Option<String>) -> Self {
        let base = base_url.unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
        Self {
            openai: OpenAiProvider::with_base_url(api_key, base),
        }
    }
}

#[async_trait]
impl LlmProvider for BigModelProvider {
    async fn chat_stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        config: &LlmConfig,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ChatEvent, LlmError>> + Send>>, LlmError> {
        // 智谱 temperature 范围 [0.0, 1.0]，clamp 防 API 拒绝
        let mut cfg = config.clone();
        cfg.temperature = clamp_temperature(cfg.temperature);
        self.openai.chat_stream(messages, tools, &cfg, cancel).await
    }
}

#[cfg(test)]
#[path = "bigmodel_tests.rs"]
mod tests;
