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
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs as unix_fs;
    use tempfile::TempDir;

    #[test]
    fn test_valid_path() {
        let tmp = TempDir::new().unwrap();
        let working_dir = tmp.path();
        let file_path = working_dir.join("test.txt");
        fs::write(&file_path, "hello").unwrap();

        let result = validate_path(Path::new("test.txt"), working_dir);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), fs::canonicalize(&file_path).unwrap());
    }

    #[test]
    fn test_parent_traversal_rejected() {
        let tmp = TempDir::new().unwrap();
        let working_dir = tmp.path();

        let result = validate_path(Path::new("../outside"), working_dir);
        assert!(result.is_err());
    }

    #[test]
    fn test_symlink_bypass_rejected() {
        let tmp = TempDir::new().unwrap();
        let working_dir = tmp.path();

        // 在 working_dir 外创建一个目录
        let outside_dir = TempDir::new().unwrap();

        // 在 working_dir 内创建指向外部的符号链接
        let link_path = working_dir.join("evil_link");
        unix_fs::symlink(outside_dir.path(), &link_path).unwrap();

        let result = validate_path(Path::new("evil_link"), working_dir);
        assert!(result.is_err());
    }
}
