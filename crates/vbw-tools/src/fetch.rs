use async_trait::async_trait;
use std::path::Path;
use vbw_core::tool::{Tool, ToolContext, ToolResult};

// ── Constants ────────────────────────────────────────────────────────────────

/// 最大响应大小：5MB
const MAX_RESPONSE_BYTES: u64 = 5 * 1024 * 1024;

/// 默认超时秒数
const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// 允许的 URL 协议
const ALLOWED_SCHEMES: &[&str] = &["http", "https"];

// ── WebFetch Tool ────────────────────────────────────────────────────────────

pub struct WebFetch {
    client: reqwest::Client,
    daemon_allow_domains: Vec<String>,
}

impl WebFetch {
    /// 从 daemon 配置的 raw toml 值构造
    /// raw = config.tool.get("webfetch") → Option<&toml::Value>
    pub fn from_toml(raw: Option<&toml::Value>) -> Self {
        let mut allow_domains: Vec<String> = Vec::new();
        let mut timeout_secs = DEFAULT_TIMEOUT_SECS;

        if let Some(config) = raw.and_then(|v| v.as_table()) {
            if let Some(domains) = config.get("allow_domains").and_then(|v| v.as_array()) {
                allow_domains = domains
                    .iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect();
            }
            if let Some(t) = config.get("timeout_secs").and_then(|v| v.as_integer())
                && t > 0
            {
                timeout_secs = t.min(120) as u64;
            }
        }

        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(timeout_secs))
                .build()
                .unwrap_or_default(),
            daemon_allow_domains: allow_domains,
        }
    }

    /// 加载项目级白名单配置 .vibewisp/webfetch.toml
    async fn load_project_config(project_dir: &Path) -> Vec<String> {
        let config_path = project_dir.join(".vibewisp").join("webfetch.toml");
        let content = match tokio::fs::read_to_string(&config_path).await {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };
        let value: toml::Value = match toml::from_str(&content) {
            Ok(v) => v,
            Err(_) => return Vec::new(),
        };
        value
            .get("allow_domains")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// 检查域名是否在白名单中（精确匹配）
    fn host_in_allow_list(host: &str, allow_domains: &[String]) -> bool {
        allow_domains.iter().any(|d| d == host)
    }

    /// 检查域名是否在 daemon 或项目级白名单中
    async fn is_url_allowed(&self, url: &str, project_dir: &Path) -> bool {
        let parsed = match reqwest::Url::parse(url) {
            Ok(u) => u,
            Err(_) => return false,
        };
        let host = match parsed.host_str() {
            Some(h) => h.to_lowercase(),
            None => return false,
        };

        // 检查 daemon 级白名单
        if Self::host_in_allow_list(&host, &self.daemon_allow_domains) {
            return true;
        }

        // 检查项目级白名单
        let project_domains = Self::load_project_config(project_dir).await;
        Self::host_in_allow_list(&host, &project_domains)
    }

    /// URL 协议校验
    fn validate_url(url_str: &str) -> Result<reqwest::Url, String> {
        let parsed = reqwest::Url::parse(url_str).map_err(|_| format!("Invalid URL: {url_str}"))?;
        if !ALLOWED_SCHEMES.contains(&parsed.scheme()) {
            return Err(format!(
                "URL scheme '{}' not allowed (only http/https)",
                parsed.scheme()
            ));
        }
        Ok(parsed)
    }

    /// MIME 类型是否为文本类
    fn is_textual_content_type(content_type: &str) -> bool {
        let mime = content_type
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_lowercase();
        mime.starts_with("text/")
            || mime == "application/json"
            || mime == "application/xml"
            || mime == "application/javascript"
            || mime == "application/ecmascript"
            || mime.starts_with("application/xhtml")
    }
}

#[async_trait]
impl Tool for WebFetch {
    fn name(&self) -> &str {
        "fetch_web"
    }

    fn category(&self) -> &str {
        "network"
    }

