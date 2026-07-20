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
#[path = "truncate_tests.rs"]
mod tests;
