use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

#[derive(Debug, Clone)]
pub struct RuleFile {
    pub path: PathBuf,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct RuleSet {
    pub content: String,
    pub files: Vec<RuleFile>,
}

#[derive(Debug)]
pub struct RuleEngine {
    rules: Arc<RwLock<RuleSet>>,
}

impl RuleEngine {
    pub fn new(project_path: &Path) -> std::io::Result<Self> {
        let mut files = Vec::new();

        // Project rules: .vibewisp/rules/
        let project_rules = project_path.join(".vibewisp").join("rules");
        if project_rules.is_dir() {
            collect_rules(&project_rules, &mut files)?;
        }

        // Global rules: ~/.config/vibewisp/rules/
        if let Some(home) = home_dir() {
            let global_rules = home.join(".config").join("vibewisp").join("rules");
            if global_rules.is_dir() {
                collect_rules(&global_rules, &mut files)?;
            }
        }

        let content = files
            .iter()
            .map(|f| f.content.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");

        Ok(RuleEngine {
            rules: Arc::new(RwLock::new(RuleSet { content, files })),
        })
    }

    pub fn get_active_rules(&self) -> String {
        self.rules.read().unwrap().content.clone()
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var("HOME").ok().map(PathBuf::from)
}

pub(crate) fn collect_rules(dir: &Path, files: &mut Vec<RuleFile>) -> std::io::Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
        .collect();

    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let path = entry.path();
        let content = std::fs::read_to_string(&path)?;
        if has_always_apply_true(&content) {
            files.push(RuleFile { path, content });
        }
    }

    Ok(())
}

fn has_always_apply_true(content: &str) -> bool {
    content
        .lines()
        .take(5)
        .any(|line| line.trim().contains("alwaysApply: true"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_loads_always_apply_true() {
        let dir = tempdir().unwrap();
        let rules_dir = dir.path().join(".vibewisp").join("rules");
        fs::create_dir_all(&rules_dir).unwrap();

        fs::write(
            rules_dir.join("test.md"),
            "---\nalwaysApply: true\n---\n# My Rule\nContent here\n",
        )
        .unwrap();

        let engine = RuleEngine::new(dir.path()).unwrap();
        let rules = engine.get_active_rules();
        assert!(rules.contains("# My Rule"));
        assert!(rules.contains("Content here"));
    }

    #[test]
    fn test_skips_always_apply_false() {
        let dir = tempdir().unwrap();
        let rules_dir = dir.path().join(".vibewisp").join("rules");
        fs::create_dir_all(&rules_dir).unwrap();

        fs::write(
            rules_dir.join("test.md"),
            "---\nalwaysApply: false\n---\n# Rule\ncontent",
        )
        .unwrap();

        let engine = RuleEngine::new(dir.path()).unwrap();
        assert!(engine.get_active_rules().is_empty());
    }

    #[test]
    fn test_skips_no_marker() {
        let dir = tempdir().unwrap();
        let rules_dir = dir.path().join(".vibewisp").join("rules");
        fs::create_dir_all(&rules_dir).unwrap();

        fs::write(
            rules_dir.join("test.md"),
            "# Just a regular file\nno marker here",
        )
        .unwrap();

        let engine = RuleEngine::new(dir.path()).unwrap();
        assert!(engine.get_active_rules().is_empty());
    }

    #[test]
    fn test_skips_non_md() {
        let dir = tempdir().unwrap();
        let rules_dir = dir.path().join(".vibewisp").join("rules");
        fs::create_dir_all(&rules_dir).unwrap();

        fs::write(
            rules_dir.join("test.txt"),
            "alwaysApply: true\nText content",
        )
        .unwrap();
        fs::write(rules_dir.join("test.md"), "alwaysApply: true\n# Real rule").unwrap();

        let engine = RuleEngine::new(dir.path()).unwrap();
        let rules = engine.get_active_rules();
        assert!(rules.contains("# Real rule"));
        assert!(!rules.contains("Text content"));
    }

    #[test]
    fn test_missing_dir_no_error() {
        let dir = tempdir().unwrap();
        let engine = RuleEngine::new(dir.path());
        assert!(engine.is_ok());
        assert!(engine.unwrap().get_active_rules().is_empty());
    }

    #[test]
    fn test_multiple_files_sorted() {
        let dir = tempdir().unwrap();
        let rules_dir = dir.path().join(".vibewisp").join("rules");
        fs::create_dir_all(&rules_dir).unwrap();

        fs::write(rules_dir.join("b.md"), "alwaysApply: true\n# B rule").unwrap();
        fs::write(rules_dir.join("a.md"), "alwaysApply: true\n# A rule").unwrap();
        fs::write(rules_dir.join("c.md"), "alwaysApply: true\n# C rule").unwrap();

        let engine = RuleEngine::new(dir.path()).unwrap();
        let rules = engine.get_active_rules();

        let a_pos = rules.find("# A rule").unwrap();
        let b_pos = rules.find("# B rule").unwrap();
        let c_pos = rules.find("# C rule").unwrap();

        assert!(a_pos < b_pos);
        assert!(b_pos < c_pos);
    }

    #[test]
    fn test_project_before_global_order() {
        // Test ordering via collect_rules directly, avoiding env var manipulation.
        let project_dir = tempdir().unwrap();
        let global_dir = tempdir().unwrap();

        fs::write(
            project_dir.path().join("a.md"),
            "alwaysApply: true\n# Project rule",
        )
        .unwrap();
        fs::write(
            global_dir.path().join("a.md"),
            "alwaysApply: true\n# Global rule",
        )
        .unwrap();

        let mut files = Vec::new();
        collect_rules(project_dir.path(), &mut files).unwrap();
        collect_rules(global_dir.path(), &mut files).unwrap();

        assert_eq!(files.len(), 2);
        assert!(files[0].content.contains("# Project rule"));
        assert!(files[1].content.contains("# Global rule"));
    }

    #[test]
    fn test_check_only_first_five_lines() {
        let dir = tempdir().unwrap();
        let rules_dir = dir.path().join(".vibewisp").join("rules");
        fs::create_dir_all(&rules_dir).unwrap();

        // Marker beyond 5th line should NOT be detected
        let content = "line1\nline2\nline3\nline4\nline5\nalwaysApply: true\n# Should be ignored";
        fs::write(rules_dir.join("test.md"), content).unwrap();

        let engine = RuleEngine::new(dir.path()).unwrap();
        assert!(engine.get_active_rules().is_empty());
    }

    #[test]
    fn test_allow_whitespace_around_marker() {
        let dir = tempdir().unwrap();
        let rules_dir = dir.path().join(".vibewisp").join("rules");
        fs::create_dir_all(&rules_dir).unwrap();

        fs::write(
            rules_dir.join("test.md"),
            "  alwaysApply: true  \n# Rule with whitespace",
        )
        .unwrap();

        let engine = RuleEngine::new(dir.path()).unwrap();
        let rules = engine.get_active_rules();
        assert!(rules.contains("# Rule with whitespace"));
    }
}
