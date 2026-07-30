use ratatui::{
    style::Style,
    text::{Line, Span},
};

use crate::app::{ChatLine, LineType, highlight_code_block, pad_to_width, wrap_text};
use crate::theme;

// ════════════════════════════════════════════════════════════════
// Helper functions (moved from app.rs)
// ════════════════════════════════════════════════════════════════

/// 工具名称 → emoji 图标映射
pub fn tool_icon(name: &str) -> &'static str {
    match name {
        "read_file" | "read_files" => "📖",
        "write_file" => "📝",
        "edit_file" => "✏️",
        "grep" => "🔍",
        "glob" => "📂",
        "bash" | "cmd" | "powershell" => "💻",
        "fetch_web" | "fetch" => "🌐",
        n if n.starts_with("codegraph_") => "🔎",
        _ => "🔧",
    }
}

/// 根据工具名获取结果最大行数
/// - None = 不截断
/// - Some(0) = 不显示内容
/// - Some(N) = 最多 N 行
pub fn max_lines_for_tool(name: &str) -> Option<usize> {
    match name {
        "read_file" | "read_files" => Some(0),
        "edit_file" | "write_file" => None,
        "bash" | "cmd" | "powershell" => Some(30),
        "grep" => Some(20),
        "glob" => Some(15),
        "fetch_web" | "fetch" => Some(20),
        n if n.starts_with("codegraph_") => Some(20),
        _ => Some(20),
    }
}

/// read_file 类工具的摘要行
pub fn result_summary(name: &str, content: &str) -> String {
    match name {
        "read_file" | "read_files" => {
            let lines = content.lines().count();
            let bytes = content.len();
            format!("Read {} bytes ({} lines)", bytes, lines)
        }
        "write_file" => {
            // WriteFile 成功结果格式: "Written N bytes to PATH\n<content>"
            // 闭合态摘要：显示写入的字节数和行数
            if let Some(first_line) = content.lines().next() {
                // 尝试从 "Written N bytes to PATH" 提取字节数
                if let Some(bytes) = first_line
                    .strip_prefix("Written ")
                    .and_then(|s| s.split_whitespace().next())
                    .and_then(|s| s.parse::<usize>().ok())
                {
                    let line_count = content.lines().count().saturating_sub(1); // 减去首行
                    format!("Written {} bytes ({} lines)", bytes, line_count)
                } else {
                    String::new()
                }
            } else {
                String::new()
            }
        }
        "edit_file" => {
            // EditFile 成功结果格式: "Replaced 1 occurrence in PATH\n<unified diff>"
            // 闭合态摘要：显示 diff 行数
            if content.starts_with("Replaced") {
                let diff_lines = content.lines().count().saturating_sub(1); // 减去首行 "Replaced..."
                format!("Diff {} lines", diff_lines)
            } else {
                String::new()
            }
        }
        _ => String::new(),
    }
}

/// Format tool arguments as a concise summary string for display in the collapsible header.
pub(crate) fn format_tool_args_summary(tool_name: &str, args_json: &str) -> String {
    match tool_name {
        "read_file" | "read_files" => {
            if let Ok(args) = serde_json::from_str::<serde_json::Value>(args_json) {
                let path = args.get("path").and_then(|v| v.as_str())
                    .or_else(|| args.get("paths").and_then(|v| v.as_str()))
                    .unwrap_or("?");
                let start = args.get("start_line").and_then(|v| v.as_i64());
                let end = args.get("end_line").and_then(|v| v.as_i64());
                match (start, end) {
                    (Some(s), Some(e)) => format!("{}:{}-{}", path, s, e),
                    (Some(s), None) => format!("{}:{}-", path, s),
                    (None, Some(e)) => format!("{}:-{}", path, e),
                    (None, None) => path.to_string(),
                }
            } else {
                args_json.to_string()
            }
        }
        "write_file" => {
            if let Ok(args) = serde_json::from_str::<serde_json::Value>(args_json) {
                let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("?");
                let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
                let lines = content.lines().count();
                format!("{}: {}", path, lines)
            } else {
                args_json.to_string()
            }
        }
        "edit_file" => {
            if let Ok(args) = serde_json::from_str::<serde_json::Value>(args_json) {
                let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("?");
                let new_string = args.get("new_string").and_then(|v| v.as_str()).unwrap_or("");
                let lines = new_string.lines().count();
                format!("{}: {}", path, lines)
            } else {
                args_json.to_string()
            }
        }
        _ => {
            if let Ok(args) = serde_json::from_str::<serde_json::Value>(args_json) {
                if let Some(obj) = args.as_object() {
                    let parts: Vec<String> = obj.iter().map(|(k, v)| {
                        let val_str = match v {
                            serde_json::Value::String(s) => s.clone(),
                            _ => v.to_string(),
                        };
                        let char_count = val_str.chars().count();
                        if char_count > 40 {
                            let truncated: String = val_str.chars().take(37).collect();
                            format!("{}:{}...", k, truncated)
                        } else {
                            format!("{}:{}", k, val_str)
                        }
                    }).collect();
                    parts.join(" ")
                } else {
                    args_json.to_string()
                }
            } else {
                args_json.to_string()
            }
        }
    }
}

