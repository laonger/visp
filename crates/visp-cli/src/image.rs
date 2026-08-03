#![allow(dead_code)]

use std::collections::HashMap;
use std::path::Path;

use ratatui_image::picker::Picker;
use ratatui_image::protocol::Protocol;
use ratatui_image::Resize;
use tokio::sync::mpsc;

use crate::app::{ChatLine, LineType};

/// Marker prefix for inline image references in text content.
const IMAGE_MARKER: &str = "<image: ";

/// Parse image markers from text content and split into multiple ChatLines.
///
/// Markers format: `<image: /path/or/url>`, optionally with a remote URL:
///   - `<image: /local/path.png>`
///   - `<image: | https://example.com/img.png>` (remote URL only)
///   - `<image: /local/path.png | https://example.com/img.png>` (both)
///
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
        // Marker content may be `path`, `| url`, or `path | url`.
        let raw = content[path_start..marker_end].trim();
        let (path, remote_url) = if let Some(idx) = raw.find('|') {
            let p = raw[..idx].trim().to_string();
            let u = raw[idx + 1..].trim().to_string();
            (p, if u.is_empty() { None } else { Some(u) })
        } else {
            (raw.to_string(), None)
        };
        if !path.is_empty() || remote_url.is_some() {
            lines.push(make_image_line(path, remote_url));
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

/// Build an Image ChatLine: content stores the path (or URL when path is empty),
/// line_type carries the path, derived alt_text and optional remote_url.
fn make_image_line(path: String, remote_url: Option<String>) -> ChatLine {
    let alt_text = if path.is_empty() {
        if let Some(ref url) = remote_url {
            extract_alt_text(url)
        } else {
            String::new()
        }
    } else {
        extract_alt_text(&path)
    };
    let content = if path.is_empty() {
        remote_url.clone().unwrap_or_default()
    } else {
        path.clone()
    };
    ChatLine {
        id: 0,
        version: 0,
        line_type: LineType::Image {
            path,
            alt_text,
            remote_url,
        },
        content,
        call_id: None,
        tool_result: None,
        tool_error: false,
        sub_session_id: None,
    }
}

// ════════════════════════════════════════════════════════════════
// ImageCache：图片缓存与加载
// ════════════════════════════════════════════════════════════════

/// An entry in the image cache, representing the loading state of an image.
pub enum ImageEntry {
    /// Image is ready.
    Ready {
        /// Original decoded image, kept for re-encoding when terminal size changes.
        image: image::DynamicImage,
        /// Cached protocol encoded at `rendered_size`. Recreated when size changes.
        protocol: Option<std::sync::Arc<std::sync::Mutex<Protocol>>>,
        /// The terminal size (cols, rows) at which `protocol` was encoded.
        rendered_size: (u16, u16),
        /// Original pixel dimensions.
        pixel_size: (u32, u32),
        /// Local cache file path (downloaded URL images) or source path (local images).
        local_path: Option<String>,
    },
    /// Network image is being downloaded.
    Loading,
    /// Loading, downloading, or decoding failed.
    Error(String),
}

/// Snapshot of an image entry's state for cache invalidation purposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageState {
    Ready,
    Loading,
    Error,
}

impl ImageEntry {
    pub fn state(&self) -> ImageState {
        match self {
            ImageEntry::Ready { .. } => ImageState::Ready,
            ImageEntry::Loading => ImageState::Loading,
            ImageEntry::Error(_) => ImageState::Error,
        }
    }
}

/// Result of querying image height for layout calculation.
#[derive(Debug, Clone, Copy)]
pub enum ImageHeightInfo {
    /// Actual computed height in terminal rows.
    Ready(u16),
    /// Loading or Error: use 1-row placeholder height.
    Placeholder,
}

/// Image metrics for `MessageCache` height calculation.
pub struct ImageMetrics<'a> {
    pub font_size: (u16, u16),
    pub image_cache: &'a ImageCache,
    /// Maximum image height in terminal rows (proportional to terminal height).
    pub max_rows: u16,
}

