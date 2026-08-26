use crate::bigmodel::{BigModelProvider, DEFAULT_BASE_URL, clamp_temperature};

#[test]
fn test_clamp_temperature_high() {
    assert_eq!(clamp_temperature(1.5), 1.0);
}

#[test]
fn test_clamp_temperature_low() {
    assert_eq!(clamp_temperature(-0.5), 0.0);
}

#[test]
fn test_clamp_temperature_in_range() {
    assert_eq!(clamp_temperature(0.7), 0.7);
}

#[test]
fn test_default_base_url_is_versioned() {
    // 智谱 base_url 以 v4 结尾，OpenAiProvider 的 is_versioned_base_url
    // 会识别它，URL 组装为 {base}/chat/completions 而非追加 /v1。
    assert!(DEFAULT_BASE_URL.ends_with("/v4"));
}

#[test]
fn test_provider_created_via_lib() {
    // Smoke test: provider 可构造且可作为 trait 对象使用
    use std::sync::Arc;
    use visp_core::provider::LlmProvider;
    let p: Arc<dyn LlmProvider> = Arc::new(BigModelProvider::new("sk-test".to_string(), None));
    let _ = &p;
}

#[test]
fn test_provider_with_custom_base_url() {
    let p = BigModelProvider::new(
        "sk-test".to_string(),
        Some("https://example.com/v4".to_string()),
    );
    let _ = &p;
}
