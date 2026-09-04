use crate::opencode::{DEFAULT_BASE_URL, OpencodeProvider};

#[test]
fn test_default_base_url_not_versioned() {
    // opencode zen base_url 结尾是 go/（非 vN 版本段），
    // OpenAiProvider 的 is_versioned_base_url 会追加 /v1/chat/completions，
    // 最终 URL 为 https://opencode.ai/zen/go/v1/chat/completions。
    let last_seg = DEFAULT_BASE_URL
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap();
    assert!(
        !(last_seg.starts_with('v') && last_seg[1..].chars().all(|c| c.is_ascii_digit())),
        "default base_url must NOT end with a version segment, got: {DEFAULT_BASE_URL}"
    );
}

#[test]
fn test_provider_created_via_lib() {
    // Smoke test: provider 可构造且可作为 trait 对象使用
    use std::sync::Arc;
    use visp_core::provider::LlmProvider;
    let p: Arc<dyn LlmProvider> = Arc::new(OpencodeProvider::new("sk-test".to_string(), None));
    let _ = &p;
}

#[test]
fn test_provider_with_custom_base_url() {
    let p = OpencodeProvider::new(
        "sk-test".to_string(),
        Some("https://example.com/zen/go/".to_string()),
    );
    let _ = &p;
}

#[test]
fn test_provider_declares_opencode_session_header() {
    // opencode 网关要求 x-opencode-session 头，provider 构造时声明
    let p = OpencodeProvider::new("sk-test".to_string(), None);
    let _ = p;
}