/// Cache for decoded images, keyed by file path or URL.
pub struct ImageCache {
    picker: Picker,
    cache: std::sync::Arc<std::sync::Mutex<HashMap<String, ImageEntry>>>,
    image_ready_tx: Option<mpsc::UnboundedSender<()>>,
}

impl ImageCache {
    /// Create a new ImageCache. Tries to detect terminal capabilities, falls back to Halfblocks.
    pub fn new() -> Self {
        let picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
        Self {
            picker,
            cache: std::sync::Arc::new(std::sync::Mutex::new(HashMap::new())),
            image_ready_tx: None,
        }
    }

    /// Set the image_ready notification channel. Called during AppState initialization.
    pub fn set_ready_tx(&mut self, tx: mpsc::UnboundedSender<()>) {
        self.image_ready_tx = Some(tx);
    }

    /// Get the font size (character cell size in pixels) from the picker.
    pub fn font_size(&self) -> (u16, u16) {
        let fs = self.picker.font_size();
        (fs.width, fs.height)
    }

    /// Get a reference to the picker (for rendering).
    pub fn picker(&self) -> &Picker {
        &self.picker
    }

    /// Load or retrieve a cached image entry by path/URL.
    ///
    /// For local files: synchronously reads and decodes.
    /// For URLs: returns Loading on first call, spawns async download that updates the cache.
    pub fn get_or_load(&self, path: &str) {
        let mut cache = self.cache.lock().unwrap();
        if cache.contains_key(path) {
            return;
        }
        if is_url(path) {
            cache.insert(path.to_string(), ImageEntry::Loading);
            drop(cache);

            let url = path.to_string();
            let tx = self.image_ready_tx.clone();
            let cache = self.cache.clone();
            let picker = self.picker.clone();
            tokio::spawn(async move {
                let result = download_and_decode(&url, &picker).await;
                {
                    let mut cache = cache.lock().unwrap();
                    cache.insert(url, result);
                }
                if let Some(tx) = tx {
                    let _ = tx.send(());
                }
            });
        } else {
            let entry = load_local_image(path, &self.picker);
            cache.insert(path.to_string(), entry);
        }
    }

    /// Try to get a ready image entry's protocol for rendering.
    /// If the cached protocol was encoded at a different size, it is re-created.
    /// Returns None if not Ready.
    pub fn try_get_protocol(
        &self,
        path: &str,
        target_size: (u16, u16),
    ) -> Option<std::sync::Arc<std::sync::Mutex<Protocol>>> {
        let mut cache = self.cache.lock().unwrap();
        let entry = cache.get_mut(path)?;
        match entry {
            ImageEntry::Ready {
                image,
                protocol,
                rendered_size,
                ..
            } => {
                // Re-create protocol if size changed or not yet encoded
                if *rendered_size != target_size || protocol.is_none() {
                    let size = ratatui::layout::Size::new(target_size.0, target_size.1);
                    match self.picker.new_protocol(image.clone(), size, Resize::Fit(None)) {
                        Ok(proto) => {
                            *protocol = Some(std::sync::Arc::new(std::sync::Mutex::new(proto)));
                            *rendered_size = target_size;
                        }
                        Err(_) => {
                            *protocol = None;
                        }
                    }
                }
                protocol.clone()
            }
            _ => None,
        }
    }

    /// Query the height (in terminal rows) of an image at the given path.
    /// Returns Placeholder for Loading/Error/not-found states.
    pub fn query_height(&self, path: &str, max_cols: u16, max_rows: u16) -> ImageHeightInfo {
        let cache = self.cache.lock().unwrap();
        match cache.get(path) {
            Some(ImageEntry::Ready { pixel_size, .. }) => {
                let font_size = self.font_size();
                ImageHeightInfo::Ready(calc_image_height(
                    pixel_size.0,
                    pixel_size.1,
                    max_cols,
                    font_size,
                    max_rows,
                ))
            }
            _ => ImageHeightInfo::Placeholder,
        }
    }

    /// Check the current state of a cached image (for cache invalidation).
    pub fn image_state(&self, path: &str) -> Option<ImageState> {
        let cache = self.cache.lock().unwrap();
        cache.get(path).map(|e| e.state())
    }

