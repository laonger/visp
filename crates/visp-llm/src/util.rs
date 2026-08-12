/// 从 HTTP 响应头解析 `Retry-After` 秒数
pub fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
}

/// 构建共享的 reqwest Client（含超时配置）
///
/// 不设置总请求超时：流式响应可能持续很久，且 mid-stream stall 由
/// TCP keepalive 负责检测，总超时会导致长响应被误杀。
pub fn build_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(30))
        .tcp_keepalive(std::time::Duration::from_secs(60))
        .build()
        .expect("failed to build reqwest Client")
}
