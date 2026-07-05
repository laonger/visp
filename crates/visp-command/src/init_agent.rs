//! `/init-agent <name>` — create a template agent definition file.
//!
//! The generated file is a Markdown file with YAML frontmatter that defines
//! a sub-agent, to be placed in `.visp/agents/`.

use std::path::{Path, PathBuf};

/// Validate an agent/skill name (alphanumeric, hyphens, underscores).
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

/// Compute the file path for an agent definition.
pub fn file_path(project_path: &Path, name: &str) -> PathBuf {
    project_path
        .join(".visp")
        .join("agents")
        .join(format!("{name}.md"))
}

/// Generate a well-documented agent template Markdown file with YAML frontmatter.
pub fn template(name: &str) -> String {
    format!(
        r#"---
name: {name}
description: A brief description of what this agent does
mode: subagent        # all | primary | subagent
model:                # optional, e.g. "Anthropic/claude-sonnet-4-20250514"
temperature: 0.1      # optional
permission: allow read_file *
permission: allow grep *
permission: allow glob *
permission: deny edit_file *
---

# Agent: {name}

Describe the agent's purpose and capabilities here.

## When to Use This Agent

<!-- Describe scenarios where this agent should be invoked. -->
<!-- Example: "Use this agent when refactoring a module." -->

- Use case 1
- Use case 2

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
        assert!(validate_name("my-agent").is_ok());
        assert!(validate_name("my_agent").is_ok());
        assert!(validate_name("agent42").is_ok());
    }

    #[test]
    fn test_validate_name_invalid() {
        assert!(validate_name("").is_err());
        assert!(validate_name("bad name").is_err());
        assert!(validate_name("name!").is_err());
    }

    #[test]
    fn test_file_path() {
        let path = file_path(Path::new("/project"), "my-agent");
        assert_eq!(path, Path::new("/project/.visp/agents/my-agent.md"));
    }

    #[test]
    fn test_template_contains_name() {
        let tpl = template("test-agent");
        assert!(tpl.contains("name: test-agent"));
        assert!(tpl.contains("# Agent: test-agent"));
    }
}
