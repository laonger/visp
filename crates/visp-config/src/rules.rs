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

        // 1. AGENTS.md from project directory upward to root (closest first)
        for md in discover_agents_md(project_path) {
            if let Ok(content) = std::fs::read_to_string(&md) {
                let header = format!("Instructions from: {}", md.display());
                files.push(RuleFile {
                    path: md,
                    content: format!("{header}\n{content}"),
                });
            }
        }

        // 2. Global AGENTS.md: ~/.config/visp/AGENTS.md
        if let Some(global_agents) = crate::path::global_agents_md()
            && global_agents.is_file()
            && let Ok(content) = std::fs::read_to_string(&global_agents)
        {
            let header = format!("Instructions from: {}", global_agents.display());
            files.push(RuleFile {
                path: global_agents,
                content: format!("{header}\n{content}"),
            });
        }

        // 3. Project rules: .visp/rules/
        let project_rules = crate::path::rules_dir_project(project_path);
        if project_rules.is_dir() {
            collect_rules(&project_rules, &mut files)?;
        }

        // 4. Global rules: ~/.config/visp/rules/
        if let Some(global_rules) = crate::path::rules_dir_global()
            && global_rules.is_dir()
        {
            collect_rules(&global_rules, &mut files)?;
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

/// 从 project_path 向上遍历到根目录，寻找所有 AGENTS.md 文件。
/// 返回结果按距离 project_path 从近到远排序。
fn discover_agents_md(project_path: &Path) -> Vec<PathBuf> {
    let mut result = Vec::new();
    let mut current = Some(project_path.to_path_buf());

    while let Some(path) = current {
        let agents = path.join("AGENTS.md");
        if agents.is_file() {
            result.push(agents);
        }
        // Walk up to parent
        current = path.parent().map(|p| p.to_path_buf());
        // Stop at filesystem root
        if path == path.parent().unwrap_or(&path) {
            break;
        }
    }

    result
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
        let rules_dir = dir.path().join(".visp").join("rules");
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
        let rules_dir = dir.path().join(".visp").join("rules");
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
        let rules_dir = dir.path().join(".visp").join("rules");
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
        let rules_dir = dir.path().join(".visp").join("rules");
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
        let rules_dir = dir.path().join(".visp").join("rules");
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
        let rules_dir = dir.path().join(".visp").join("rules");
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
        let rules_dir = dir.path().join(".visp").join("rules");
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

    #[test]
    fn test_loads_project_agents_md() {
        let dir = tempdir().unwrap();
        let agents_path = dir.path().join("AGENTS.md");
        fs::write(
            &agents_path,
            "<Role>\nYou are a Rust coding assistant.\n</Role>",
        )
        .unwrap();

        let engine = RuleEngine::new(dir.path()).unwrap();
        let rules = engine.get_active_rules();
        assert!(rules.contains("Instructions from:"));
        assert!(rules.contains("Rust coding assistant"));
    }

    #[test]
    fn test_missing_agents_md_no_error() {
        let dir = tempdir().unwrap();
        // No AGENTS.md file exists
        let engine = RuleEngine::new(dir.path()).unwrap();
        // Should contain nothing (no rules dirs either)
        assert!(engine.get_active_rules().is_empty());
    }

    #[test]
    fn test_agents_md_before_rules_order() {
        let dir = tempdir().unwrap();
        // Create AGENTS.md
        fs::write(dir.path().join("AGENTS.md"), "<Role>Agent role</Role>").unwrap();
        // Create .visp/rules/ with a rule
        let rules_dir = dir.path().join(".visp").join("rules");
        fs::create_dir_all(&rules_dir).unwrap();
        fs::write(
            rules_dir.join("test.md"),
            "alwaysApply: true\n# Custom rule",
        )
        .unwrap();

        let engine = RuleEngine::new(dir.path()).unwrap();
        let rules = engine.get_active_rules();
        // AGENTS.md should come first (higher priority)
        let agents_pos = rules.find("Agent role").unwrap();
        let rule_pos = rules.find("Custom rule").unwrap();
        assert!(agents_pos < rule_pos);
    }

    #[test]
    fn test_discover_ancestor_agents_md() {
        // Simulate: project/subdir/ with AGENTS.md in project/
        let tmp = tempdir().unwrap();
        let project = tmp.path().join("project");
        let subdir = project.join("subdir");
        fs::create_dir_all(&subdir).unwrap();

        // AGENTS.md in parent directory (project root)
        fs::write(project.join("AGENTS.md"), "Ancestor instructions").unwrap();

        // RuleEngine created from subdir should discover ancestor AGENTS.md
        let engine = RuleEngine::new(&subdir).unwrap();
        let rules = engine.get_active_rules();
        assert!(rules.contains("Ancestor instructions"));
    }

    #[test]
    fn test_agents_md_closest_highest_priority() {
        // Simulate: project/AGENTS.md and project/subdir/AGENTS.md
        let tmp = tempdir().unwrap();
        let subdir = tmp.path().join("subdir");
        fs::create_dir_all(&subdir).unwrap();

        fs::write(tmp.path().join("AGENTS.md"), "Root instructions").unwrap();
        fs::write(subdir.join("AGENTS.md"), "Subdir instructions").unwrap();

        // RuleEngine created from subdir should have both, subdir first
        let engine = RuleEngine::new(&subdir).unwrap();
        let rules = engine.get_active_rules();
        assert!(rules.contains("Root instructions"));
        assert!(rules.contains("Subdir instructions"));

        // Subdir instructions should come first (closer)
        let sub_pos = rules.find("Subdir instructions").unwrap();
        let root_pos = rules.find("Root instructions").unwrap();
        assert!(sub_pos < root_pos);
    }
}
