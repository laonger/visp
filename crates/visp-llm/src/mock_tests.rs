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
    let cancel = tokio_util::sync::CancellationToken::new();
    let mut stream = provider
        .chat_stream(&[], &[], &visp_config::LlmConfig::default(), &cancel)
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
    let cancel = tokio_util::sync::CancellationToken::new();
    let mut stream = provider
        .chat_stream(&[], &[], &visp_config::LlmConfig::default(), &cancel)
        .await
        .unwrap();

    let collected: Vec<_> = stream.by_ref().collect().await;
    assert_eq!(collected.len(), 1, "empty queue should emit Done");
    assert!(matches!(&collected[0], Ok(ChatEvent::Done)));
}

/// 慢 provider：chat_stream 内部模拟阻塞 send，监听 cancel
/// ���于验证 trait 的 cancel 契约：实现方在阻塞路径监听 cancel 后立即返回 Cancelled
struct SlowProvider {
    delay: std::time::Duration,
}

#[async_trait]
impl LlmProvider for SlowProvider {
    async fn chat_stream(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _config: &LlmConfig,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<Pin<Box<dyn futures::Stream<Item = Result<ChatEvent, LlmError>> + Send>>, LlmError>
    {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(LlmError::Cancelled),
            _ = tokio::time::sleep(self.delay) => {
                let stream = stream::iter(vec![Ok(ChatEvent::Done)]);
                Ok(Box::pin(stream))
            }
        }
    }
}

#[tokio::test]
async fn test_chat_stream_cancel_returns_cancelled_within_50ms() {
    use std::time::{Duration, Instant};

    let provider = std::sync::Arc::new(SlowProvider {
        delay: Duration::from_secs(5),
    });
    let cancel = tokio_util::sync::CancellationToken::new();
    let cancel_clone = cancel.clone();
    let provider_clone = provider.clone();

    let started = Instant::now();
    let handle = tokio::spawn(async move {
        provider_clone
            .chat_stream(&[], &[], &visp_config::LlmConfig::default(), &cancel_clone)
            .await
    });

    // 50ms 后取消
    tokio::time::sleep(Duration::from_millis(50)).await;
    cancel.cancel();

    let result = tokio::time::timeout(Duration::from_millis(100), handle)
        .await
        .expect("future should complete within 100ms after cancel")
        .expect("join should not panic");

    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_millis(150),
        "expected to return within 150ms, took {:?}",
        elapsed
    );
    assert!(
        matches!(result, Err(LlmError::Cancelled)),
        "expected Err(LlmError::Cancelled), got {:?}",
        result.as_ref().err()
    );
}
