use std::path::{Path, PathBuf};

/// 返回用户的 home 目录路径
pub fn home_dir() -> Option<PathBuf> {
    std::env::var("HOME").ok().map(PathBuf::from)
}

/// 返回当前工作目录
pub fn project_dir() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// 返回 `{project}/.visp` 目录
pub fn visp_dir(project: &Path) -> PathBuf {
    project.join(".visp")
}

/// 全局配置目录根。
///
/// 优先级：
/// 1. 环境变量 `VISP_CONFIG_DIR`（可由 `--config-dir` CLI 参数设置）
/// 2. `~/.config/visp`（默认）
///
/// 所有 `*_global()` 路径函数都基于此，因此设置 `VISP_CONFIG_DIR`
/// 即可重定向 daemon.toml / rules / skills / agents / AGENTS.md 等全部全局配置。
pub fn global_config_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("VISP_CONFIG_DIR")
        && !dir.is_empty()
    {
        return Some(PathBuf::from(dir));
    }
    home_dir().map(|h| h.join(".config").join("visp"))
}

/// 全局数据目录根（logs 等）。
///
/// 优先级：
/// 1. 环境变量 `VISP_DATA_DIR`
/// 2. `~/.visp`（默认）
pub fn global_data_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("VISP_DATA_DIR")
        && !dir.is_empty()
    {
        return Some(PathBuf::from(dir));
    }
    home_dir().map(|h| h.join(".visp"))
}

/// 返回 `{project}/.visp/daemon.toml`
pub fn daemon_toml_project(project: &Path) -> PathBuf {
    visp_dir(project).join("daemon.toml")
}

/// 返回 `~/.config/visp/daemon.toml`
pub fn daemon_toml_global() -> Option<PathBuf> {
    global_config_dir().map(|d| d.join("daemon.toml"))
}

/// 返回 `{project}/.visp/rules`
pub fn rules_dir_project(project: &Path) -> PathBuf {
    visp_dir(project).join("rules")
}

/// 返回 `~/.config/visp/rules`
pub fn rules_dir_global() -> Option<PathBuf> {
    global_config_dir().map(|d| d.join("rules"))
}

/// 返回 `{project}/.visp/skills`
pub fn skills_dir_project(project: &Path) -> PathBuf {
    visp_dir(project).join("skills")
}

/// 返回 `~/.config/visp/skills`
pub fn skills_dir_global() -> Option<PathBuf> {
    global_config_dir().map(|d| d.join("skills"))
}

/// 返回 `{project}/.visp/agents`
pub fn agents_dir_project(project: &Path) -> PathBuf {
    visp_dir(project).join("agents")
}

/// 返回 `~/.config/visp/agents`
pub fn agents_dir_global() -> Option<PathBuf> {
    global_config_dir().map(|d| d.join("agents"))
}

/// 返回 `{project}/.visp/webfetch.toml`
pub fn webfetch_toml_project(project: &Path) -> PathBuf {
    visp_dir(project).join("webfetch.toml")
}

/// 返回 `{project}/.visp/system-prompt.md`
pub fn system_prompt_project(project: &Path) -> PathBuf {
    visp_dir(project).join("system-prompt.md")
}

/// 返回 `~/.config/visp/system-prompt.md`
pub fn system_prompt_global() -> Option<PathBuf> {
    global_config_dir().map(|d| d.join("system-prompt.md"))
}

/// 返回 `~/.config/visp/AGENTS.md`
pub fn global_agents_md() -> Option<PathBuf> {
    global_config_dir().map(|d| d.join("AGENTS.md"))
}

/// 返回 `{project}/.visp/codegraph.db`
pub fn codegraph_db(project: &Path) -> PathBuf {
    visp_dir(project).join("codegraph.db")
}

/// 返回 `~/.visp/logs`（运行时日志目录）
pub fn log_dir() -> Option<PathBuf> {
    global_data_dir().map(|d| d.join("logs"))
}

/// 返回 `{temp_dir}/.visp/images`（图片缓存目录）
pub fn image_cache_dir() -> PathBuf {
    std::env::temp_dir().join(".visp").join("images")
}

/// 展开 `~/` 为 `$HOME/`，否则原样返回。
///
/// 若 `path` 以 `~/` 开头但 HOME 未设置，返回 `PathBuf::from(path)`（不展开）。
pub fn expand_home(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        home_dir()
            .map(|home| home.join(rest))
            .unwrap_or_else(|| PathBuf::from(path))
    } else {
        PathBuf::from(path)
    }
}

