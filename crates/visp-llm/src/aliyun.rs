//! Aliyun（阿里云百炼）LLM 提供器。
//!
//! 阿里云百炼的 OpenAI 兼容 chat 与 DashScope 原生文生图使用不同的
//! base_url 和请求格式，无法用单个 OpenAI 兼容 provider 覆盖，因此独立
//! 为一个 `aliyun` protocol：
//!
//! - Chat（含识图模型）：OpenAI 兼容 `{base}/compatible-mode/v1/chat/completions`
//! - 文生图（qwen-image-*）：DashScope 原生
//!   `{base}/api/v1/services/aigc/multimodal-generation/generation`
//!
//! 配置 `base_url` 为百炼服务根地址，如
//! `https://llm-ji63pi09aiovbq89.cn-beijing.maas.aliyuncs.com`
//! （不要带 `/compatible-mode/v1` 后缀，provider 会自行拼接）。

use async_trait::async_trait;
use futures::stream::{self, Stream};
use std::pin::Pin;

use visp_config::LlmConfig;
use visp_core::error::LlmError;
use visp_core::message::{Message, ToolDefinition};
use visp_core::provider::{ChatEvent, LlmProvider};

use crate::openai::OpenAiProvider;
use crate::util::build_client;

/// DashScope 文生图端点路径（追加在 base_url 之后）。
const DASHSCOPE_IMAGE_PATH: &str = "/api/v1/services/aigc/multimodal-generation/generation";
/// OpenAI 兼容 chat 路径（追加在 base_url 之后）。
const COMPATIBLE_CHAT_PATH: &str = "/compatible-mode/v1";

/// Aliyun LLM 提供器。
pub struct AliyunProvider {
    api_key: String,
    /// 百炼服务根地址（不含 /compatible-mode/v1）
    base_url: String,
    client: reqwest::Client,
    /// 内部 OpenAI 兼容 provider，用于 chat 请求
    openai: OpenAiProvider,
}

impl AliyunProvider {
    pub fn new(api_key: String, base_url: String) -> Self {
        let openai_base = format!("{}{}", base_url.trim_end_matches('/'), COMPATIBLE_CHAT_PATH);
        Self {
            openai: OpenAiProvider::with_base_url(api_key.clone(), openai_base),
            api_key,
            base_url: base_url.trim_end_matches('/').to_string(),
            client: build_client(),
        }
    }

    /// DashScope 文生图 URL。
    fn dashscope_image_url(&self) -> String {
        format!("{}{}", self.base_url, DASHSCOPE_IMAGE_PATH)
    }

    /// 调用 DashScope 文生图 API，返回 ImageBlock 事件流。
    async fn image_generate(
        &self,
        messages: &[Message],
        config: &LlmConfig,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ChatEvent, LlmError>> + Send>>, LlmError> {
        use visp_core::message::Role;

        let prompt: String = messages
            .iter()
            .rev()
            .find(|m| m.role == Role::User)
            .map(|m| m.content.clone())
            .ok_or_else(|| LlmError::Api {
                status: 400,
                message: "No user message found for image generation prompt".to_string(),
            })?;

        // 请求体：{model, input: {messages: [{role, content: [{text}]}]}, parameters}
        let mut parameters = serde_json::json!({});
        if let Some(val) = config.extra.get("prompt_extend") {
            parameters["prompt_extend"] = serde_json::Value::String(val.clone());
        }
        let body = serde_json::json!({
            "model": config.model,
            "input": {
                "messages": [
                    {
                        "role": "user",
                        "content": [
                            { "text": prompt }
                        ]
                    }
                ]
            },
            "parameters": parameters,
        });

        let url = self.dashscope_image_url();
        let headers = crate::openai::build_openai_headers(&self.api_key);

        tracing::debug!(url = %url, model = %config.model, "aliyun image generation request");
        let send_fut = self.client.post(&url).headers(headers).json(&body).send();
        let response = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(LlmError::Cancelled),
            resp = send_fut => resp.map_err(|e| LlmError::Network(e.to_string()))?,
        };

        let status = response.status();
        if !status.is_success() {
            let body_text = response.text().await.unwrap_or_default();
            return Err(LlmError::Api {
                status: status.as_u16(),
                message: format!("Image generation API error: {}", body_text),
            });
        }

        let resp_json: serde_json::Value = response.json().await.map_err(|e| LlmError::Api {
            status: 502,
            message: format!("Failed to parse image generation response: {}", e),
        })?;

        // 响应：{output: {choices: [{message: {content: [{image}]}}]}}
        // 其中 content 数组的每一项是 {image: <url>} 或 {text: <说明>}
        let image_url = parse_dashscope_image_url(&resp_json)?;

        let events = vec![
            Ok(ChatEvent::ImageBlock {
                path: String::new(),
                mime_type: String::new(),
                remote_url: Some(image_url),
            }),
            Ok(ChatEvent::Done),
        ];

        Ok(Box::pin(stream::iter(events)))
    }
}

/// 从 DashScope 文生图响应解析图片 URL。
///
/// 实际响应结构：
/// ```json
/// {
///   "output": {
///     "choices": [{
///       "message": {
///         "content": [
///           { "image": "https://..." },
///           { "text": "..." }
///         ]
///       }
///     }]
///   }
/// }
/// ```
/// content 数组中的每一项为 {image: <url>} 或 {text: <说明>}，
/// 遍历找到第一个带 image 字段的项。
fn parse_dashscope_image_url(resp_json: &serde_json::Value) -> Result<String, LlmError> {
    let content = resp_json
        .get("output")
        .and_then(|o| o.get("choices"))
        .and_then(|c| c.get(0))
        .and_then(|choice| choice.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array());

    let Some(content) = content else {
        return Err(LlmError::Api {
            status: 502,
            message: format!("No image URL in response: {}", resp_json),
        });
    };

    for item in content {
        if let Some(url) = item.get("image").and_then(|u| u.as_str()) {
            return Ok(url.to_string());
        }
    }

    Err(LlmError::Api {
        status: 502,
        message: format!("No image URL in response: {}", resp_json),
    })
}

#[async_trait]
impl LlmProvider for AliyunProvider {
    async fn chat_stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        config: &LlmConfig,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ChatEvent, LlmError>> + Send>>, LlmError> {
        // 文生图模型：走 DashScope 原生接口
        if config.image_generation {
            return self.image_generate(messages, config, cancel).await;
        }
        // Chat / 识图模型：委托给 OpenAI 兼容 provider
        self.openai
            .chat_stream(messages, tools, config, cancel)
            .await
    }
}

#[cfg(test)]
#[path = "aliyun_tests.rs"]
mod tests;