// ════════════════════════════════════════════════════════════════
// Tool rendering functions (extracted from MessageCache::from_message)
// ════════════════════════════════════════════════════════════════

/// Render a ToolCall message block.
pub(crate) fn render_tool_call(msg: &ChatLine, width: u16, expanded: bool) -> Vec<Line<'static>> {
    let name = match &msg.line_type {
        LineType::ToolCall { name } => name.as_str(),
        _ => return Vec::new(),
    };

    let icon = tool_icon(name);
    let mut lines = Vec::new();

    // Parse arguments for summary header
    let args_summary = format_tool_args_summary(name, &msg.content);
    let header = format!("{} {} {}", icon, name, args_summary);
    let header_wrapped = wrap_text(&header, width);

    for dl in header_wrapped.iter() {
        let content = if dl.is_empty() {
            " ".repeat(width as usize)
        } else {
            pad_to_width(dl, width as usize)
        };
        lines.push(Line::styled(content, Style::default().fg(theme::TOOL_CALL_FG)));
    }

    // If we have a merged result, show it
    if let Some(ref result) = msg.tool_result {
        if expanded {
            // Expanded: show empty separator + full result content
            lines.push(Line::styled(
                pad_to_width("", width as usize),
                Style::default().fg(theme::TOOL_RESULT_FG),
            ));

            let result_style = if msg.tool_error {
                Style::default().fg(theme::ERROR_FG)
            } else {
                Style::default().fg(theme::TOOL_RESULT_FG)
            };

            // Render result content with syntax highlighting
            let mut highlighted = highlight_code_block("", result);
            // Pad each line to full width
            for line in &mut highlighted {
                let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                let padded = if text.is_empty() {
                    " ".repeat(width as usize)
                } else {
                    pad_to_width(&text, width as usize)
                };
                // 保留 syntect 样式，只替换内容
                line.spans.clear();
                line.spans.push(Span::styled(padded, result_style));
            }
            // 展开态：完整显示结果内容，不截断
            lines.extend(highlighted);
        } else {
            // Collapsed: show result summary
            let summary = result_summary(name, result);
            if !summary.is_empty() {
                let summary_line = format!("  {}", summary);
                let summary_wrapped = wrap_text(&summary_line, width);
                for sl in &summary_wrapped {
                    let content = if sl.is_empty() {
                        " ".repeat(width as usize)
                    } else {
                        pad_to_width(sl, width as usize)
                    };
                    lines.push(Line::styled(content, Style::default().fg(theme::TOOL_RESULT_FG)));
                }
            }
        }
    }

    lines
}

/// Render a ToolResult message block.
pub(crate) fn render_tool_result(msg: &ChatLine, width: u16) -> Vec<Line<'static>> {
    let name = match &msg.line_type {
        LineType::ToolResult { name } => name.as_str(),
        _ => return Vec::new(),
    };

    let max_lines = max_lines_for_tool(name);
    let icon = tool_icon(name);
    let mut lines = Vec::new();
    if max_lines == Some(0) {
        // 不显示内容，只显示摘要
        let summary = result_summary(name, &msg.content);
        let status_line = format!("  ✓ {} {} {}", icon, name, summary);
        lines.push(Line::styled(
            pad_to_width(&status_line, width as usize),
            Style::default().fg(theme::TOOL_RESULT_FG),
        ));
    } else {
        // bash/shell 输出：首行就是内容，不走"灰色状态头"模式
        let is_shell = matches!(name, "bash" | "cmd" | "powershell");

        let content_body = if is_shell {
            // shell：全部内容传语法高亮，不加灰色头
            &msg.content
        } else if let Some((first, rest)) = msg.content.split_once('\n') {
            // 其他工具：第一行作为灰色状态头
            let status = format!("  ✓ {} {} {}", icon, name, first);
            lines.push(Line::styled(
                pad_to_width(&status, width as usize),
                Style::default().fg(theme::TOOL_RESULT_FG),
            ));
            rest
        } else {
            let summary = msg.content.clone();
            let status = format!("  ✓ {} {}", icon, summary);
            lines.push(Line::styled(
                pad_to_width(&status, width as usize),
                Style::default().fg(theme::TOOL_RESULT_FG),
            ));
            return lines;
        };

        let mut highlighted = highlight_code_block("", content_body);
        let total_len = highlighted.len();
        if let Some(max) = max_lines
            && total_len > max
        {
            highlighted.truncate(max);
            let remaining = total_len.saturating_sub(max);
            highlighted.push(Line::styled(
                format!("... [truncated, {} more lines]", remaining),
                Style::default().fg(theme::TOOL_RESULT_FG),
            ));
        }
        lines.extend(highlighted);
    }
    lines
}

