//! `/init-skill <name>` — create a template skill file.
//!
//! The generated file is a Markdown file with YAML frontmatter that defines
//! a skill, to be placed in `.visp/skills/<name>/SKILL.md`.

use std::path::{Path, PathBuf};

/// Validate a skill name (reuses the same rules as agent names).
pub fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Name cannot be empty".into());
    }
    if !name
        .chars()
        .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        return Err(
            "Name can only contain alphanumeric characters, hyphens, and underscores".into(),
        );
    }
    Ok(())
}

/// Compute the file path for a skill definition.
pub fn file_path(project_path: &Path, name: &str) -> PathBuf {
    project_path
        .join(".visp")
        .join("skills")
        .join(name)
        .join("SKILL.md")
}

/// Compute the parent directory for a skill (needs to be created before writing).
pub fn parent_dir(project_path: &Path, name: &str) -> PathBuf {
    project_path.join(".visp").join("skills").join(name)
}

/// Generate a well-documented skill template Markdown file with YAML frontmatter.
pub fn template(name: &str) -> String {
    format!(
        r#"---
name: {name}
description: A brief description of what this skill does and when to use it
---

# Skill: {name}

## When to Use This Skill

<!-- Describe the scenarios where this skill should be activated. -->
<!-- Example: "Use this skill when the user asks to refactor a module." -->

- Trigger condition 1
- Trigger condition 2

## When NOT to Use

- Scenario where this skill is unnecessary
- Scenario where a different approach is better

## Workflow

1. **Step 1**: Describe the first action
2. **Step 2**: Describe the next action
3. **Step 3**: Finalize and report

## Guidelines

- Guideline 1
- Guideline 2

## Constraints

- Constraint 1
- Constraint 2
"#,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_name_ok() {
        assert!(validate_name("my-skill").is_ok());
        assert!(validate_name("code-review").is_ok());
    }

    #[test]
    fn test_validate_name_invalid() {
        assert!(validate_name("").is_err());
        assert!(validate_name("bad skill").is_err());
    }

    #[test]
    fn test_file_path() {
        let path = file_path(Path::new("/project"), "my-skill");
        assert_eq!(path, Path::new("/project/.visp/skills/my-skill/SKILL.md"));
    }

    #[test]
    fn test_parent_dir() {
        let dir = parent_dir(Path::new("/project"), "my-skill");
        assert_eq!(dir, Path::new("/project/.visp/skills/my-skill"));
    }

    #[test]
    fn test_template_contains_name() {
        let tpl = template("test-skill");
        assert!(tpl.contains("name: test-skill"));
        assert!(tpl.contains("# Skill: test-skill"));
    }
}