/// 返回 `~/.config/visp/.startup-error`（daemon 启动失败时写入，launcher 读取后删除）
pub fn startup_error_file() -> Option<PathBuf> {
    global_config_dir().map(|d| d.join(".startup-error"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    fn set_home(val: Option<&str>) {
        match val {
            Some(v) => unsafe { std::env::set_var("HOME", v) },
            None => unsafe { std::env::remove_var("HOME") },
        }
    }

    #[test]
    fn test_project_dir() {
        let dir = project_dir();
        assert!(dir.is_absolute() || dir == *".");
    }

    #[test]
    fn test_visp_dir() {
        let project = Path::new("/my/project");
        assert_eq!(visp_dir(project), PathBuf::from("/my/project/.visp"));
    }

    #[test]
    #[serial]
    fn test_global_config_dir() {
        set_home(Some("/home/user"));
        assert_eq!(
            global_config_dir(),
            Some(PathBuf::from("/home/user/.config/visp"))
        );
    }

    #[test]
    #[serial]
    fn test_global_config_dir_no_home() {
        set_home(None);
        assert_eq!(global_config_dir(), None);
    }

    #[test]
    #[serial]
    fn test_global_config_dir_env_override() {
        set_home(Some("/home/user"));
        unsafe { std::env::set_var("VISP_CONFIG_DIR", "/custom/config") };
        assert_eq!(global_config_dir(), Some(PathBuf::from("/custom/config")));
        unsafe { std::env::remove_var("VISP_CONFIG_DIR") };
    }

    #[test]
    #[serial]
    fn test_global_config_dir_env_empty_falls_back() {
        set_home(Some("/home/user"));
        unsafe { std::env::set_var("VISP_CONFIG_DIR", "") };
        assert_eq!(
            global_config_dir(),
            Some(PathBuf::from("/home/user/.config/visp"))
        );
        unsafe { std::env::remove_var("VISP_CONFIG_DIR") };
    }

    #[test]
    #[serial]
    fn test_global_data_dir_env_override() {
        set_home(Some("/home/user"));
        unsafe { std::env::set_var("VISP_DATA_DIR", "/custom/data") };
        assert_eq!(global_data_dir(), Some(PathBuf::from("/custom/data")));
        unsafe { std::env::remove_var("VISP_DATA_DIR") };
    }

    #[test]
    #[serial]
    fn test_daemon_toml_global_env_override() {
        set_home(Some("/home/user"));
        unsafe { std::env::set_var("VISP_CONFIG_DIR", "/custom/config") };
        assert_eq!(
            daemon_toml_global(),
            Some(PathBuf::from("/custom/config/daemon.toml"))
        );
        unsafe { std::env::remove_var("VISP_CONFIG_DIR") };
    }

    #[test]
    #[serial]
    fn test_global_data_dir() {
        set_home(Some("/home/user"));
        assert_eq!(global_data_dir(), Some(PathBuf::from("/home/user/.visp")));
    }

    #[test]
    #[serial]
    fn test_daemon_toml_paths() {
        let project = Path::new("/proj");
        assert_eq!(
            daemon_toml_project(project),
            PathBuf::from("/proj/.visp/daemon.toml")
        );
        set_home(Some("/home/user"));
        assert_eq!(
            daemon_toml_global(),
            Some(PathBuf::from("/home/user/.config/visp/daemon.toml"))
        );
    }

    #[test]
    #[serial]
    fn test_rules_dir_paths() {
        let project = Path::new("/proj");
        assert_eq!(
            rules_dir_project(project),
            PathBuf::from("/proj/.visp/rules")
        );
        set_home(Some("/home/user"));
        assert_eq!(
            rules_dir_global(),
            Some(PathBuf::from("/home/user/.config/visp/rules"))
        );
    }

    #[test]
    #[serial]
    fn test_skills_dir_paths() {
        let project = Path::new("/proj");
        assert_eq!(
            skills_dir_project(project),
            PathBuf::from("/proj/.visp/skills")
        );
        set_home(Some("/home/user"));
        assert_eq!(
            skills_dir_global(),
            Some(PathBuf::from("/home/user/.config/visp/skills"))
        );
    }

    #[test]
    #[serial]
    fn test_agents_dir_paths() {
        let project = Path::new("/proj");
        assert_eq!(
            agents_dir_project(project),
            PathBuf::from("/proj/.visp/agents")
        );
        set_home(Some("/home/user"));
        assert_eq!(
            agents_dir_global(),
            Some(PathBuf::from("/home/user/.config/visp/agents"))
        );
    }

    #[test]
    fn test_webfetch_toml_project() {
        let project = Path::new("/proj");
        assert_eq!(
            webfetch_toml_project(project),
            PathBuf::from("/proj/.visp/webfetch.toml")
        );
    }

    #[test]
    #[serial]
    fn test_system_prompt_paths() {
        let project = Path::new("/proj");
        assert_eq!(
            system_prompt_project(project),
            PathBuf::from("/proj/.visp/system-prompt.md")
        );
        set_home(Some("/home/user"));
        assert_eq!(
            system_prompt_global(),
            Some(PathBuf::from("/home/user/.config/visp/system-prompt.md"))
        );
    }

    #[test]
    #[serial]
    fn test_global_agents_md() {
        set_home(Some("/home/user"));
        assert_eq!(
            global_agents_md(),
            Some(PathBuf::from("/home/user/.config/visp/AGENTS.md"))
        );
    }

    #[test]
    fn test_codegraph_db() {
        let project = Path::new("/proj");
        assert_eq!(
            codegraph_db(project),
            PathBuf::from("/proj/.visp/codegraph.db")
        );
    }

    #[test]
    #[serial]
    fn test_log_dir() {
        set_home(Some("/home/user"));
        assert_eq!(log_dir(), Some(PathBuf::from("/home/user/.visp/logs")));
    }

    #[test]
    fn test_image_cache_dir() {
        let dir = image_cache_dir();
        assert!(dir.starts_with(std::env::temp_dir()));
        assert!(dir.ends_with(".visp/images"));
    }

    #[test]
    #[serial]
    fn test_expand_home_with_tilde() {
        set_home(Some("/home/user"));
        assert_eq!(expand_home("~/foo"), PathBuf::from("/home/user/foo"));
    }

    #[test]
    fn test_expand_home_without_tilde() {
        assert_eq!(expand_home("/abs/path"), PathBuf::from("/abs/path"));
        assert_eq!(expand_home("relative/path"), PathBuf::from("relative/path"));
    }

    #[test]
    #[serial]
    fn test_expand_home_no_home() {
        set_home(None);
        // When HOME is not set, return the path as-is
        assert_eq!(expand_home("~/foo"), PathBuf::from("~/foo"));
    }

    #[test]
    #[serial]
    fn test_home_dir() {
        set_home(Some("/home/user"));
        assert_eq!(home_dir(), Some(PathBuf::from("/home/user")));
    }
}