    /// Get error message for an Error-state image (for rendering error text).
    pub fn error_message(&self, path: &str) -> Option<String> {
        let cache = self.cache.lock().unwrap();
        match cache.get(path)? {
            ImageEntry::Error(msg) => Some(msg.clone()),
            _ => None,
        }
    }

    /// Check if a URL image is currently loading.
    pub fn is_loading(&self, path: &str) -> bool {
        let cache = self.cache.lock().unwrap();
        matches!(cache.get(path), Some(ImageEntry::Loading))
    }
}

/// Calculate the height (in terminal rows) for an image given its pixel dimensions,
/// available terminal columns, and font cell size.
///
/// The image is scaled to fit within `max_cols` columns (no upscale if narrower),
/// and the resulting height is clamped to `max_rows`.
pub fn calc_image_height(
    pixel_w: u32,
    pixel_h: u32,
    max_cols: u16,
    font_size: (u16, u16),
    max_rows: u16,
) -> u16 {
    if pixel_w == 0 || pixel_h == 0 {
        return 1;
    }
    let (font_w, font_h) = (font_size.0 as u32, font_size.1 as u32);
    if font_w == 0 || font_h == 0 {
        return 1;
    }

    // How many columns does the image need at original size?
    let natural_cols = pixel_w.div_ceil(font_w);

    // If the image is narrower than max_cols, don't upscale.
    let effective_cols = natural_cols.min(max_cols as u32);

    // Scale height proportionally.
    let scaled_h_px = pixel_h * effective_cols * font_w / pixel_w;

    // Convert to rows.
    let rows = (scaled_h_px.div_ceil(font_h) as u16).max(1);

    // Clamp to max_rows (proportional height limit based on terminal size)
    rows.min(max_rows).max(1)
}

/// Load a local image file synchronously.
fn load_local_image(path: &str, _picker: &Picker) -> ImageEntry {
    let path_obj = Path::new(path);
    if !path_obj.exists() {
        return ImageEntry::Error(format!("File not found: {}", path));
    }
    match image::ImageReader::open(path_obj) {
        Ok(reader) => match reader.decode() {
            Ok(img) => {
                let pixel_size = (img.width(), img.height());
                ImageEntry::Ready {
                    image: img,
                    protocol: None,
                    rendered_size: (0, 0),
                    pixel_size,
                    local_path: Some(path.to_string()),
                }
            }
            Err(e) => ImageEntry::Error(format!("Decode failed: {}", e)),
        },
        Err(e) => ImageEntry::Error(format!("Open failed: {}", e)),
    }
}

/// Download a URL image asynchronously and decode it.
async fn download_and_decode(url: &str, _picker: &Picker) -> ImageEntry {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build();
    let client = match client {
        Ok(c) => c,
        Err(e) => return ImageEntry::Error(format!("HTTP client error: {}", e)),
    };

    match client.get(url).send().await {
        Ok(resp) => {
            if !resp.status().is_success() {
                return ImageEntry::Error(format!("HTTP {}", resp.status()));
            }
            // Determine extension from content-type
            let ext = resp
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .map(|ct| {
                    if ct.contains("png") { "png" }
                    else if ct.contains("jpeg") || ct.contains("jpg") { "jpg" }
                    else if ct.contains("webp") { "webp" }
                    else if ct.contains("gif") { "gif" }
                    else { "png" }
                })
                .unwrap_or("png");
            match resp.bytes().await {
                Ok(bytes) => match image::load_from_memory(&bytes) {
                    Ok(img) => {
                        let pixel_size = (img.width(), img.height());
                        // Write cache file
                        let local_path = write_url_cache(url, &bytes, ext);
                        ImageEntry::Ready {
                            image: img,
                            protocol: None,
                            rendered_size: (0, 0),
                            pixel_size,
                            local_path,
                        }
                    }
                    Err(e) => ImageEntry::Error(format!("Decode failed: {}", e)),
                },
                Err(e) => ImageEntry::Error(format!("Download failed: {}", e)),
            }
        }
        Err(e) => {
            if e.is_timeout() {
                ImageEntry::Error("Download timeout".to_string())
            } else {
                ImageEntry::Error(format!("Download failed: {}", e))
            }
        }
    }
}

