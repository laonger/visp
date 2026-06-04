#![allow(dead_code)]

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};

static LINE_HAS_OUTPUT: AtomicBool = AtomicBool::new(false);

fn ensure_newline() {
    if LINE_HAS_OUTPUT.swap(false, Ordering::Relaxed) {
        println!();
    }
}

pub fn print_streaming(delta: &str) {
    print!("{}", delta);
    io::stdout().flush().unwrap();
    LINE_HAS_OUTPUT.store(true, Ordering::Relaxed);
}

pub fn print_tool_call(name: &str, args: &str) {
    ensure_newline();
    println!("🔧 {}({})", name, args);
}

pub fn print_tool_result(content: &str, is_error: bool) {
    ensure_newline();
    let prefix = if is_error { "❌" } else { "📄" };
    let truncated = truncate(content, 2000);
    println!("{} {}", prefix, truncated);
}

pub fn print_query(message: &str) {
    ensure_newline();
    println!("❓ {}", message);
}

pub fn print_status(message: &str) {
    ensure_newline();
    println!("{}", message);
}

pub fn print_daemon_error(code: &str, message: &str) {
    ensure_newline();
    println!("❌ Error [{}]: {}", code, message);
}

pub fn print_cli_error(message: &str) {
    ensure_newline();
    println!("❌ {}", message);
}

pub fn print_done() {
    ensure_newline();
    println!("✓");
}

pub fn truncate(content: &str, max_chars: usize) -> String {
    if content.chars().count() <= max_chars {
        content.to_string()
    } else {
        let truncated: String = content.chars().take(max_chars).collect();
        format!(
            "{}... [truncated, {} bytes total]",
            truncated,
            content.len()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_short() {
        let result = truncate("hello", 100);
        assert_eq!(result, "hello");
    }

    #[test]
    fn test_truncate_long() {
        let content = "x".repeat(2500);
        let result = truncate(&content, 2000);
        assert!(result.contains("[truncated"));
        assert!(result.len() < content.len());
    }

    #[test]
    fn test_truncate_exact_boundary() {
        let content = "a".repeat(2000);
        let result = truncate(&content, 2000);
        assert_eq!(result, content);
    }

    #[test]
    fn test_truncate_empty() {
        let result = truncate("", 100);
        assert_eq!(result, "");
    }

    #[test]
    fn test_truncate_unicode() {
        let content = "🦀".repeat(3000);
        let result = truncate(&content, 2000);
        assert!(result.contains("[truncated"));
        // Ensure no broken characters
        assert!(!result.contains('�'));
    }
}
