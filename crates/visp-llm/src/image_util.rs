// Utilities for parsing data URIs and persisting LLM-generated images.
// Functions are currently consumed by the provider layer; until then the
// whole module is dead code from the crate's perspective.
#![allow(dead_code)]

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};

use base64::Engine;
use visp_core::error::LlmError;

/// Maximum decoded size of a saved image (20MB).
const MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;

/// Monotonic counter for unique image filenames within a process.
static IMAGE_INDEX: AtomicU32 = AtomicU32::new(0);

/// Parse a data URI string into `(mime_type, base64_data)`.
///
/// Supported format: `data:[<mime>];base64,<data>`
/// - Missing MIME type defaults to `application/octet-stream`.
/// - Non-data-URI strings and non-base64 data URIs return `None`.
pub fn parse_data_uri(uri: &str) -> Option<(String, String)> {
    let rest = uri.strip_prefix("data:")?;
    let (meta, data) = rest.split_once(";base64,")?;
    let mime = if meta.is_empty() {
        "application/octet-stream".to_string()
    } else {
        meta.to_string()
    };
    Some((mime, data.to_string()))
}

/// Map a MIME type to a file extension. Falls back to `"png"`.
pub fn mime_to_extension(mime_type: &str) -> &'static str {
    match mime_type {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        "image/bmp" => "bmp",
        _ => "png",
    }
}

/// Save a base64-encoded image to `{project_path}/.visp/images/`.
///
/// Returns the full path of the written file.
pub fn save_base64_image(
    data: &str,
    mime_type: &str,
    project_path: &str,
) -> Result<String, LlmError> {
    // Rough upper bound check before decoding.
    let estimated_size = data.len() * 3 / 4;
    if estimated_size > MAX_IMAGE_BYTES {
        return Err(LlmError::Stream("image too large (max 20MB)".to_string()));
    }

    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|e| LlmError::Stream(format!("base64 decode error: {e}")))?;

    let dir = Path::new(project_path).join(".visp").join("images");
    fs::create_dir_all(&dir)
        .map_err(|e| LlmError::Stream(format!("create image dir failed: {e}")))?;

    let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let index = IMAGE_INDEX.fetch_add(1, Ordering::Relaxed);
    let filename = format!("{timestamp}_{index}.{}", mime_to_extension(mime_type));
    let path = dir.join(filename);

    fs::write(&path, &bytes)
        .map_err(|e| LlmError::Stream(format!("write image failed: {e}")))?;

    Ok(path.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal 1x1 PNG.
    const TINY_PNG: &str =
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("visp_image_util_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn parse_data_uri_correct() {
        let parsed = parse_data_uri("data:image/png;base64,iVBOR=");
        assert_eq!(parsed, Some(("image/png".to_string(), "iVBOR=".to_string())));
    }

    #[test]
    fn parse_data_uri_invalid() {
        assert_eq!(parse_data_uri("not-a-data-uri"), None);
        assert_eq!(parse_data_uri("data:image/png;charset=utf-8,iVBOR="), None);
    }

    #[test]
    fn parse_data_uri_no_mime() {
        let parsed = parse_data_uri("data:;base64,aGVsbG8=");
        assert_eq!(
            parsed,
            Some(("application/octet-stream".to_string(), "aGVsbG8=".to_string()))
        );
    }

    #[test]
    fn mime_to_extension_png() {
        assert_eq!(mime_to_extension("image/png"), "png");
    }

    #[test]
    fn mime_to_extension_jpeg() {
        assert_eq!(mime_to_extension("image/jpeg"), "jpg");
    }

    #[test]
    fn mime_to_extension_webp() {
        assert_eq!(mime_to_extension("image/webp"), "webp");
    }

    #[test]
    fn mime_to_extension_unknown() {
        assert_eq!(mime_to_extension("image/avif"), "png");
        assert_eq!(mime_to_extension(""), "png");
    }

    #[test]
    fn save_base64_image_success() {
        let dir = temp_dir("success");
        let path = save_base64_image(TINY_PNG, "image/png", dir.to_str().unwrap()).unwrap();
        assert!(Path::new(&path).is_file());
        let bytes = fs::read(&path).unwrap();
        // Decoded PNG starts with the magic bytes.
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_base64_image_too_large() {
        let dir = temp_dir("too_large");
        let big = "A".repeat(28 * 1024 * 1024); // ~21MB estimated decoded size
        let err = save_base64_image(&big, "image/png", dir.to_str().unwrap()).unwrap_err();
        assert!(matches!(err, LlmError::Stream(_)));
        assert!(!dir.join(".visp").exists(), "no files should be written");
    }

    #[test]
    fn save_base64_image_decode_fail() {
        let dir = temp_dir("decode_fail");
        let err = save_base64_image("!!!invalid-base64!!!", "image/png", dir.to_str().unwrap())
            .unwrap_err();
        assert!(matches!(err, LlmError::Stream(_)));
    }
}