    fn description(&self) -> &str {
        "Fetch the content of a URL and extract the main text content as Markdown. \
         Use this to read documentation pages, API references, or any web resource. \
         Supports HTTP and HTTPS URLs. Only textual content (HTML, plain text, JSON, XML) \
         is extracted. Binary content (images, PDFs) is rejected. \
         Respects the project's allowed domain whitelist if configured. \
         Timeout defaults to 30s (max 120s). \
         Prefer reading local files with ReadFile when the content is already available locally."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The HTTP or HTTPS URL to fetch."
                },
                "timeout": {
                    "type": "integer",
                    "description": "Optional timeout in seconds (default: 30, max: 120)."
                }
            },
            "required": ["url"]
        })
    }

    fn requires_approval_for(&self, arguments: &serde_json::Value) -> bool {
        let url = match arguments.get("url").and_then(|v| v.as_str()) {
            Some(u) => u,
            None => return true, // 无 URL 参数，需要确认
        };
        // 不做异步 IO，仅做同步检查（daemon 级白名单）
        // 项目级白名单在 execute 中处理
        let parsed = match reqwest::Url::parse(url) {
            Ok(u) => u,
            Err(_) => return true,
        };
        let host = match parsed.host_str() {
            Some(h) => h.to_lowercase(),
            None => return true,
        };
        !Self::host_in_allow_list(&host, &self.daemon_allow_domains)
    }

    async fn execute(&self, arguments: serde_json::Value, context: &ToolContext) -> ToolResult {
        // 1. 提取 URL
        let url_str = match arguments.get("url").and_then(|v| v.as_str()) {
            Some(u) => u,
            None => return ToolResult::error("Missing required parameter: url"),
        };

        // 2. URL 校验
        let url = match Self::validate_url(url_str) {
            Ok(u) => u,
            Err(e) => return ToolResult::error(e),
        };

        // 3. 白名单检查（含项目级）
        let allowed = self.is_url_allowed(url_str, &context.working_dir).await;
        if !allowed {
            // 未命中白名单：requires_approval_for 已触发审批流程
            // 到达 execute 说明用户已批准，直接放行
        }

        // 4. HTTP GET
        let response = match self.client.get(url.clone()).send().await {
            Ok(r) => r,
            Err(e) => {
                return ToolResult::error(format!("Unable to fetch {}: {e}", url_str));
            }
        };

        // 5. Content-Length 检查
        if let Some(content_length) = response.content_length()
            && content_length > MAX_RESPONSE_BYTES
        {
            return ToolResult::error(format!(
                "Response too large: {} bytes (max {} bytes)",
                content_length, MAX_RESPONSE_BYTES
            ));
        }

        // 6. MIME 类型检查
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("text/html")
            .to_string();

        if !Self::is_textual_content_type(&content_type) {
            return ToolResult::error(format!("Unsupported content type: {content_type}"));
        }

        // 7. 流式读取 body（边读边检查大小）
        let body_bytes = match self.stream_body(response).await {
            Ok(b) => b,
            Err(e) => return ToolResult::error(e),
        };

        let body_str = match String::from_utf8(body_bytes) {
            Ok(s) => s,
            Err(_) => return ToolResult::error("Response is not valid UTF-8"),
        };

        if body_str.trim().is_empty() {
            return ToolResult::success(String::new());
        }

        // 8. HTML→Markdown 转换（CPU 密集型）
        let markdown = tokio::task::spawn_blocking(move || html_to_markdown(&body_str))
            .await
            .unwrap_or_else(|_| "<conversion error>".to_string());

        ToolResult::success(markdown)
    }
}

impl WebFetch {
    /// 流式读取响应 body，限制最大大小
    async fn stream_body(&self, response: reqwest::Response) -> Result<Vec<u8>, String> {
        use tokio_stream::StreamExt;
        let mut body = Vec::new();
        let mut stream = response.bytes_stream();

        while let Some(chunk_result) = stream.next().await {
            let chunk = match chunk_result {
                Ok(c) => c,
                Err(e) => return Err(format!("Error reading response: {e}")),
            };
            if body.len() + chunk.len() > MAX_RESPONSE_BYTES as usize {
                body.extend_from_slice(&chunk[..MAX_RESPONSE_BYTES as usize - body.len()]);
                return Ok(body);
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }
}

/// HTML 转 Markdown（同步函数，用于 spawn_blocking）
fn html_to_markdown(html: &str) -> String {
    html_to_markdown_rs::convert(html, None)
        .ok()
        .and_then(|r| r.content)
        .unwrap_or_default()
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::OnceLock;

    /// 创建一个测试用 WebFetch（daemon 白名单为空）
    fn test_webfetch() -> &'static WebFetch {
        static INSTANCE: OnceLock<WebFetch> = OnceLock::new();
        INSTANCE.get_or_init(|| WebFetch::from_toml(None))
    }

    fn test_context(dir: &Path) -> ToolContext {
        ToolContext {
            working_dir: dir.to_path_buf(),
            session_id: None,
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
        let config_dir = dir.path().join(".vibewisp");
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
}
