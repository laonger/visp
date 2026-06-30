/// 脱敏和截断工具函数，用于打印安全的 LLM generation input/output capture。
///
/// 脱敏敏感 JSON 字段
///
/// 将已知敏感字段（api_key, authorization, secret, token, password 等）的值
/// 替换为 `[REDACTED]`。同时替换明文 Bearer/sk-/pk- 等常见 secret 模式。
pub fn sanitize_json(value: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(value) {
        Ok(mut v) => {
            redact_value(&mut v);
            // Also apply pattern-based redaction on all string values
            redact_string_values(&mut v);
            serde_json::to_string(&v).unwrap_or_else(|_| simple_redact(value))
        }
        Err(_) => simple_redact(value),
    }
}

/// 对所有 JSON 字符串值应用 simple_redact（捕获 sk-/pk-/Bearer 等模式）
fn redact_string_values(v: &mut serde_json::Value) {
    match v {
        serde_json::Value::String(s) => {
            let redacted = simple_redact(s);
            if redacted != *s {
                *s = redacted;
            }
        }
        serde_json::Value::Object(map) => {
            for val in map.values_mut() {
                redact_string_values(val);
            }
        }
        serde_json::Value::Array(arr) => {
            for item in arr.iter_mut() {
                redact_string_values(item);
            }
        }
        _ => {}
    }
}

/// JSON 值递归脱敏
fn redact_value(v: &mut serde_json::Value) {
    match v {
        serde_json::Value::Object(map) => {
            let keys: Vec<String> = map.keys().cloned().collect();
            for key in keys {
                if is_sensitive_key(&key)
                    && let Some(val) = map.get_mut(&key)
                    && val.is_string()
                {
                    *val = serde_json::Value::String("[REDACTED]".to_string());
                    continue;
                }
                if let Some(val) = map.get_mut(&key) {
                    redact_value(val);
                }
            }
        }
        serde_json::Value::Array(arr) => {
            for item in arr.iter_mut() {
                redact_value(item);
            }
        }
        _ => {}
    }
}

const SENSITIVE_KEYS: &[&str] = &[
    "api_key",
    "api-key",
    "authorization",
    "secret",
    "token",
    "password",
    "passwd",
    "credential",
    "x-api-key",
    "x-auth-token",
];

fn is_sensitive_key(key: &str) -> bool {
    let lower = key.to_lowercase();
    SENSITIVE_KEYS
        .iter()
        .any(|k| lower == *k || lower.ends_with(k))
}

/// 简单字符串脱敏：替换 Bearer/sk-/pk- 等模式
fn simple_redact(s: &str) -> String {
    let mut result = s.to_string();

    // Bearer token: "Bearer xxxxx"  where xxxxx ≥ 6 chars
    result = replace_bearer_token(&result);

    // sk- / pk- / sv- prefixed keys (common for API keys), length ≥ 8
    result = replace_prefixed_secret(&result, "sk-");
    result = replace_prefixed_secret(&result, "pk-");
    result = replace_prefixed_secret(&result, "sv-");

    result
}

/// 替换 `Bearer <token>` 模式（token ≥ 6 字符）
fn replace_bearer_token(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(pos) = rest.to_lowercase().find("bearer ") {
        // Keep everything before "bearer "
        result.push_str(&rest[..pos + 7]); // "bearer " = 7 chars
        rest = &rest[pos + 7..];

        // Find the end of the token (whitespace, quote, comma, closing paren/bracket/brace, or end)
        let token_len = rest
            .chars()
            .take_while(|&c| {
                !c.is_whitespace() && c != '"' && c != ',' && c != ')' && c != ']' && c != '}'
            })
            .count();
        if token_len >= 6 {
            result.push_str("[REDACTED]");
            rest = &rest[token_len..];
        } else {
            // Short token — might not be a real secret, keep as-is
            // (we already included the prefix, so just continue)
        }
    }
    result.push_str(rest);
    result
}

/// 替换 `prefix<token>` 模式（token ≥ 8 字符，且后跟非字母数字或结尾）
fn replace_prefixed_secret(s: &str, prefix: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(pos) = rest.find(prefix) {
        result.push_str(&rest[..pos]);
        rest = &rest[pos + prefix.len()..];

        // Check that prefix is at a word boundary (preceded by non-alphanumeric or start)
        if pos > 0 {
            let prev = result.chars().last().unwrap();
            if prev.is_alphanumeric() || prev == '_' || prev == '-' {
                // Not a real prefix boundary, keep the prefix
                result.push_str(prefix);
                continue;
            }
        }

        // Count token chars (alphanumeric, underscore, hyphen, dot)
        let token_len = rest
            .chars()
            .take_while(|&c| c.is_alphanumeric() || c == '_' || c == '-' || c == '.')
            .count();
        if token_len >= 8 {
            result.push_str("[REDACTED]");
            rest = &rest[token_len..];
        } else {
            result.push_str(prefix);
        }
    }
    result.push_str(rest);
    result
}

/// UTF-8 安全截断。
///
/// 最多保留 `max_chars` 个字符。如果被截断，追加 `…[truncated]` 标记。
pub fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let truncated: String = s.chars().take(max_chars).collect();
    format!("{}…[truncated]", truncated)
}

/// 组合：先脱敏，再截断。
pub fn sanitize_and_truncate(s: &str, max_chars: usize, redact: bool) -> String {
    let cleaned = if redact {
        sanitize_json(s)
    } else {
        s.to_string()
    };
    truncate(&cleaned, max_chars)
}

