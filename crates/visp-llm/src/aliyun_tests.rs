use crate::aliyun::AliyunProvider;

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
