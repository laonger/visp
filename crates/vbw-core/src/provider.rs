use async_trait::async_trait;
use futures::stream::Stream;
use std::pin::Pin;

use crate::message::{Message, ToolDefinition};

/// LLM 配置参数
#[derive(Debug, Clone)]
pub struct LlmConfig {
    /// 模型名称
    pub model: String,
    /// 温度（0.0-2.0）
    pub temperature: f64,
    /// 最大 token 数
    pub max_tokens: u32,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            model: "claude-sonnet-4-20250514".to_string(),
            temperature: 0.7,
            max_tokens: 4096,
        }
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
    /// 流结束
    Done,
}

/// LLM 提供器抽象 trait
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// 流式对话
    /// 发送消息列表，流式接收响应
    async fn chat_stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        config: &LlmConfig,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ChatEvent, String>> + Send>>, String>;
}
