use super::*;
use std::sync::OnceLock;

/// 创建一个测试用 WebFetch（daemon 白名单为空）
fn test_webfetch() -> &'static WebFetch {
    static INSTANCE: OnceLock<WebFetch> = OnceLock::new();
    INSTANCE.get_or_init(|| {
        // reqwest 0.13+ 会读取 macOS 等系统代理设置；本机若开启系统代理
        // 会拦截对 127.0.0.1 测试服务器的请求，导致超时测试不稳定。
        // 测试夹具统一禁用代理，确保直连本地服务器。
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .unwrap();
        WebFetch {
            client,
            daemon_allow_domains: Vec::new(),
        }
    })
}

fn test_context(dir: &Path) -> ToolContext {
    ToolContext {
        working_dir: dir.to_path_buf(),
        session_id: None,
        permission_rules: None,
        global_tx: None,
        visp_trace_id: None,
        iter_span_w3c_id: None,
    }
}

// ── URL 验证 ──────────────────────────────────────────────────────────

#[test]
fn test_validate_url_https() {
    let result = WebFetch::validate_url("https://example.com");
    assert!(result.is_ok());
}

#[test]
fn test_validate_url_http() {
    let result = WebFetch::validate_url("http://example.com:8080/path?q=1");
    assert!(result.is_ok());
}

#[test]
fn test_validate_url_file() {
    let result = WebFetch::validate_url("file:///etc/passwd");
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("not allowed"));
}

#[test]
fn test_validate_url_ftp() {
    let result = WebFetch::validate_url("ftp://files.example.com");
    assert!(result.is_err());
}

#[test]
fn test_validate_url_empty() {
    let result = WebFetch::validate_url("");
    assert!(result.is_err());
}

#[test]
fn test_validate_url_invalid() {
    let result = WebFetch::validate_url("not a url");
    assert!(result.is_err());
}

// ── MIME 类型检查 ────────────────────────────────────────────────────

#[test]
fn test_is_textual_mime_html() {
    assert!(WebFetch::is_textual_content_type("text/html"));
}

#[test]
fn test_is_textual_mime_plain() {
    assert!(WebFetch::is_textual_content_type(
        "text/plain; charset=utf-8"
    ));
}

#[test]
fn test_is_textual_mime_json() {
    assert!(WebFetch::is_textual_content_type("application/json"));
}

#[test]
fn test_is_textual_mime_xml() {
    assert!(WebFetch::is_textual_content_type("application/xml"));
}

#[test]
fn test_is_textual_mime_png() {
    assert!(!WebFetch::is_textual_content_type("image/png"));
}

#[test]
fn test_is_textual_mime_pdf() {
    assert!(!WebFetch::is_textual_content_type("application/pdf"));
}

#[test]
fn test_is_textual_mime_octet() {
    assert!(!WebFetch::is_textual_content_type(
        "application/octet-stream"
    ));
}

// ── 白名单匹配 ────────────────────────────────────────────────────────

#[test]
fn test_host_in_allow_list_match() {
    assert!(WebFetch::host_in_allow_list(
        "example.com",
        &["example.com".into()]
    ));
}

#[test]
fn test_host_in_allow_list_no_match() {
    assert!(!WebFetch::host_in_allow_list(
        "evil.com",
        &["example.com".into()]
    ));
}

#[test]
fn test_host_in_allow_list_empty() {
    assert!(!WebFetch::host_in_allow_list("example.com", &[]));
}

#[test]
fn test_host_in_allow_list_subdomain() {
    // 精确匹配，子域名不匹配
    assert!(!WebFetch::host_in_allow_list(
        "sub.example.com",
        &["example.com".into()]
    ));
}

// ── from_toml ─────────────────────────────────────────────────────────

#[test]
fn test_from_toml_empty() {
    let wf = WebFetch::from_toml(None);
    assert!(wf.daemon_allow_domains.is_empty());
}

#[test]
fn test_from_toml_with_domains() {
    let toml_str = r#"allow_domains = ["docs.rs", "crates.io"]"#;
    let value: toml::Value = toml::from_str(toml_str).unwrap();
    let wf = WebFetch::from_toml(Some(&value));
    assert_eq!(wf.daemon_allow_domains, vec!["docs.rs", "crates.io"]);
}

#[test]
fn test_from_toml_with_timeout() {
    let toml_str = "timeout_secs = 60";
    let value: toml::Value = toml::from_str(toml_str).unwrap();
    // 验证构造不 panic，超时用客户端测试
    let _wf = WebFetch::from_toml(Some(&value));
}

#[test]
fn test_from_toml_timeout_capped() {
    let toml_str = "timeout_secs = 200";
    let value: toml::Value = toml::from_str(toml_str).unwrap();
    // 验证 caps 不 panic
    let _wf = WebFetch::from_toml(Some(&value));
}

