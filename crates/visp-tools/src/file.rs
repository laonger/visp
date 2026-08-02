use crate::path::validate_path;
use crate::truncate::{DEFAULT_MAX_OUTPUT_BYTES, truncate_output};
use async_trait::async_trait;
use std::fs;
use std::path::{Path, PathBuf};
use visp_core::tool::{Tool, ToolContext, ToolResult};

const MAX_FILE_SIZE: u64 = 1_048_576; // 1MB
const BINARY_SCAN_BYTES: usize = 8000;

const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp", "ico"];

/// 读取文件
pub struct ReadFile {
    max_file_size: u64,
}

impl Default for ReadFile {
    fn default() -> Self {
        Self {
            max_file_size: MAX_FILE_SIZE,
        }
    }
}

impl ReadFile {
    pub fn from_toml(raw: Option<&toml::Value>) -> Self {
        let mut max_file_size = MAX_FILE_SIZE;
        if let Some(config) = raw.and_then(|v| v.as_table())
            && let Some(s) = config.get("max_file_size").and_then(|v| v.as_integer())
            && s > 0
        {
            max_file_size = s as u64;
        }
        Self { max_file_size }
    }

    /// 读取单个文件，返回格式化后的内容
    fn read_single_file(
        &self,
        path_str: &str,
        working_dir: &Path,
        start_line: Option<i64>,
        end_line: Option<i64>,
    ) -> Result<String, String> {
        let path = validate_path(Path::new(path_str), working_dir)?;

        // Image file detection: return marker instead of reading binary content
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            if IMAGE_EXTENSIONS.contains(&ext.to_lowercase().as_str()) {
                return Ok(format!("<image: {}>", path.display()));
            }
        }

        // 检查文件大小
        let metadata =
            fs::metadata(&path).map_err(|e| format!("Failed to read file metadata: {}", e))?;

        if metadata.len() > self.max_file_size {
            return Err(format!(
                "File too large: {} bytes (max {} bytes)",
                metadata.len(),
                self.max_file_size
            ));
        }

        // 读取内容
        let bytes = fs::read(&path).map_err(|e| format!("Failed to read file: {}", e))?;

        // 二进制检测（前 8000 字节）
        let scan_end = BINARY_SCAN_BYTES.min(bytes.len());
        if scan_end > 0 {
            let null_count = bytes[..scan_end].iter().filter(|&&b| b == 0).count();
            if null_count > scan_end / 10 {
                return Err(format!(
                    "File appears to be binary ({} null bytes in first {} bytes)",
                    null_count, scan_end
                ));
            }
        }

        let content =
            String::from_utf8(bytes).map_err(|_| "File is not valid UTF-8".to_string())?;

        // 应用行范围
        let result = if start_line.is_some() || end_line.is_some() {
            let lines: Vec<&str> = content.lines().collect();
            let total_lines = lines.len();

            let start = start_line
                .map(|n| (n as usize).saturating_sub(1))
                .unwrap_or(0);
            let end = end_line
                .map(|n| (n as usize).saturating_sub(1))
                .unwrap_or(total_lines.saturating_sub(1));

            if start >= total_lines {
                return Err(format!(
                    "start_line {} exceeds file length ({} lines)",
                    start + 1,
                    total_lines
                ));
            }
            if end >= total_lines {
                return Err(format!(
                    "end_line {} exceeds file length ({} lines)",
                    end + 1,
                    total_lines
                ));
            }
            if start > end {
                return Err(format!(
                    "start_line {} is after end_line {}",
                    start + 1,
                    end + 1
                ));
            }

            let excerpt: String = lines[start..=end].join("\n");
            let range_info = format!(
                "{} (lines {}-{}, {} of {} lines):\n",
                path.display(),
                start + 1,
                end + 1,
                end - start + 1,
                total_lines
            );
            format!("{}{}", range_info, excerpt)
        } else {
            content.to_string()
        };

        Ok(result)
    }
}

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
         You can optionally specify a line range (start_line and/or end_line) to read \
         only a portion of the file, which is useful for large files. \
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
                },
                "paths": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Multiple files to read. Each is relative to the project root. Mutually exclusive with 'path'; if both are provided, 'path' takes priority."
                },
                "start_line": {
                    "type": "integer",
                    "description": "Optional 1-based line number to start reading from (inclusive).",
                    "minimum": 1
                },
                "end_line": {
                    "type": "integer",
                    "description": "Optional 1-based line number to stop reading at (inclusive). If start_line is set but end_line is not, reads from start_line to end of file.",
                    "minimum": 1
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, arguments: serde_json::Value, context: &ToolContext) -> ToolResult {
        let start_line = arguments.get("start_line").and_then(|v| v.as_i64());
        let end_line = arguments.get("end_line").and_then(|v| v.as_i64());

        // path 优先（向后兼容）
        if let Some(path_str) = arguments.get("path").and_then(|v| v.as_str()) {
            return match self.read_single_file(path_str, &context.working_dir, start_line, end_line)
            {
                Ok(content) => {
                    ToolResult::success(truncate_output(&content, DEFAULT_MAX_OUTPUT_BYTES))
                }
                Err(e) => ToolResult::error(e),
            };
        }

        // 尝试 paths（多文件模式）
        if let Some(paths) = arguments.get("paths").and_then(|v| v.as_array()) {
            if paths.is_empty() {
                return ToolResult::error("paths is empty");
            }

            let mut results: Vec<String> = Vec::new();
            let mut any_success = false;

            for path_val in paths {
                let entry = match path_val.as_str() {
                    Some(s) => {
                        match self.read_single_file(s, &context.working_dir, start_line, end_line) {
                            Ok(content) => {
                                any_success = true;
                                format!("=== {} ===\n{}", s, content)
                            }
                            Err(e) => format!("=== {} ===\nError: {}", s, e),
                        }
                    }
                    None => continue,
                };
                results.push(entry);
            }

            let output = results.join("\n\n");
            if any_success {
                ToolResult::success(truncate_output(&output, DEFAULT_MAX_OUTPUT_BYTES))
            } else {
                ToolResult::error(output)
            }
        } else {
            ToolResult::error("Missing required argument: path")
        }
    }
}

