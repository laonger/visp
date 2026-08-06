//! 内置技能定义与技能加载

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// 单个内置技能的数据结构
pub struct BuiltinSkill {
    pub name: &'static str,
    pub description: &'static str,
    pub content: &'static str,
}

/// 返回所有内置技能
pub fn builtin_skills() -> &'static [BuiltinSkill] {
    &[BuiltinSkill {
        name: "delegation-workflow",
        description: "Prefer this workflow for code exploration and implementation tasks.",
        content: concat!(
            "## When to Delegate (PREFERRED)\n",
            "\n",
            "**Prefer delegation over doing it yourself.** Delegate when the task involves:\n",
            "\n",
            "- Searching or exploring the codebase → `explorer`\n",
            "- Making code changes (edit, write, refactor, fix) → `fixer`\n",
            "\n",
            "## When NOT to Delegate\n",
            "\n",
            "- Reading a single file with a known path → use `read_file` directly\n",
            "- Running build/test/git commands → use `bash` directly\n",
            "- Quick symbol lookup → use `codegraph_*` tools directly\n",
            "\n",
            "## Delegation Workflow\n",
            "\n",
            "1. **Explore**: delegate to `explorer` to gather context\n",
            "2. **Implement**: delegate to `fixer` with the gathered context\n",
            "3. **Review**: verify sub-agent results before proceeding\n",
        ),
    }]
}

/// 按名称查找内置技能
pub fn find_builtin_skill(name: &str) -> Option<&'static BuiltinSkill> {
    builtin_skills().iter().find(|s| s.name == name)
}

/// 从 `.visp/skills/` 和全局 `~/.config/visp/skills/` 加载技能定义，格式化为 prompt 附加内容。
/// 每个技能目录下需有 `SKILL.md` 文件。
/// 项目级技能优先级高于全局级（同名时项目技能覆盖全局技能）。
pub fn load_skills(project_path: &Path) -> String {
    load_skills_inner(project_path, crate::path::home_dir())
}

/// 与 `load_skills` 相同，但允许指定 home 目录（用于测试隔离）。
fn load_skills_inner(project_path: &Path, home: Option<PathBuf>) -> String {
    let mut seen_names = HashSet::new();
    let mut sections = Vec::new();

    // 0. Built-in skills (lowest priority, can be overridden by file system)
    for skill in builtin_skills() {
        if !seen_names.insert(skill.name.to_string()) {
            continue;
        }
        let mut section = format!("### {}", skill.name);
        if !skill.description.is_empty() {
            section.push_str(&format!("\n{}", skill.description));
        }
        sections.push(section);
    }

    // 1. Project skills (higher priority)
    let project_dir = crate::path::skills_dir_project(project_path);
    load_skills_from_dir(&project_dir, &mut seen_names, &mut sections);

    // 2. Global skills (lower priority, skipped if project already has same name)
    if let Some(home) = home {
        let global_dir = home.join(".config").join("visp").join("skills");
        load_skills_from_dir(&global_dir, &mut seen_names, &mut sections);
    }

    if sections.is_empty() {
        return String::new();
    }

    format!(
        "\n\n## Available Skills\n\n\
         Use the `skill` tool to load a skill's detailed instructions.\n\n{}",
        sections.join("\n\n---\n\n")
    )
}

/// 从单个技能目录加载技能。
/// `seen_names` 跟踪已加载的技能名，同名跳过（用于项目优先级覆盖全局）。
/// `sections` 追加加载到的技能格式化片段。
fn load_skills_from_dir(dir: &Path, seen_names: &mut HashSet<String>, sections: &mut Vec<String>) {
    if !dir.is_dir() {
        return;
    }

    let mut entries: Vec<_> = match std::fs::read_dir(dir) {
        Ok(entries) => entries.filter_map(|e| e.ok()).collect(),
        Err(_) => return,
    };
    entries.sort_by_key(|e| e.file_name());

    for entry in &entries {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let skill_name = match path.file_name() {
            Some(n) => n.to_string_lossy().to_string(),
            None => continue,
        };
        // 同名跳过（项目级已加载的优先）
        if !seen_names.insert(skill_name.clone()) {
            continue;
        }
        let skill_file = path.join("SKILL.md");
        if !skill_file.is_file() {
            continue;
        }
        let content = match std::fs::read_to_string(&skill_file) {
            Ok(c) => c,
            Err(_) => continue,
        };
        // 提取 YAML frontmatter 中的 description（如果有）
        let description = extract_frontmatter_field(&content, "description");

        let mut section = format!("### {skill_name}");
        if let Some(desc) = description {
            section.push_str(&format!("\n{desc}"));
        }
        sections.push(section);
    }
}

