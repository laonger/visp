use async_trait::async_trait;
use futures::stream::{self};
use std::pin::Pin;
use visp_config::LlmConfig;
use visp_core::error::LlmError;
use visp_core::message::{Message, ToolDefinition};
use visp_core::provider::{ChatEvent, LlmProvider};

/// Mock provider，用于测试
pub struct MockProvider {
    events: Vec<ChatEvent>,
}

impl MockProvider {
    pub fn new(events: Vec<ChatEvent>) -> Self {
        Self { events }
    }
}

#[async_trait]
impl LlmProvider for MockProvider {
    async fn chat_stream(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _config: &LlmConfig,
        _cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<Pin<Box<dyn futures::Stream<Item = Result<ChatEvent, LlmError>> + Send>>, LlmError>
    {
        let mut events = self.events.clone();
        if events.is_empty() {
            events.push(ChatEvent::Done);
        }
        let stream = stream::iter(events.into_iter().map(Ok));
        Ok(Box::pin(stream))
    }
}

#[cfg(test)]
#[path = "mock_tests.rs"]
mod tests;