// ── requires_approval_for ─────────────────────────────────────────────

#[test]
fn test_requires_approval_for_whitelisted() {
    let toml_str = r#"allow_domains = ["trusted.com"]"#;
    let value: toml::Value = toml::from_str(toml_str).unwrap();
    let wf = WebFetch::from_toml(Some(&value));
    let args = serde_json::json!({"url": "https://trusted.com/page"});
    assert!(!wf.requires_approval_for(&args));
}

#[test]
fn test_requires_approval_for_not_whitelisted() {
    let toml_str = r#"allow_domains = ["trusted.com"]"#;
    let value: toml::Value = toml::from_str(toml_str).unwrap();
    let wf = WebFetch::from_toml(Some(&value));
    let args = serde_json::json!({"url": "https://evil.com"});
    assert!(wf.requires_approval_for(&args));
}

#[test]
fn test_requires_approval_for_empty_whitelist() {
    let wf = WebFetch::from_toml(None);
    let args = serde_json::json!({"url": "https://example.com"});
    assert!(wf.requires_approval_for(&args));
}

#[test]
fn test_requires_approval_for_missing_url() {
    let wf = WebFetch::from_toml(None);
    let args = serde_json::json!({});
    assert!(wf.requires_approval_for(&args));
}

// ── HTML→Markdown ────────────────────────────────────────────────────

#[test]
fn test_html_to_markdown_basic() {
    let md = html_to_markdown("<h1>Hello</h1>");
    assert!(md.contains("Hello"));
}

#[test]
fn test_html_to_markdown_empty() {
    let md = html_to_markdown("");
    assert!(md.is_empty());
}

#[test]
fn test_html_to_markdown_no_html() {
    let md = html_to_markdown("hello world");
    assert!(md.contains("hello world"));
}

// ── 项目级配置加载 ────────────────────────────────────────────────────

#[tokio::test]
async fn test_load_project_config_exists() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path().join(".visp");
    tokio::fs::create_dir_all(&config_dir).await.unwrap();
    tokio::fs::write(
        config_dir.join("webfetch.toml"),
        r#"allow_domains = ["project.local"]"#,
    )
    .await
    .unwrap();

    let domains = WebFetch::load_project_config(dir.path()).await;
    assert_eq!(domains, vec!["project.local"]);
}

#[tokio::test]
async fn test_load_project_config_not_exists() {
    let dir = tempfile::tempdir().unwrap();
    let domains = WebFetch::load_project_config(dir.path()).await;
    assert!(domains.is_empty());
}

// ── 完整工具执行（仅测错误路径，不发起 HTTP）────────────────────────

#[tokio::test]
async fn test_execute_missing_url() {
    let wf = test_webfetch();
    let ctx = test_context(Path::new("/tmp"));
    let result = wf.execute(serde_json::json!({}), &ctx).await;
    assert!(result.is_error);
    assert!(result.content.contains("url"));
}

#[tokio::test]
async fn test_execute_invalid_url() {
    let wf = test_webfetch();
    let ctx = test_context(Path::new("/tmp"));
    let result = wf
        .execute(serde_json::json!({"url": "not a url"}), &ctx)
        .await;
    assert!(result.is_error);
}

#[tokio::test]
async fn test_execute_file_url() {
    let wf = test_webfetch();
    let ctx = test_context(Path::new("/tmp"));
    let result = wf
        .execute(serde_json::json!({"url": "file:///etc/passwd"}), &ctx)
        .await;
    assert!(result.is_error);
    assert!(result.content.contains("not allowed"));
}

#[tokio::test]
async fn test_execute_ftp_url() {
    let wf = test_webfetch();
    let ctx = test_context(Path::new("/tmp"));
    let result = wf
        .execute(serde_json::json!({"url": "ftp://example.com"}), &ctx)
        .await;
    assert!(result.is_error);
}

// ── per-call timeout 生效 ─────────────────────────────────────────────

#[tokio::test]
async fn test_execute_per_call_timeout_applied() {
    // 起一个接受连接但永不返回 HTTP 响应的本地服务器
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        // 接受连接后挂起，不写任何响应
        let _ = listener.accept().await;
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
    });

    let wf = test_webfetch();
    let ctx = test_context(Path::new("/tmp"));
    let url = format!("http://{addr}/slow");
    let started = std::time::Instant::now();
    let result = wf
        .execute(serde_json::json!({"url": url, "timeout": 1}), &ctx)
        .await;
    let elapsed = started.elapsed();

    assert!(result.is_error);
    assert!(
        result.content.contains("Unable to fetch"),
        "期望超时错误，got: {:?}",
        result.content
    );
    assert!(
        elapsed < std::time::Duration::from_secs(10),
        "per-call timeout 未生效，耗时 {elapsed:?}"
    );
}
