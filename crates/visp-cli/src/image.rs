#![allow(dead_code)]

use crate::app::{ChatLine, LineType};

/// Marker prefix for inline image references in text content.
const IMAGE_MARKER: &str = "<image: ";

/// Parse image markers from text content and split into multiple ChatLines.
///
/// Markers format: `<image: /path/or/url>`
/// Empty text segments (before first marker, between markers, after last marker) are skipped.
///
/// Returns a Vec of ChatLine with id=0 (placeholder). Callers must assign unique ids.
pub fn split_image_markers(content: &str, base_line_type: LineType) -> Vec<ChatLine> {
    let mut lines: Vec<ChatLine> = Vec::new();
    let mut search_from = 0usize;

    while let Some(rel_start) = content[search_from..].find(IMAGE_MARKER) {
        let marker_start = search_from + rel_start;
        let path_start = marker_start + IMAGE_MARKER.len();

        // Find the closing '>'; if absent, treat the rest as plain text.
        let Some(rel_end) = content[path_start..].find('>') else {
            break;
        };
        let marker_end = path_start + rel_end;

        // Text segment before the marker (skip if empty).
        let text_before = &content[search_from..marker_start];
        if !text_before.is_empty() {
            lines.push(make_text_line(text_before.to_string(), &base_line_type));
        }

        // Image marker.
        let path = content[path_start..marker_end].trim();
        if !path.is_empty() {
            lines.push(make_image_line(path.to_string()));
        }

        search_from = marker_end + 1;
    }

    // Trailing text segment after the last marker (skip if empty).
    let trailing = &content[search_from..];
    if !trailing.is_empty() {
        lines.push(make_text_line(trailing.to_string(), &base_line_type));
    }

    lines
}

/// Extract alt_text (filename) from a path or URL.
fn extract_alt_text(path: &str) -> String {
    // For URLs: strip query string first, then take last path segment
    // For local paths: take last path segment
    // Examples:
    //   /abs/path/to/image.png -> "image.png"
    //   https://example.com/cat.png -> "cat.png"
    //   https://example.com/cat.png?w=100 -> "cat.png"
    //   /abs/path/noext -> "noext"
    let without_query = match path.find('?') {
        Some(idx) => &path[..idx],
        None => path,
    };
    without_query
        .rsplit(|c| c == '/' || c == '\\')
        .next()
        .unwrap_or("")
        .to_string()
}

/// Build a text ChatLine with placeholder id/version and default metadata.
fn make_text_line(content: String, line_type: &LineType) -> ChatLine {
    ChatLine {
        id: 0,
        version: 0,
        line_type: line_type.clone(),
        content,
        call_id: None,
        tool_result: None,
        tool_error: false,
        sub_session_id: None,
    }
}

/// Build an Image ChatLine: content stores the path (for debugging),
/// line_type carries the path and derived alt_text.
fn make_image_line(path: String) -> ChatLine {
    let alt_text = extract_alt_text(&path);
    ChatLine {
        id: 0,
        version: 0,
        line_type: LineType::Image {
            path: path.clone(),
            alt_text,
        },
        content: path,
        call_id: None,
        tool_result: None,
        tool_error: false,
        sub_session_id: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::LineType;

    fn assert_text_line(line: &ChatLine, expected_content: &str) {
        assert_eq!(line.line_type, LineType::User);
        assert_eq!(line.content, expected_content);
        assert_eq!(line.id, 0);
        assert_eq!(line.version, 0);
        assert!(line.call_id.is_none());
        assert!(line.tool_result.is_none());
        assert!(!line.tool_error);
        assert!(line.sub_session_id.is_none());
    }

    fn assert_image_line(line: &ChatLine, expected_path: &str, expected_alt_text: &str) {
        assert_eq!(
            line.line_type,
            LineType::Image {
                path: expected_path.to_string(),
                alt_text: expected_alt_text.to_string(),
            }
        );
        assert_eq!(line.content, expected_path);
        assert_eq!(line.id, 0);
        assert_eq!(line.version, 0);
        assert!(line.call_id.is_none());
        assert!(line.tool_result.is_none());
        assert!(!line.tool_error);
        assert!(line.sub_session_id.is_none());
    }

    #[test]
    fn no_markers_in_plain_text() {
        let lines = split_image_markers("hello world", LineType::User);
        assert_eq!(lines.len(), 1);
        assert_text_line(&lines[0], "hello world");
    }

    #[test]
    fn single_marker_in_middle_of_text() {
        let lines = split_image_markers("look at <image: /tmp/a.png> here", LineType::User);
        assert_eq!(lines.len(), 3);
        assert_text_line(&lines[0], "look at ");
        assert_image_line(&lines[1], "/tmp/a.png", "a.png");
        assert_text_line(&lines[2], " here");
    }

    #[test]
    fn marker_at_text_start() {
        let lines = split_image_markers("<image: /tmp/a.png> rest", LineType::User);
        assert_eq!(lines.len(), 2);
        assert_image_line(&lines[0], "/tmp/a.png", "a.png");
        assert_text_line(&lines[1], " rest");
    }

    #[test]
    fn marker_at_text_end() {
        let lines = split_image_markers("before <image: /tmp/a.png>", LineType::User);
        assert_eq!(lines.len(), 2);
        assert_text_line(&lines[0], "before ");
        assert_image_line(&lines[1], "/tmp/a.png", "a.png");
    }

    #[test]
    fn multiple_markers() {
        let lines = split_image_markers(
            "a<image: /x.png>b<image: /y.png>c",
            LineType::User,
        );
        assert_eq!(lines.len(), 5);
        assert_text_line(&lines[0], "a");
        assert_image_line(&lines[1], "/x.png", "x.png");
        assert_text_line(&lines[2], "b");
        assert_image_line(&lines[3], "/y.png", "y.png");
        assert_text_line(&lines[4], "c");
    }

    #[test]
    fn only_one_marker_no_other_text() {
        let lines = split_image_markers("<image: /tmp/a.png>", LineType::User);
        assert_eq!(lines.len(), 1);
        assert_image_line(&lines[0], "/tmp/a.png", "a.png");
    }

    #[test]
    fn url_marker() {
        let lines = split_image_markers("<image: https://example.com/cat.png>", LineType::User);
        assert_eq!(lines.len(), 1);
        assert_image_line(&lines[0], "https://example.com/cat.png", "cat.png");
    }

    #[test]
    fn alt_text_extraction() {
        assert_eq!(extract_alt_text("/abs/path/to/image.png"), "image.png");
        assert_eq!(extract_alt_text("https://example.com/cat.png"), "cat.png");
        assert_eq!(
            extract_alt_text("https://example.com/cat.png?w=100"),
            "cat.png"
        );
        assert_eq!(extract_alt_text("/abs/path/noext"), "noext");
    }
}
