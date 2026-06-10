use crate::path::validate_path;
use crate::truncate::{DEFAULT_MAX_OUTPUT_BYTES, truncate_output};
use async_trait::async_trait;
use std::fs;
use std::path::{Path, PathBuf};
use visp_core::tool::{Tool, ToolContext, ToolResult};

const MAX_FILE_SIZE: u64 = 1_048_576; // 1MB
const BINARY_SCAN_BYTES: usize = 8000;

/// 读取文件
pub struct ReadFile;

#[async_trait]
impl Tool for ReadFile {
    fn name(&self) -> &str {
        "read_file"
    }

    fn category(&self) -> &str {
        "common"
    }

    fn description(&self) -> &str {
        "Read the contents of a file from the local filesystem. \
         Use this to view source code, configuration files, logs, or any text file. \
         File size is limited to 1MB (detected early to avoid large reads). \
         Binary files are detected and rejected automatically. \
         Paths are validated to prevent directory traversal attacks. \
         Symlinks are not followed for security."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to read, relative to the project root."
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, arguments: serde_json::Value, context: &ToolContext) -> ToolResult {
        let path_str = match arguments.get("path").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => return ToolResult::error("Missing required argument: path"),
        };

        let path = match validate_path(Path::new(path_str), &context.working_dir) {
            Ok(p) => p,
            Err(e) => return ToolResult::error(e),
        };

        // 检查文件大小
        let metadata = match fs::metadata(&path) {
            Ok(m) => m,
            Err(e) => return ToolResult::error(format!("Failed to read file metadata: {}", e)),
        };

        if metadata.len() > MAX_FILE_SIZE {
            return ToolResult::error(format!(
                "File too large: {} bytes (max {} bytes)",
                metadata.len(),
                MAX_FILE_SIZE
            ));
        }

        // 读取内容
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(e) => return ToolResult::error(format!("Failed to read file: {}", e)),
        };

        // 二进制检测（前 8000 字节中 null 占比 > 10%）
        let scan_end = BINARY_SCAN_BYTES.min(bytes.len());
        if scan_end > 0 {
            let null_count = bytes[..scan_end].iter().filter(|&&b| b == 0).count();
            if null_count > scan_end / 10 {
                return ToolResult::error(format!(
                    "File appears to be binary ({} null bytes in first {} bytes)",
                    null_count, scan_end
                ));
            }
        }

        let content = match String::from_utf8(bytes) {
            Ok(s) => s,
            Err(_) => return ToolResult::error("File is not valid UTF-8"),
        };

        ToolResult::success(truncate_output(&content, DEFAULT_MAX_OUTPUT_BYTES))
    }
}

// ---------------------------------------------------------------------------

/// 写入文件（覆盖）
pub struct WriteFile;

#[async_trait]
impl Tool for WriteFile {
    fn name(&self) -> &str {
        "write_file"
    }

    fn category(&self) -> &str {
        "common"
    }

    fn requires_approval(&self) -> bool {
        true
    }

    fn description(&self) -> &str {
        "Write content to a file, creating or overwriting it. \
         Use this to create new files, update existing ones, or generate code/output. \
         Automatically creates parent directories if they don't exist. \
         Paths are validated to prevent directory traversal. \
         File size is limited to 1MB."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Target file path, relative to the project root."
                },
                "content": {
                    "type": "string",
                    "description": "Content to write to the file."
                }
            },
            "required": ["path", "content"]
        })
    }

    async fn execute(&self, arguments: serde_json::Value, context: &ToolContext) -> ToolResult {
        let path_str = match arguments.get("path").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => return ToolResult::error("Missing required argument: path"),
        };
        let content = match arguments.get("content").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s,
            Some(_) => return ToolResult::error("Content is empty"),
            None => return ToolResult::error("Missing required argument: content"),
        };

        // 文件不存在时用父目录验证
        let path = match validate_write_path(Path::new(path_str), &context.working_dir) {
            Ok(p) => p,
            Err(e) => return ToolResult::error(e),
        };

        // 父目录不存在则自动创建
        if let Some(parent) = path.parent()
            && !parent.exists()
            && let Err(e) = fs::create_dir_all(parent)
        {
            return ToolResult::error(format!("Failed to create parent directory: {}", e));
        }

        match fs::write(&path, content) {
            Ok(_) => ToolResult::success(format!(
                "Written {} bytes to {}",
                content.len(),
                path.display()
            )),
            Err(e) => ToolResult::error(format!("Failed to write file: {}", e)),
        }
    }
}

// ---------------------------------------------------------------------------

/// 精确字符串替换编辑
pub struct EditFile;

#[async_trait]
impl Tool for EditFile {
    fn name(&self) -> &str {
        "edit_file"
    }

    fn category(&self) -> &str {
        "common"
    }

    fn requires_approval(&self) -> bool {
        true
    }