// ---------------------------------------------------------------------------

/// 写入文件（覆盖）
pub struct WriteFile {
    /// 仅兼容，不使用
    #[allow(dead_code)]
    max_file_size: u64,
    require_approval: bool,
}

impl Default for WriteFile {
    fn default() -> Self {
        Self {
            max_file_size: MAX_FILE_SIZE,
            require_approval: false,
        }
    }
}

impl WriteFile {
    pub fn from_toml(raw: Option<&toml::Value>) -> Self {
        let mut max_file_size = MAX_FILE_SIZE;
        let mut require_approval = false;
        if let Some(config) = raw.and_then(|v| v.as_table()) {
            if let Some(s) = config.get("max_file_size").and_then(|v| v.as_integer())
                && s > 0
            {
                max_file_size = s as u64;
            }
            if let Some(b) = config.get("require_approval").and_then(|v| v.as_bool()) {
                require_approval = b;
            }
        }
        Self {
            max_file_size,
            require_approval,
        }
    }
}

#[async_trait]
impl Tool for WriteFile {
    fn name(&self) -> &str {
        "write_file"
    }

    fn category(&self) -> &str {
        "common"
    }

    fn requires_approval(&self) -> bool {
        self.require_approval
    }

    fn description(&self) -> &str {
        "Write content to a file, creating or overwriting it. \
         Use this to create new files or completely rewrite entire existing files. \
         For targeted edits (changing a few lines in an existing file), \
         prefer `edit_file` instead. \
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
                    "minLength": 1,
                    "description": "Target file path, relative to the project root."
                },
                "content": {
                    "type": "string",
                    "minLength": 1,
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
                "Written {} bytes to {}\n{}",
                content.len(),
                path.display(),
                content
            )),
            Err(e) => ToolResult::error(format!("Failed to write file: {}", e)),
        }
    }
}

// ---------------------------------------------------------------------------

/// 精确字符串替换编辑
pub struct EditFile {
    max_file_size: u64,
    require_approval: bool,
}

impl Default for EditFile {
    fn default() -> Self {
        Self {
            max_file_size: MAX_FILE_SIZE,
            require_approval: false,
        }
    }
}

impl EditFile {
    pub fn from_toml(raw: Option<&toml::Value>) -> Self {
        let mut max_file_size = MAX_FILE_SIZE;
        let mut require_approval = false;
        if let Some(config) = raw.and_then(|v| v.as_table()) {
            if let Some(s) = config.get("max_file_size").and_then(|v| v.as_integer())
                && s > 0
            {
                max_file_size = s as u64;
            }
            if let Some(b) = config.get("require_approval").and_then(|v| v.as_bool()) {
                require_approval = b;
            }
        }
        Self {
            max_file_size,
            require_approval,
        }
    }
}

#[async_trait]
impl Tool for EditFile {
    fn name(&self) -> &str {
        "edit_file"
    }

    fn category(&self) -> &str {
        "common"
    }

    fn requires_approval(&self) -> bool {
        self.require_approval
    }

    fn description(&self) -> &str {
        "Apply an exact string replacement in a file. \
         Use this for surgical edits to existing files (e.g., fix a bug, rename a variable). \
         This is the preferred tool for targeted changes — \
         for creating new files or full rewrites, use `write_file` instead. \
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
                    "minLength": 1,
                    "description": "Path to the file to edit, relative to the project root."
                },
                "old_string": {
                    "type": "string",
                    "minLength": 1,
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

        // 检查文件大小
        if content.len() as u64 > self.max_file_size {
            return ToolResult::error(format!(
                "File too large: {} bytes (max {} bytes)",
                content.len(),
                self.max_file_size
            ));
        }

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
            Ok(_) => {
                // 生成 unified diff：原内容 vs 新内容
                let diff_output = diff_text(&path, &content, &new_content);
                ToolResult::success(format!(
                    "Replaced 1 occurrence in {}\n{}",
                    path.display(),
                    diff_output
                ))
            }
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

/// 生成两个文本之间的 unified diff（仅显示有变化的 hunk + 上下文）
fn diff_text(path: &Path, old: &str, new: &str) -> String {
    use similar::TextDiff;

    TextDiff::from_lines(old, new)
        .unified_diff()
        .context_radius(3)
        .header(
            &format!("a/{}", path.display()),
            &format!("b/{}", path.display()),
        )
        .to_string()
}

#[cfg(test)]
#[path = "file_tests.rs"]
mod tests;