/// 从 YAML frontmatter 中提取指定字段值
fn extract_frontmatter_field(content: &str, field: &str) -> Option<String> {
    let content = content.trim();
    if !content.starts_with("---") {
        return None;
    }
    let rest = content.strip_prefix("---")?;
    let end = rest.find("\n---")?;
    let frontmatter = &rest[..end];
    let prefix = format!("{field}:");
    for line in frontmatter.lines() {
        let trimmed = line.trim();
        if let Some(val) = trimmed.strip_prefix(&prefix) {
            return Some(val.trim().to_string());
        }
    }
    None
}

/// 去除 YAML frontmatter，返回正文
pub fn strip_frontmatter(content: &str) -> &str {
    let content = content.trim();
    if !content.starts_with("---") {
        return content;
    }
    let rest = content.strip_prefix("---").unwrap();
    if let Some(end) = rest.find("\n---") {
        let after = &rest[end + 4..]; // skip \n + ---
        after.trim()
    } else {
        content
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_builtin_skills_non_empty() {
        let skills = builtin_skills();
        assert!(!skills.is_empty());
    }

    #[test]
    fn test_builtin_skills_have_name_and_content() {
        for skill in builtin_skills() {
            assert!(!skill.name.is_empty());
            assert!(!skill.content.is_empty());
        }
    }

    #[test]
    fn test_find_builtin_skill_exists() {
        let skill = find_builtin_skill("delegation-workflow");
        assert!(skill.is_some());
        assert_eq!(skill.unwrap().name, "delegation-workflow");
    }

    #[test]
    fn test_find_builtin_skill_not_found() {
        assert!(find_builtin_skill("nonexistent-skill").is_none());
    }

    #[test]
    fn test_builtin_skill_content_contains_workflow() {
        let skill = find_builtin_skill("delegation-workflow").unwrap();
        assert!(skill.content.contains("PREFERRED"));
        assert!(skill.content.contains("Delegation Workflow"));
    }

    #[test]
    fn test_extract_frontmatter_field_found() {
        let content = "---\nname: test-skill\ndescription: A test skill\n---\n\nContent here";
        assert_eq!(
            extract_frontmatter_field(content, "description"),
            Some("A test skill".into())
        );
    }

    #[test]
    fn test_extract_frontmatter_field_missing() {
        let content = "---\nname: test\n---\n\nContent";
        assert_eq!(extract_frontmatter_field(content, "description"), None);
    }

    #[test]
    fn test_extract_frontmatter_field_no_frontmatter() {
        let content = "Just content";
        assert_eq!(extract_frontmatter_field(content, "name"), None);
    }

    #[test]
    fn test_strip_frontmatter_removes_yaml() {
        let content = "---\nname: test\n---\nBody text";
        assert_eq!(strip_frontmatter(content), "Body text");
    }

    #[test]
    fn test_strip_frontmatter_no_frontmatter() {
        let content = "Just body";
        assert_eq!(strip_frontmatter(content), "Just body");
    }

    #[test]
    fn test_load_skills_empty_dir_still_has_builtins() {
        let tmp = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        // No skills dir → only built-in skills
        let result = load_skills_inner(tmp.path(), Some(home.path().to_path_buf()));
        assert!(result.contains("delegation-workflow"));
        assert!(result.contains("Available Skills"));
    }

    #[test]
    fn test_load_skills_with_skill_file() {
        use std::io::Write;
        let tmp = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        let skills_dir = tmp.path().join(".visp").join("skills").join("my-skill");
        std::fs::create_dir_all(&skills_dir).unwrap();
        let mut f = std::fs::File::create(skills_dir.join("SKILL.md")).unwrap();
        f.write_all(
            b"---\nname: my-skill\ndescription: A custom skill\n---\n\nDo something useful.\n",
        )
        .unwrap();

        let result = load_skills_inner(tmp.path(), Some(home.path().to_path_buf()));
        assert!(result.contains("my-skill"));
        assert!(result.contains("A custom skill"));
        assert!(!result.contains("Do something useful.")); // body 不应包含在提示词中
        assert!(result.contains("Available Skills"));
    }

    #[test]
    fn test_load_skills_ignores_non_skill_dirs() {
        use std::io::Write;
        let tmp = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        let skills_dir = tmp.path().join(".visp").join("skills").join("my-skill");
        std::fs::create_dir_all(&skills_dir).unwrap();
        let mut f = std::fs::File::create(skills_dir.join("SKILL.md")).unwrap();
        f.write_all(b"---\nname: my-skill\n---\n\nContent").unwrap();
        // Add a non-skill file/dir
        std::fs::create_dir_all(tmp.path().join(".visp").join("skills").join("not-a-skill"))
            .unwrap();

        let result = load_skills_inner(tmp.path(), Some(home.path().to_path_buf()));
        assert!(result.contains("my-skill"));
    }

    #[test]
    fn test_load_skills_global_skills_loaded() {
        use std::io::Write;
        let tmp = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();

        // Create global skill at ~/.config/visp/skills/global-tool/
        let global_skill_dir = home
            .path()
            .join(".config")
            .join("visp")
            .join("skills")
            .join("global-tool");
        std::fs::create_dir_all(&global_skill_dir).unwrap();
        let mut f = std::fs::File::create(global_skill_dir.join("SKILL.md")).unwrap();
        f.write_all(b"---\ndescription: A global skill\n---\n\nDo stuff.\n")
            .unwrap();

        let result = load_skills_inner(tmp.path(), Some(home.path().to_path_buf()));
        assert!(
            result.contains("global-tool"),
            "should contain global skill"
        );
        assert!(
            result.contains("A global skill"),
            "should contain global skill description"
        );
        assert!(result.contains("Available Skills"));
    }

    #[test]
    fn test_load_skills_project_overrides_global() {
        use std::io::Write;
        let tmp = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();

        // Create project skill
        let project_skill = tmp.path().join(".visp").join("skills").join("my-tool");
        std::fs::create_dir_all(&project_skill).unwrap();
        let mut f = std::fs::File::create(project_skill.join("SKILL.md")).unwrap();
        f.write_all(b"---\ndescription: Project version\n---\n\nProject content.\n")
            .unwrap();

        // Create global skill with same name (should be overridden)
        let global_skill = home
            .path()
            .join(".config")
            .join("visp")
            .join("skills")
            .join("my-tool");
        std::fs::create_dir_all(&global_skill).unwrap();
        let mut f = std::fs::File::create(global_skill.join("SKILL.md")).unwrap();
        f.write_all(b"---\ndescription: Global version\n---\n\nGlobal content.\n")
            .unwrap();

        let result = load_skills_inner(tmp.path(), Some(home.path().to_path_buf()));
        assert!(
            result.contains("Project version"),
            "should use project version description"
        );
        assert!(
            !result.contains("Global version"),
            "should NOT contain global version description"
        );
        assert!(result.contains("my-tool"), "should contain the skill name");
    }

    #[test]
    fn test_load_skills_both_project_and_global() {
        use std::io::Write;
        let tmp = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();

        // Create project skill (unique name)
        let project_skill = tmp.path().join(".visp").join("skills").join("proj-skill");
        std::fs::create_dir_all(&project_skill).unwrap();
        let mut f = std::fs::File::create(project_skill.join("SKILL.md")).unwrap();
        f.write_all(b"---\ndescription: Project only\n---\n\nContent.\n")
            .unwrap();

        // Create global skill (unique name)
        let global_skill = home
            .path()
            .join(".config")
            .join("visp")
            .join("skills")
            .join("glob-skill");
        std::fs::create_dir_all(&global_skill).unwrap();
        let mut f = std::fs::File::create(global_skill.join("SKILL.md")).unwrap();
        f.write_all(b"---\ndescription: Global only\n---\n\nContent.\n")
            .unwrap();

        let result = load_skills_inner(tmp.path(), Some(home.path().to_path_buf()));
        assert!(
            result.contains("proj-skill"),
            "should contain project skill"
        );
        assert!(result.contains("glob-skill"), "should contain global skill");
        assert!(result.contains("Project only"));
        assert!(result.contains("Global only"));
    }
}
