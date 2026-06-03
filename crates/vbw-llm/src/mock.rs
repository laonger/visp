use async_trait::async_trait;
use futures::stream::{self};
use std::pin::Pin;
use vbw_core::error::LlmError;
use vbw_core::message::{Message, ToolDefinition};
use vbw_core::provider::{ChatEvent, LlmConfig, LlmProvider};

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
mod tests {
    use super::*;
    use futures::StreamExt;

    #[tokio::test]
    async fn test_mock_returns_preset_events() {
        let events = vec![
            ChatEvent::TextDelta("Hello".into()),
            ChatEvent::TextDelta(" World".into()),
            ChatEvent::Done,
        ];
        let provider = MockProvider::new(events.clone());
        let mut stream = provider
            .chat_stream(&[], &[], &vbw_core::provider::LlmConfig::default())
            .await
            .unwrap();

        let mut collected = Vec::new();
        while let Some(event) = stream.next().await {
            collected.push(event.unwrap());
        }

        assert_eq!(collected.len(), 3);
        assert!(matches!(&collected[0], ChatEvent::TextDelta(t) if t == "Hello"));
        assert!(matches!(&collected[1], ChatEvent::TextDelta(t) if t == " World"));
        assert!(matches!(&collected[2], ChatEvent::Done));
    }

    #[tokio::test]
    async fn test_mock_empty_queue() {
        let provider = MockProvider::new(vec![]);
        let mut stream = provider
            .chat_stream(&[], &[], &vbw_core::provider::LlmConfig::default())
            .await
            .unwrap();

        let collected: Vec<_> = stream.by_ref().collect().await;
        assert_eq!(collected.len(), 1, "empty queue should emit Done");
        assert!(matches!(&collected[0], Ok(ChatEvent::Done)));
    }
}
