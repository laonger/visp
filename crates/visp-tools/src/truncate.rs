/// 默认截断上限（100KB）
pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 102400;

/// 截断工具输出，超过 max_bytes 的部分用提示信息替换
pub fn truncate_output(content: &str, max_bytes: usize) -> String {
    if content.len() > max_bytes {
        let truncated = &content[..max_bytes];
        format!(
            "{}\n... [output truncated at {} bytes]",
            truncated, max_bytes
        )
    } else {
        content.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_short_content_not_truncated() {
        let result = truncate_output("hello world", DEFAULT_MAX_OUTPUT_BYTES);
        assert_eq!(result, "hello world");
    }

    #[test]
    fn test_long_content_truncated() {
        let long = "a".repeat(DEFAULT_MAX_OUTPUT_BYTES + 1);
        let result = truncate_output(&long, DEFAULT_MAX_OUTPUT_BYTES);
        assert_ne!(result, long, "result should differ from original");
        assert!(
            result.contains("truncated at"),
            "result should contain truncation hint"
        );
        assert!(
            result.ends_with("]"),
            "result should end with truncation message"
        );
    }

    #[test]
    fn test_exact_boundary_not_truncated() {
        let exact = "a".repeat(DEFAULT_MAX_OUTPUT_BYTES);
        let result = truncate_output(&exact, DEFAULT_MAX_OUTPUT_BYTES);
        assert_eq!(result.len(), DEFAULT_MAX_OUTPUT_BYTES);
    }
}
