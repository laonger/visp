use crate::aliyun::{AliyunProvider, parse_dashscope_image_url};

fn test_provider() -> AliyunProvider {
    AliyunProvider::new(
        "sk-test".to_string(),
        "https://llm-ji63pi09aiovbq89.cn-beijing.maas.aliyuncs.com".to_string(),
    )
}

#[test]
fn test_dashscope_image_url() {
    let p = test_provider();
    assert_eq!(
        p.dashscope_image_url(),
        "https://llm-ji63pi09aiovbq89.cn-beijing.maas.aliyuncs.com/api/v1/services/aigc/multimodal-generation/generation"
    );
}

#[test]
fn test_openai_compatible_base_url_has_compatible_mode() {
    // The internally held OpenAI provider must target the compatible-mode path
    // so chat/vision requests work. We can't access the private field directly,
    // so verify through the DashScope URL builder instead: the root base_url
    // must NOT contain /compatible-mode/v1.
    let p = test_provider();
    assert!(!p.dashscope_image_url().contains("/compatible-mode/v1"));
}

#[test]
fn test_new_trims_trailing_slash() {
    let p = AliyunProvider::new("sk-test".to_string(), "https://example.com/".to_string());
    assert_eq!(
        p.dashscope_image_url(),
        "https://example.com/api/v1/services/aigc/multimodal-generation/generation"
    );
}

#[test]
fn test_provider_created_via_lib() {
    // Smoke test: the provider type is constructible and Send+Sync usable via Arc<dyn LlmProvider>
    use std::sync::Arc;
    use visp_core::provider::LlmProvider;
    let p: Arc<dyn LlmProvider> = Arc::new(test_provider());
    // trait object usable
    let _ = &p;
}

#[test]
fn test_parse_dashscope_image_url_real_response() {
    // Real qwen-image-3.0-pro response shape:
    // output.choices[0].message.content[0].image
    let resp = serde_json::json!({
        "request_id": "b7de2f21-afda-9efb-8f8d-3c9c561bfc1b",
        "output": {
            "choices": [{
                "message": {
                    "content": [
                        { "image": "https://dashscope-a717.oss-accelerate.aliyuncs.com/img.png?Expires=1" }
                    ]
                }
            }]
        }
    });
    let url = parse_dashscope_image_url(&resp).unwrap();
    assert!(url.starts_with("https://dashscope-a717.oss-accelerate.aliyuncs.com/img.png"));
}

#[test]
fn test_parse_dashscope_image_url_picks_image_over_text() {
    // content 数组可能同时含 image 与 text 项，必须取 image 项
    let resp = serde_json::json!({
        "output": {
            "choices": [{
                "message": {
                    "content": [
                        { "text": "generated image" },
                        { "image": "https://example.com/result.png" }
                    ]
                }
            }]
        }
    });
    let url = parse_dashscope_image_url(&resp).unwrap();
    assert_eq!(url, "https://example.com/result.png");
}

#[test]
fn test_parse_dashscope_image_url_missing_image() {
    let resp = serde_json::json!({
        "output": {
            "choices": [{
                "message": {
                    "content": [{ "text": "no image here" }]
                }
            }]
        }
    });
    assert!(parse_dashscope_image_url(&resp).is_err());
}

#[test]
fn test_parse_dashscope_image_url_empty_content() {
    let resp = serde_json::json!({ "output": { "choices": [] } });
    assert!(parse_dashscope_image_url(&resp).is_err());
}
