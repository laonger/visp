use std::fs;
use std::path::{Path, PathBuf};

/// 校验路径是否在 working_dir 内（含符号链接解析）
/// 返回解析后的绝对路径
pub fn validate_path(target: &Path, working_dir: &Path) -> Result<PathBuf, String> {
    let joined = working_dir.join(target);
    let real = fs::canonicalize(&joined).map_err(|e| format!("Path resolution failed: {}", e))?;
    let real_working = fs::canonicalize(working_dir)
        .map_err(|e| format!("Working directory resolution failed: {}", e))?;
    if real.starts_with(&real_working) {
        Ok(real)
    } else {
        Err(format!(
            "Path {} is outside the working directory {}",
            real.display(),
            real_working.display()
        ))
    }
}

#[cfg(test)]
#[path = "path_tests.rs"]
mod tests;