/// 构建 Langfuse observation span 的 input 字段值。
///
/// 输入是完整的请求 body JSON。输出经过 sanitize 处理。
pub fn format_langfuse_input(body: &serde_json::Value, max_chars: usize, redact: bool) -> String {
    let json_str = serde_json::to_string(body).unwrap_or_default();
    sanitize_and_truncate(&json_str, max_chars, redact)
}

/// 构建 Langfuse observation span 的 output 字段值。
///
/// 输入是最终 assistant 文本。输出经过 sanitize 处理。
pub fn format_langfuse_output(text: &str, max_chars: usize, redact: bool) -> String {
    sanitize_and_truncate(text, max_chars, redact)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── truncate tests ───────────────────────────────────────────────────────

    #[test]
    fn test_truncate_short_string() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_exact() {
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn test_truncate_adds_marker() {
        let result = truncate("hello world", 5);
        assert_eq!(result, "hello…[truncated]");
    }

    #[test]
    fn test_truncate_utf8_safe() {
        let s = "你好世界abcd";
        let result = truncate(s, 4);
        // First 4 chars: "你好世界" → truncated, so result is "你好世界…[truncated]"
        assert_eq!(result, "你好世界…[truncated]");
    }

    #[test]
    fn test_truncate_utf8_boundary() {
        let s = "你好世";
        // 3 chars, limit 4 → no truncation
        let result = truncate(s, 4);
        assert_eq!(result, "你好世");
    }

    // ── sanitize_json tests ─────────────────────────────────────────────────

    #[test]
    fn test_redact_api_key_field() {
        let input = r#"{"api_key": "sk-abc123xyz", "model": "gpt-4"}"#;
        let result = sanitize_json(input);
        assert!(result.contains(r#""[REDACTED]"#));
        assert!(result.contains("gpt-4"));
        assert!(!result.contains("sk-abc123xyz"));
    }

    #[test]
    fn test_redact_authorization_field() {
        let input = r#"{"authorization": "Bearer token12345", "model": "claude"}"#;
        let result = sanitize_json(input);
        assert!(result.contains(r#""[REDACTED]"#));
        assert!(!result.contains("Bearer"));
    }

    #[test]
    fn test_redact_nested_sensitive_field() {
        let input = r#"{"config": {"api_key": "sk-secret", "name": "test"}, "model": "claude"}"#;
        let result = sanitize_json(input);
        assert!(result.contains(r#""[REDACTED]"#));
        assert!(result.contains("test"));
        assert!(!result.contains("sk-secret"));
    }

    #[test]
    fn test_redact_array_nested() {
        let input = r#"{"models": [{"name": "gpt-4", "api_key": "sk-xxx"}]}"#;
        let result = sanitize_json(input);
        assert!(result.contains(r#""[REDACTED]"#));
        assert!(result.contains("gpt-4"));
        assert!(!result.contains("sk-xxx"));
    }

    #[test]
    fn test_redact_bearer_in_string() {
        let input = r#"some text with Bearer sk-abc123xyz and stuff"#;
        let result = simple_redact(input);
        assert!(!result.contains("sk-abc123xyz"));
        assert!(result.contains("[REDACTED]"));
    }

    #[test]
    fn test_redact_non_json_string() {
        let input = "Authorization: Bearer tok_abcdef123456\napi_key=sk-secret-key";
        let result = sanitize_json(input);
        // Falls back to simple_redact for non-JSON
        assert!(result.contains("[REDACTED]"));
        assert!(!result.contains("tok_abcdef123456"));
    }

    #[test]
    fn test_sanitize_sk_prefix() {
        let input = r#"{"key": "sk-test-key-value"}"#;
        let result = sanitize_json(input);
        // "key" is not in sensitive keys list, but redact_string_values applies
        // simple_redact to all string values, catching sk- patterns
        assert!(
            !result.contains("sk-test-key-value"),
            "sk- value should be redacted"
        );
        assert!(result.contains("[REDACTED]"));
    }

    #[test]
    fn test_sanitize_and_truncate_combined() {
        let input = r#"{"api_key": "sk-very-secret", "output": "hello world this is a long text"}"#;
        let result = sanitize_and_truncate(input, 30, true);
        assert!(result.contains("[REDACTED]"));
        assert!(result.contains("[truncated]") || result.len() < input.len());
    }

    #[test]
    fn test_no_redact_when_disabled() {
        let input = r#"{"api_key": "sk-secret", "model": "gpt-4"}"#;
        let result = sanitize_and_truncate(input, 100, false);
        assert!(result.contains("sk-secret"));
    }

    #[test]
    fn test_format_langfuse_input_with_system_prompt() {
        let body = serde_json::json!({
            "model": "claude-sonnet-4",
            "system": [{"type": "text", "text": "You are a helpful assistant."}],
            "messages": [{"role": "user", "content": "Hi"}],
            "max_tokens": 4096,
        });
        let result = format_langfuse_input(&body, 10000, true);
        assert!(result.contains("You are a helpful assistant."));
        assert!(result.contains("claude-sonnet-4"));
    }

    #[test]
    fn test_format_langfuse_output_simple() {
        let result = format_langfuse_output("Hello, how can I help?", 10000, true);
        assert_eq!(result, "Hello, how can I help?");
    }
}
