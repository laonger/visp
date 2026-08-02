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

// ════════════════════════════════════════════════════════════════
// @path 输入解析
// ════════════════════════════════════════════════════════════════

/// Supported image file extensions for `@path` reference matching.
const IMAGE_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "bmp", "ico",
];

/// Check if a path has a supported image file extension.
fn is_image_file(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| IMAGE_EXTENSIONS.contains(&ext.to_lowercase().as_str()))
        .unwrap_or(false)
}

/// Check if a string starts with a URL scheme (http:// or https://).
fn is_url(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
}

/// Parse `@path` or `@url` references in user input text and replace them with
/// `<image: path-or-url>` markers.
///
/// Matching rules:
/// - `@` must be at the start of a word (preceded by whitespace or start of string)
/// - `@http://...` or `@https://...` -> URL image, directly matched
/// - `@<other text>` -> resolve as file path relative to `project_path`; if file exists
///   and has a supported image extension, replace with `<image: abs_path>`; otherwise
///   leave `@` and the text as-is
///
/// Non-matching `@` sequences (e.g. `@mention`, `@nonexistent.txt`) are left as-is.
pub fn parse_image_refs(text: &str, project_path: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut result = String::with_capacity(text.len());
    let mut i = 0;
    let project = std::path::Path::new(project_path);

    while i < chars.len() {
        // Check for `@` at word boundary (start of string or preceded by whitespace)
        if chars[i] == '@'
            && (i == 0 || chars[i - 1].is_whitespace())
        {
            // Collect the word after `@` (non-whitespace sequence)
            let word_start = i + 1;
            let mut word_end = word_start;
            while word_end < chars.len() && !chars[word_end].is_whitespace() {
                word_end += 1;
            }
            let word: String = chars[word_start..word_end].iter().collect();

            if word.is_empty() {
                // `@` followed by whitespace, keep as-is
                result.push('@');
                i += 1;
                continue;
            }

            if is_url(&word) {
                // URL image: directly replace
                result.push_str(&format!("<image: {}>", word));
                i = word_end;
            } else {
                // Try to resolve as local file path
                let path = std::path::Path::new(&word);
                let resolved = if path.is_absolute() {
                    path.to_path_buf()
                } else {
                    project.join(&word)
                };

                if resolved.exists() && is_image_file(&resolved) {
                    // Match: replace with absolute path marker
                    let abs = resolved.canonicalize().unwrap_or(resolved);
                    result.push_str(&format!("<image: {}>", abs.display()));
                } else {
                    // No match: keep `@` and the word as-is
                    result.push('@');
                    result.push_str(&word);
                }
                i = word_end;
            }
        } else {
            result.push(chars[i]);
            i += 1;
        }
    }

    result
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

    // ── parse_image_refs tests ──────────────────────────────

    use std::io::Write;

    /// Create a temp image file for testing and return its absolute path.
    fn make_temp_image(ext: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir();
        let name = format!("visp_test_img_{}.{}", std::process::id(), ext);
        let path = dir.join(name);
        // Write minimal PNG header (or empty file for other exts - we only check existence + ext)
        let mut f = std::fs::File::create(&path).unwrap();
        if ext == "png" {
            f.write_all(&[
                0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A,
            ]).unwrap();
        }
        f.flush().unwrap();
        path
    }

    #[test]
    fn parse_absolute_path() {
        let img = make_temp_image("png");
        let input = format!("look at @{}", img.display());
        let result = parse_image_refs(&input, "/tmp");
        let canonical = img.canonicalize().unwrap();
        assert!(result.contains(&format!("<image: {}>", canonical.display())));
        assert!(!result.contains('@'));
        let _ = std::fs::remove_file(&img);
    }

    #[test]
    fn parse_relative_path_with_dot_slash() {
        let dir = std::env::temp_dir();
        let img_name = format!("visp_test_rel_{}.png", std::process::id());
        let img_path = dir.join(&img_name);
        std::fs::write(&img_path, b"fake").unwrap();

        let input = format!("see @./{}", img_name);
        let result = parse_image_refs(&input, dir.to_str().unwrap());

        let canonical = img_path.canonicalize().unwrap();
        assert!(result.contains(&format!("<image: {}>", canonical.display())));
        let _ = std::fs::remove_file(&img_path);
    }

    #[test]
    fn parse_relative_path_bare() {
        let dir = std::env::temp_dir();
        let img_name = format!("visp_test_bare_{}.png", std::process::id());
        let img_path = dir.join(&img_name);
        std::fs::write(&img_path, b"fake").unwrap();

        let input = format!("see @{}", img_name);
        let result = parse_image_refs(&input, dir.to_str().unwrap());

        let canonical = img_path.canonicalize().unwrap();
        assert!(result.contains(&format!("<image: {}>", canonical.display())));
        let _ = std::fs::remove_file(&img_path);
    }

    #[test]
    fn parse_nonexistent_file_kept_as_is() {
        let result = parse_image_refs("check @nonexistent.png", "/tmp");
        assert_eq!(result, "check @nonexistent.png");
    }

    #[test]
    fn parse_mention_kept_as_is() {
        let result = parse_image_refs("hello @mention", "/tmp");
        assert_eq!(result, "hello @mention");
    }

    #[test]
    fn parse_url() {
        let result = parse_image_refs(
            "see @https://example.com/img.png",
            "/tmp",
        );
        assert_eq!(result, "see <image: https://example.com/img.png>");
    }

    #[test]
    fn parse_email_kept_as_is() {
        let result = parse_image_refs("contact user@email.com", "/tmp");
        assert_eq!(result, "contact user@email.com");
    }

    #[test]
    fn parse_multiple_refs() {
        let dir = std::env::temp_dir();
        let img1_name = format!("visp_test_multi1_{}.png", std::process::id());
        let img2_name = format!("visp_test_multi2_{}.jpg", std::process::id());
        let img1_path = dir.join(&img1_name);
        let img2_path = dir.join(&img2_name);
        std::fs::write(&img1_path, b"fake").unwrap();
        std::fs::write(&img2_path, b"fake").unwrap();

        let input = format!(
            "first @{} second @https://x.com/y.png third @{}",
            img1_name, img2_name
        );
        let result = parse_image_refs(&input, dir.to_str().unwrap());

        let c1 = img1_path.canonicalize().unwrap();
        let c2 = img2_path.canonicalize().unwrap();
        assert!(result.contains(&format!("<image: {}>", c1.display())));
        assert!(result.contains("<image: https://x.com/y.png>"));
        assert!(result.contains(&format!("<image: {}>", c2.display())));
        assert!(!result.contains('@'));

        let _ = std::fs::remove_file(&img1_path);
        let _ = std::fs::remove_file(&img2_path);
    }

    #[test]
    fn parse_at_followed_by_space() {
        let result = parse_image_refs("hello @ world", "/tmp");
        assert_eq!(result, "hello @ world");
    }
}