    fn description(&self) -> &str {
        "Apply an exact string replacement in a file. \
         Use this for surgical edits to existing files (e.g., fix a bug, rename a variable). \
         Uses atomic write (temp file + rename) to prevent data loss on failure. \
         If the old string matches multiple times, the operation is rejected — \
         provide more surrounding context in the old string to make it unique."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to edit, relative to the project root."
                },
                "old_string": {
                    "type": "string",
                    "description": "Exact string to search for in the file."
                },
                "new_string": {
                    "type": "string",
                    "description": "String to replace the old string with."
                }
            },
            "required": ["path", "old_string", "new_string"]
        })
    }

    async fn execute(&self, arguments: serde_json::Value, context: &ToolContext) -> ToolResult {
        let path_str = match arguments.get("path").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => return ToolResult::error("Missing required argument: path"),
        };
        let old_string = match arguments.get("old_string").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s,
            Some(_) => return ToolResult::error("old_string is empty"),
            None => return ToolResult::error("Missing required argument: old_string"),
        };
        let new_string = match arguments.get("new_string").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => return ToolResult::error("Missing required argument: new_string"),
        };

        let path = match validate_path(Path::new(path_str), &context.working_dir) {
            Ok(p) => p,
            Err(e) => return ToolResult::error(e),
        };

        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => return ToolResult::error(format!("Failed to read file: {}", e)),
        };

        // 查找匹配次数和位置
        let matches: Vec<_> = content.match_indices(old_string).collect();
        let count = matches.len();

        if count == 0 {
            return ToolResult::error(format!("No matches found for '{}'", old_string));
        }

        if count > 1 {
            let positions: Vec<String> = matches
                .iter()
                .map(|(pos, _)| {
                    let line_num = content[..*pos].matches('\n').count() + 1;
                    let last_newline = content[..*pos].rfind('\n').map(|i| i + 1).unwrap_or(0);
                    let col = pos - last_newline + 1;
                    format!("line {}, column {}", line_num, col)
                })
                .collect();
            return ToolResult::error(format!(
                "Found {} matches for '{}' at: {}. Use WriteFile for multiple replacements",
                count,
                old_string,
                positions.join("; ")
            ));
        }

        // 单次匹配，执行替换
        let new_content = content.replace(old_string, new_string);

        // 原子写入：先写临时文件再 rename
        let temp_name = format!(".{}.visp-tmp", path.file_name().unwrap().to_string_lossy());
        let temp_path = path.with_file_name(&temp_name);

        if let Err(e) = fs::write(&temp_path, &new_content) {
            let _ = fs::remove_file(&temp_path);
            return ToolResult::error(format!("Failed to write temp file: {}", e));
        }

        match fs::rename(&temp_path, &path) {
            Ok(_) => ToolResult::success(format!("Replaced 1 occurrence in {}", path.display())),
            Err(e) => {
                let _ = fs::remove_file(&temp_path);
                ToolResult::error(format!("Failed to rename temp file: {}", e))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 内部辅助函数

/// 验证写入路径安全性（文件可能不存在，尝试从最深存在的祖先进行验证）
fn validate_write_path(target: &Path, working_dir: &Path) -> Result<PathBuf, String> {
    // 文件已存在 — 直接用标准 validate_path
    if let Ok(p) = validate_path(target, working_dir) {
        return Ok(p);
    }

    let abs = working_dir.join(target);
    let real_working = fs::canonicalize(working_dir)
        .map_err(|e| format!("Working directory resolution failed: {}", e))?;

    // 从 abs 向上遍历，找到第一个存在的祖先进行 canonicalize
    let mut current = Some(abs.as_path());
    while let Some(path) = current {
        match fs::canonicalize(path) {
            Ok(real) => {
                if !real.starts_with(&real_working) {
                    return Err("Path is outside the working directory".to_string());
                }
                // 将剩余的路径组件拼接到 canonical 结果上
                let remaining = abs
                    .strip_prefix(path)
                    .map_err(|_| "Unexpected path resolution error".to_string())?;
                return Ok(real.join(remaining));
            }
            Err(_) => {
                current = path.parent();
            }
        }
    }

    Err(format!("Cannot resolve path: {}", abs.display()))
}

// ---------------------------------------------------------------------------
// 测试

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // ---- ReadFile ----

    #[test]
    fn test_read_file_success() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("test.txt");
        std::fs::write(&file_path, "hello world").unwrap();

        let tool = ReadFile;
        let ctx = ToolContext {
            working_dir: tmp.path().to_path_buf(),
            session_id: None,
        };
        let args = serde_json::json!({"path": "test.txt"});

        let result = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(tool.execute(args, &ctx));
        assert!(
            !result.is_error,
            "expected success, got error: {}",
            result.content
        );
        assert_eq!(result.content, "hello world");
    }

    #[test]
    fn test_read_file_not_found() {
        let tmp = TempDir::new().unwrap();
        let tool = ReadFile;
        let ctx = ToolContext {
            working_dir: tmp.path().to_path_buf(),
            session_id: None,
        };
        let args = serde_json::json!({"path": "nonexistent.txt"});

        let result = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(tool.execute(args, &ctx));
        assert!(result.is_error);
    }

    #[test]
    fn test_read_file_too_large() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("large.txt");
        let content = vec![b'a'; (MAX_FILE_SIZE + 1) as usize];
        std::fs::write(&file_path, &content).unwrap();

        let tool = ReadFile;
        let ctx = ToolContext {
            working_dir: tmp.path().to_path_buf(),
            session_id: None,
        };
        let args = serde_json::json!({"path": "large.txt"});

        let result = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(tool.execute(args, &ctx));
        assert!(result.is_error);
        assert!(result.content.contains("too large"));
    }

    #[test]
    fn test_read_file_binary() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("binary.bin");
        let mut content = vec![0u8; 2000]; // 2000 null bytes > 10% of 8000
        content[0] = b'h';
        content[1] = b'i';
        std::fs::write(&file_path, &content).unwrap();

        let tool = ReadFile;
        let ctx = ToolContext {
            working_dir: tmp.path().to_path_buf(),
            session_id: None,
        };
        let args = serde_json::json!({"path": "binary.bin"});

        let result = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(tool.execute(args, &ctx));
        assert!(result.is_error);
        assert!(result.content.contains("binary"));
    }

    // ---- WriteFile ----

    #[test]
    fn test_write_file_success() {
        let tmp = TempDir::new().unwrap();
        let tool = WriteFile;
        let ctx = ToolContext {
            working_dir: tmp.path().to_path_buf(),
            session_id: None,
        };
        let args = serde_json::json!({"path": "written.txt", "content": "hello write"});

        let result = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(tool.execute(args, &ctx));
        assert!(
            !result.is_error,
            "expected success, got error: {}",
            result.content
        );

        let read_back = std::fs::read_to_string(tmp.path().join("written.txt")).unwrap();
        assert_eq!(read_back, "hello write");
    }

    #[test]
    fn test_write_file_auto_create_parent() {
        let tmp = TempDir::new().unwrap();
        let tool = WriteFile;
        let ctx = ToolContext {
            working_dir: tmp.path().to_path_buf(),
            session_id: None,
        };
        let args = serde_json::json!({"path": "subdir/nested/file.txt", "content": "nested"});

        let result = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(tool.execute(args, &ctx));
        assert!(
            !result.is_error,
            "expected success, got error: {}",
            result.content
        );

        let target = tmp.path().join("subdir/nested/file.txt");
        assert!(target.exists(), "file should exist");
        let read_back = std::fs::read_to_string(&target).unwrap();
        assert_eq!(read_back, "nested");
    }

    // ---- EditFile ----

    #[test]
    fn test_edit_file_success() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("edit.txt");
        std::fs::write(&file_path, "hello world foo").unwrap();

        let tool = EditFile;
        let ctx = ToolContext {
            working_dir: tmp.path().to_path_buf(),
            session_id: None,
        };
        let args = serde_json::json!({
            "path": "edit.txt",
            "old_string": "world",
            "new_string": "there"
        });

        let result = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(tool.execute(args, &ctx));
        assert!(
            !result.is_error,
            "expected success, got error: {}",
            result.content
        );

        let read_back = std::fs::read_to_string(&file_path).unwrap();
        assert_eq!(read_back, "hello there foo");
    }

    #[test]
    fn test_edit_file_no_match() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("edit.txt");
        std::fs::write(&file_path, "hello world").unwrap();

        let tool = EditFile;
        let ctx = ToolContext {
            working_dir: tmp.path().to_path_buf(),
            session_id: None,
        };
        let args = serde_json::json!({
            "path": "edit.txt",
            "old_string": "xyz",
            "new_string": "abc"
        });

        let result = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(tool.execute(args, &ctx));
        assert!(result.is_error);
        assert!(result.content.contains("No matches"));
    }

    #[test]
    fn test_edit_file_multiple_matches() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("edit.txt");
        std::fs::write(&file_path, "foo bar foo baz foo").unwrap();

        let tool = EditFile;
        let ctx = ToolContext {
            working_dir: tmp.path().to_path_buf(),
            session_id: None,
        };
        let args = serde_json::json!({
            "path": "edit.txt",
            "old_string": "foo",
            "new_string": "xyz"
        });

        let result = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(tool.execute(args, &ctx));
        assert!(result.is_error);
        assert!(result.content.contains("matches"));
        // Should mention line/column positions
        assert!(result.content.contains("line"));
        assert!(result.content.contains("column"));
    }

    #[test]
    fn test_edit_file_atomic_write() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("edit.txt");
        std::fs::write(&file_path, "hello world").unwrap();

        let tool = EditFile;
        let ctx = ToolContext {
            working_dir: tmp.path().to_path_buf(),
            session_id: None,
        };
        let args = serde_json::json!({
            "path": "edit.txt",
            "old_string": "world",
            "new_string": "there"
        });

        let result = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(tool.execute(args, &ctx));
        assert!(
            !result.is_error,
            "expected success, got error: {}",
            result.content
        );

        // 验证内容
        let read_back = std::fs::read_to_string(&file_path).unwrap();
        assert_eq!(read_back, "hello there");

        // 验证临时文件已清理
        let temp_name = format!(
            ".{}.visp-tmp",
            file_path.file_name().unwrap().to_string_lossy()
        );
        let temp_path = file_path.with_file_name(&temp_name);
        assert!(!temp_path.exists(), "temp file should be cleaned up");
    }
}