/// Write downloaded image bytes to a cache file keyed by URL hash.
/// Returns the cache file path on success, None on failure.
fn write_url_cache(url: &str, bytes: &[u8], ext: &str) -> Option<String> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    url.hash(&mut hasher);
    let hash = hasher.finish();
    let cache_dir = std::env::temp_dir().join(".visp").join("images");
    let _ = std::fs::create_dir_all(&cache_dir);
    let cache_path = cache_dir.join(format!("{}.{}", hash, ext));
    match std::fs::write(&cache_path, bytes) {
        Ok(()) => Some(cache_path.to_string_lossy().into_owned()),
        Err(_) => None,
    }
}



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

    fn assert_image_line(
        line: &ChatLine,
        expected_path: &str,
        expected_alt_text: &str,
        expected_remote_url: Option<&str>,
    ) {
        assert_eq!(
            line.line_type,
            LineType::Image {
                path: expected_path.to_string(),
                alt_text: expected_alt_text.to_string(),
                remote_url: expected_remote_url.map(|s| s.to_string()),
            }
        );
        let expected_content = if expected_path.is_empty() {
            expected_remote_url.unwrap_or("").to_string()
        } else {
            expected_path.to_string()
        };
        assert_eq!(line.content, expected_content);
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
        assert_image_line(&lines[1], "/tmp/a.png", "a.png", None);
        assert_text_line(&lines[2], " here");
    }

    #[test]
    fn marker_at_text_start() {
        let lines = split_image_markers("<image: /tmp/a.png> rest", LineType::User);
        assert_eq!(lines.len(), 2);
        assert_image_line(&lines[0], "/tmp/a.png", "a.png", None);
        assert_text_line(&lines[1], " rest");
    }

    #[test]
    fn marker_at_text_end() {
        let lines = split_image_markers("before <image: /tmp/a.png>", LineType::User);
        assert_eq!(lines.len(), 2);
        assert_text_line(&lines[0], "before ");
        assert_image_line(&lines[1], "/tmp/a.png", "a.png", None);
    }

    #[test]
    fn multiple_markers() {
        let lines = split_image_markers(
            "a<image: /x.png>b<image: /y.png>c",
            LineType::User,
        );
        assert_eq!(lines.len(), 5);
        assert_text_line(&lines[0], "a");
        assert_image_line(&lines[1], "/x.png", "x.png", None);
        assert_text_line(&lines[2], "b");
        assert_image_line(&lines[3], "/y.png", "y.png", None);
        assert_text_line(&lines[4], "c");
    }

    #[test]
    fn only_one_marker_no_other_text() {
        let lines = split_image_markers("<image: /tmp/a.png>", LineType::User);
        assert_eq!(lines.len(), 1);
        assert_image_line(&lines[0], "/tmp/a.png", "a.png", None);
    }

    #[test]
    fn url_marker() {
        let lines = split_image_markers("<image: https://example.com/cat.png>", LineType::User);
        assert_eq!(lines.len(), 1);
        assert_image_line(&lines[0], "https://example.com/cat.png", "cat.png", None);
    }

    #[test]
    fn url_only_marker() {
        let lines = split_image_markers(
            "<image: | https://example.com/img.png>",
            LineType::User,
        );
        assert_eq!(lines.len(), 1);
        assert_image_line(&lines[0], "", "img.png", Some("https://example.com/img.png"));
    }

    #[test]
    fn url_marker_backward_compatible() {
        let lines = split_image_markers("<image: /local/path.png>", LineType::User);
        assert_eq!(lines.len(), 1);
        assert_image_line(&lines[0], "/local/path.png", "path.png", None);
    }

    #[test]
    fn mixed_path_and_url_markers() {
        let lines = split_image_markers(
            "Hello <image: /local/img.png> World <image: | https://url.png>",
            LineType::User,
        );
        assert_eq!(lines.len(), 4);
        assert_text_line(&lines[0], "Hello ");
        assert_image_line(&lines[1], "/local/img.png", "img.png", None);
        assert_text_line(&lines[2], " World ");
        assert_image_line(&lines[3], "", "url.png", Some("https://url.png"));
    }

    #[test]
    fn path_and_url_marker() {
        let lines = split_image_markers(
            "<image: /local/path.png | https://url.png>",
            LineType::User,
        );
        assert_eq!(lines.len(), 1);
        assert_image_line(&lines[0], "/local/path.png", "path.png", Some("https://url.png"));
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

    // ── ImageCache tests ────────────────────────────────────

    /// Create a minimal valid 1x1 PNG image file for testing using the image crate.
    fn make_test_png(path: &std::path::Path) {
        let img = image::RgbaImage::from_raw(1, 1, vec![255, 0, 0, 255]).unwrap();
        img.save(path).unwrap();
    }

    #[test]
    fn image_cache_new_does_not_panic() {
        // This may run in a non-TTY environment; should fall back to Halfblocks.
        let _cache = ImageCache::new();
    }

    #[test]
    fn image_cache_local_file_load_ready() {
        let cache = ImageCache::new();
        let img_path = std::env::temp_dir().join(format!(
            "visp_test_cache_{}.png",
            std::process::id()
        ));
        make_test_png(&img_path);

        cache.get_or_load(img_path.to_str().unwrap());

        let state = cache.image_state(img_path.to_str().unwrap());
        assert_eq!(state, Some(ImageState::Ready));

        let _ = std::fs::remove_file(&img_path);
    }

    #[test]
    fn image_cache_file_not_found_error() {
        let cache = ImageCache::new();
        let path = "/nonexistent/path/to/image.png";

        cache.get_or_load(path);

        let state = cache.image_state(path);
        assert_eq!(state, Some(ImageState::Error));
        assert!(cache.error_message(path).unwrap().contains("File not found"));
    }

    #[test]
    fn image_cache_query_height_ready() {
        let cache = ImageCache::new();
        let img_path = std::env::temp_dir().join(format!(
            "visp_test_height_{}.png",
            std::process::id()
        ));
        make_test_png(&img_path);

        cache.get_or_load(img_path.to_str().unwrap());
        let height_info = cache.query_height(img_path.to_str().unwrap(), 80, 100);
        match height_info {
            ImageHeightInfo::Ready(h) => assert!(h >= 1),
            ImageHeightInfo::Placeholder => panic!("Expected Ready, got Placeholder"),
        }

        let _ = std::fs::remove_file(&img_path);
    }

    #[test]
    fn image_cache_query_height_error_placeholder() {
        let cache = ImageCache::new();
        let path = "/nonexistent/image.png";

        cache.get_or_load(path);
        let height_info = cache.query_height(path, 80, 100);
        assert!(matches!(height_info, ImageHeightInfo::Placeholder));
    }

    #[test]
    fn calc_image_height_wide_image_scales() {
        // 200x100 image, font_size (10, 20), max_cols 40
        // natural_cols = 200/10 = 20 < 40, so effective_cols = 20
        // scaled_h_px = 100 * 20 * 10 / 200 = 100
        // rows = ceil(100 / 20) = 5
        let h = calc_image_height(200, 100, 40, (10, 20), 100);
        assert_eq!(h, 5);
    }

    #[test]
    fn calc_image_height_narrow_image_no_upscale() {
        // 20x40 image, font_size (10, 20), max_cols 80
        // natural_cols = 20/10 = 2 < 80, so effective_cols = 2
        // scaled_h_px = 40 * 2 * 10 / 20 = 40
        // rows = ceil(40 / 20) = 2
        let h = calc_image_height(20, 40, 80, (10, 20), 100);
        assert_eq!(h, 2);
    }

    #[test]
    fn calc_image_height_wider_than_terminal() {
        // 800x100 image, font_size (10, 20), max_cols 40
        // natural_cols = 800/10 = 80 > 40, so effective_cols = 40
        // scaled_h_px = 100 * 40 * 10 / 800 = 50
        // rows = ceil(50 / 20) = 3
        let h = calc_image_height(800, 100, 40, (10, 20), 100);
        assert_eq!(h, 3);
    }
}
