use crate::opencode_go::{DEFAULT_BASE_URL, OpencodeGoProvider};

#[test]
fn test_default_base_url_is_versioned() {
    // models.dev 上 opencode-go 的 api 为 https://opencode.ai/zen/go/v1，
    // 结尾是版本段（v1），OpenAiProvider 的 is_versioned_base_url 直接拼接
    // /chat/completions，最终 URL 为
    // https://opencode.ai/zen/go/v1/chat/completions。
    let last_seg = DEFAULT_BASE_URL
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap();
    assert!(
        last_seg.starts_with('v') && last_seg[1..].chars().all(|c| c.is_ascii_digit()),
        "default base_url must end with a version segment, got: {DEFAULT_BASE_URL}"
    );
}

#[test]
fn test_provider_created_via_lib() {
    // Smoke test: provider 可构造且可作为 trait 对象使用
    use std::sync::Arc;
    use visp_core::provider::LlmProvider;
    let p: Arc<dyn LlmProvider> = Arc::new(OpencodeGoProvider::new("sk-test".to_string(), None));
    let _ = &p;
}

#[test]
fn test_provider_with_custom_base_url() {
    let p = OpencodeGoProvider::new(
        "sk-test".to_string(),
        Some("https://example.com/zen/go/v1".to_string()),
    );
    let _ = &p;
}