/// Render a ToolError message block.
pub(crate) fn render_tool_error(msg: &ChatLine, width: u16) -> Vec<Line<'static>> {
    let name = match &msg.line_type {
        LineType::ToolError { name } => name.as_str(),
        _ => return Vec::new(),
    };

    let icon = tool_icon(name);
    let mut lines = Vec::new();
    let error_line = format!("❌ {} {} {}", icon, name, msg.content);
    lines.push(Line::styled(
        pad_to_width(&error_line, width as usize),
        Style::default().fg(theme::ERROR_FG),
    ));
    lines
}

// ════════════════════════════════════════════════════════════════
// Click hit-test (extracted from event.rs)
// ════════════════════════════════════════════════════════════════

/// Check if a mouse click at the given virtual_row hits a tool call block header.
/// If so, returns the call_id to toggle. Returns None if no tool block was hit.
pub(crate) fn tool_block_hit_test(
    messages: &[ChatLine],
    caches: &[crate::app::MessageCache],
    virtual_row: u16,
) -> Option<String> {
    let mut y: u16 = 0;
    for msg in messages {
        let style = theme::style_for(msg.line_type.clone());
        if let Some(cache) = caches.iter().find(|c| c.msg_id == msg.id) {
            let h = style.total_height(cache.line_count);
            if virtual_row >= y && virtual_row < y + h {
                // Clicked on this message
                if matches!(msg.line_type, LineType::ToolCall { .. }) {
                    if let Some(ref call_id) = msg.call_id {
                        // 整个 tool block（含展开的结果内容）均可点击切换展开/折叠
                        return Some(call_id.clone());
                    }
                }
                break;
            }
            y += h;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── format_tool_args_summary: write_file ──────────────

    #[test]
    fn test_args_summary_write_file() {
        let args = r#"{"path":"src/main.rs","content":"fn main() {}\n"}"#;
        let summary = format_tool_args_summary("write_file", args);
        assert_eq!(summary, "src/main.rs: 1");
    }

    #[test]
    fn test_args_summary_write_file_empty_content() {
        let args = r#"{"path":"empty.txt","content":""}"#;
        let summary = format_tool_args_summary("write_file", args);
        assert_eq!(summary, "empty.txt: 0");
    }

    #[test]
    fn test_args_summary_write_file_missing_content() {
        let args = r#"{"path":"no_content.rs"}"#;
        let summary = format_tool_args_summary("write_file", args);
        assert_eq!(summary, "no_content.rs: 0");
    }

    #[test]
    fn test_args_summary_write_file_invalid_json() {
        let summary = format_tool_args_summary("write_file", "not json");
        assert_eq!(summary, "not json");
    }

    // ── result_summary: write_file ────────────────────────

    #[test]
    fn test_result_summary_write_file() {
        let content = "Written 20 bytes to src/main.rs\nfn main() {}\n";
        let summary = result_summary("write_file", content);
        assert_eq!(summary, "Written 20 bytes (1 lines)");
    }

    #[test]
    fn test_result_summary_write_file_no_content_body() {
        let content = "Written 0 bytes to empty.txt\n";
        let summary = result_summary("write_file", content);
        assert_eq!(summary, "Written 0 bytes (0 lines)");
    }

    #[test]
    fn test_result_summary_write_file_error_result() {
        // 错误结果不是 "Written..." 开头，应返回空
        let content = "Failed to write file: permission denied";
        let summary = result_summary("write_file", content);
        assert_eq!(summary, "");
    }

    // ── format_tool_args_summary: edit_file ───────────────

    #[test]
    fn test_args_summary_edit_file() {
        let args = r#"{"path":"src/lib.rs","old_string":"foo","new_string":"bar\nbaz"}"#;
        let summary = format_tool_args_summary("edit_file", args);
        assert_eq!(summary, "src/lib.rs: 2");
    }

    #[test]
    fn test_args_summary_edit_file_empty_new_string() {
        let args = r#"{"path":"src/lib.rs","old_string":"foo","new_string":""}"#;
        let summary = format_tool_args_summary("edit_file", args);
        assert_eq!(summary, "src/lib.rs: 0");
    }

    #[test]
    fn test_args_summary_edit_file_missing_new_string() {
        let args = r#"{"path":"src/lib.rs","old_string":"foo"}"#;
        let summary = format_tool_args_summary("edit_file", args);
        assert_eq!(summary, "src/lib.rs: 0");
    }

    #[test]
    fn test_args_summary_edit_file_invalid_json() {
        let summary = format_tool_args_summary("edit_file", "bad json");
        assert_eq!(summary, "bad json");
    }

    // ── result_summary: edit_file ─────────────────────────

    #[test]
    fn test_result_summary_edit_file_success() {
        let content = "Replaced 1 occurrence in src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,1 +1,2 @@\n-old\n+new\n+new2\n";
        let summary = result_summary("edit_file", content);
        assert_eq!(summary, "Diff 6 lines");
    }

    #[test]
    fn test_result_summary_edit_file_error() {
        let content = "No matches found for 'foo'";
        let summary = result_summary("edit_file", content);
        assert_eq!(summary, "");
    }
}
