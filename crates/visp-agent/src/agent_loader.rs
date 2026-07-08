use std::path::Path;

use visp_core::agent_definition::{AgentDefinition, AgentMode, PermissionAction, PermissionRule};
use visp_core::agent_registry::AgentRegistry;

use crate::builtin_agents::register_builtin_agents;

/// 配置文件中对内置 agent 的覆盖项。
///
/// 允许在不修改源码的情况下，通过 daemon.toml 的 `[[agent.builtin]]`
/// 覆盖内置 agent 的 model / temperature / steps 等字段。
#[derive(Debug, Clone, Default)]
pub struct BuiltinAgentOverride {
    pub name: String,
    pub model: Option<String>,
    pub temperature: Option<f32>,
    pub steps: Option<u32>,
}

/// Load agent definitions from agent directories.
/// Falls back to built-in agents if no files exist.
///
/// `agent_dirs` 按优先级从低到高排列，后面的目录会覆盖前面的同名 agent。
/// 典型顺序：全局配置目录（`~/.config/visp/agents/`）→ 项目目录（`.visp/agents/`）。
///
/// `overrides` 来自 daemon.toml 的 `[[agent.builtin]]` 配置，
/// 在内置 agent 注册之后、文件 agent 加载之前应用。
pub fn load_agents(agent_dirs: &[&Path], overrides: &[BuiltinAgentOverride]) -> AgentRegistry {
    let mut registry = AgentRegistry::new();

    // Register built-in agents (lowest priority — file-loaded can overwrite)
    register_builtin_agents(&mut registry);

    // Apply config overrides (from daemon.toml [[agent.builtin]])
    for ov in overrides {
        if let Some(agent) = registry.get_mut(&ov.name) {
            if let Some(model) = &ov.model {
                agent.model = Some(model.clone());
            }
            if let Some(temp) = ov.temperature {
                agent.temperature = Some(temp);
            }
            if let Some(steps) = ov.steps {
                agent.steps = Some(steps);
            }
            tracing::debug!(
                agent = %ov.name,
                "applied config override for built-in agent"
            );
        } else {
            tracing::warn!(
                agent = %ov.name,
                "config override references unknown built-in agent, ignored"
            );
        }
    }

    // Scan agent directories (each later dir overrides earlier ones)
    for agents_dir in agent_dirs {
        if !agents_dir.exists() {
            continue;
        }
        if let Ok(dir) = std::fs::read_dir(agents_dir) {
            for entry in dir.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("md") && path.is_file() {
                    match parse_agent_file(&path) {
                        Ok(def) => {
                            // Use register_or_replace so file agents overwrite built-in defaults
                            registry.register_or_replace(def);
                        }
                        Err(e) => {
                            tracing::warn!(
                                path = %path.display(),
                                error = %e,
                                "skip invalid agent file"
                            );
                        }
                    }
                }
            }
        }
    }

    registry
}

/// Parse a single agent `.md` file into an `AgentDefinition`.
///
/// Expected format:
/// ```markdown
/// ---
/// name: my-agent
/// description: My custom agent
/// mode: all        # all, primary, subagent
/// model: gpt-4o    # optional
/// temperature: 0.7 # optional
/// steps: 50        # optional
/// ---
///
/// System prompt content (optional)
/// ```
fn parse_agent_file(path: &Path) -> Result<AgentDefinition, String> {
    let content = std::fs::read_to_string(path).map_err(|e| format!("read error: {e}"))?;

    // Strip BOM if present
    let content = content.strip_prefix('\u{feff}').unwrap_or(&content);

    // Extract YAML frontmatter (between --- markers)
    let content = content.trim();
    if !content.starts_with("---") {
        return Err("missing YAML frontmatter (start with ---)".to_string());
    }
    let content = &content[3..].trim_start();

    let end = content
        .find("\n---")
        .ok_or_else(|| "missing closing --- in YAML frontmatter".to_string())?;

    let yaml_text = &content[..end];
    let body = content[end + 4..].trim().to_string();

    // Parse YAML line by line (no serde_yaml dependency)
    let mut name: Option<String> = None;
    let mut description: Option<String> = None;
    let mut mode: Option<String> = None;
    let mut model: Option<String> = None;
    let mut temperature: Option<f32> = None;
    let mut steps: Option<u32> = None;
    let mut permission: Vec<PermissionRule> = Vec::new();

    for line in yaml_text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim().to_lowercase();
            let value = value.trim().trim_matches('"').trim();
            match key.as_str() {
                "name" => name = Some(value.to_string()),
                "description" => description = Some(value.to_string()),
                "mode" => mode = Some(value.to_string()),
                "model" => {
                    if !value.is_empty() {
                        model = Some(value.to_string());
                    }
                }
                "temperature" => {
                    temperature = value.parse::<f32>().ok();
                }
                "steps" => {
                    steps = value.parse::<u32>().ok();
                }
                "permission" => {
                    // Format: <action> <tool> [pattern]
                    // Example: "deny edit_file *", "allow read_file *", "deny bash"
                    let parts: Vec<&str> = value.split_whitespace().collect();
                    if parts.len() >= 2 {
                        let action = match parts[0] {
                            "allow" => PermissionAction::Allow,
                            "deny" => PermissionAction::Deny,
                            _ => continue,
                        };
                        let tool = parts[1].to_string();
                        let pattern = parts
                            .get(2)
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| "*".to_string());
                        permission.push(PermissionRule {
                            permission: tool,
                            pattern,
                            action,
                        });
                    }
                }
                _ => {}
            }
        }
    }

    let name = name.ok_or_else(|| "missing 'name' in frontmatter".to_string())?;
    let mode = match mode.as_deref() {
        Some("all") => AgentMode::All,
        Some("primary") => AgentMode::Primary,
        Some("subagent") => AgentMode::Subagent,
        None | Some("") => AgentMode::All,
        Some(other) => {
            return Err(format!(
                "invalid mode '{other}'; expected all|primary|subagent"
            ));
        }
    };

    Ok(AgentDefinition {
        name,
        description: description.unwrap_or_default(),
        mode,
        model,
        temperature,
        steps,
        permission,
        system_prompt: body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_agent_file(dir: &std::path::Path, name: &str, content: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn test_parse_valid_agent_file() {
        let dir = std::env::temp_dir().join(format!("agent_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        let content = r#"---
name: my-agent
description: My custom agent
mode: all
model: gpt-4o
temperature: 0.7
steps: 50
---

This is the system prompt.
"#;
        let path = write_agent_file(&dir, "test.md", content);
        let def = parse_agent_file(&path).unwrap();

        assert_eq!(def.name, "my-agent");
        assert_eq!(def.description, "My custom agent");
        assert_eq!(def.mode, AgentMode::All);
        assert_eq!(def.model, Some("gpt-4o".to_string()));
        assert_eq!(def.temperature, Some(0.7));
        assert_eq!(def.steps, Some(50));
        assert_eq!(def.system_prompt, "This is the system prompt.");
    }

    #[test]
    fn test_parse_minimal_agent_file() {
        let dir = std::env::temp_dir().join(format!("agent_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        let content = r#"---
name: minimal
mode: subagent
---
"#;
        let path = write_agent_file(&dir, "minimal.md", content);
        let def = parse_agent_file(&path).unwrap();

        assert_eq!(def.name, "minimal");
        assert_eq!(def.mode, AgentMode::Subagent);
        assert_eq!(def.description, "");
        assert_eq!(def.model, None);
        assert_eq!(def.temperature, None);
        assert_eq!(def.steps, None);
        assert_eq!(def.system_prompt, "");
    }

    #[test]
    fn test_parse_missing_name() {
        let dir = std::env::temp_dir().join(format!("agent_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        let content = r#"---
description: no name
mode: all
---
"#;
        let path = write_agent_file(&dir, "noname.md", content);
        let result = parse_agent_file(&path);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("missing 'name'"));
    }

    #[test]
    fn test_parse_invalid_mode() {
        let dir = std::env::temp_dir().join(format!("agent_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        let content = r#"---
name: bad
mode: invalid_mode
---
"#;
        let path = write_agent_file(&dir, "bad.md", content);
        let result = parse_agent_file(&path);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("invalid mode"));
    }

    #[test]
    fn test_load_agents_from_directory() {
        let dir = std::env::temp_dir().join(format!("agent_test_{}", uuid::Uuid::new_v4()));
        let agents_dir = dir.join(".visp/agents/");
        std::fs::create_dir_all(&agents_dir).unwrap();

        // Write two agent files
        write_agent_file(
            &agents_dir,
            "coder.md",
            r#"---
name: coder
mode: all
---
You are a coder.
"#,
        );
        write_agent_file(
            &agents_dir,
            "searcher.md",
            r#"---
name: searcher
mode: subagent
---
You search.
"#,
        );

        let registry = load_agents(&[dir.join(".visp/agents/").as_path()], &[]);

        // Default agent should exist
        assert!(registry.get("default").is_some());

        // File-loaded agents should exist
        assert!(registry.get("coder").is_some());
        assert!(registry.get("searcher").is_some());

        // Verify modes
        assert_eq!(registry.get("coder").unwrap().mode, AgentMode::All);
        assert_eq!(registry.get("searcher").unwrap().mode, AgentMode::Subagent);
    }

    #[test]
    fn test_load_agents_empty_directory() {
        let dir = std::env::temp_dir().join(format!("agent_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join(".visp/agents/")).unwrap();

        let registry = load_agents(&[dir.join(".visp/agents/").as_path()], &[]);
        assert!(registry.get("default").is_some());
        assert!(registry.get("explorer").is_some());
        assert!(registry.get("fixer").is_some());
        assert_eq!(registry.list().len(), 3);
    }

    #[test]
    fn test_load_agents_no_directory() {
        let _dir = std::env::temp_dir().join(format!("agent_test_{}", uuid::Uuid::new_v4()));

        let registry = load_agents(&[], &[]);
        assert!(registry.get("default").is_some());
        assert!(registry.get("explorer").is_some());
        assert!(registry.get("fixer").is_some());
        assert_eq!(registry.list().len(), 3);
    }

    #[test]
    fn test_load_agents_skips_invalid_files() {
        let dir = std::env::temp_dir().join(format!("agent_test_{}", uuid::Uuid::new_v4()));
        let agents_dir = dir.join(".visp/agents/");
        std::fs::create_dir_all(&agents_dir).unwrap();

        // Valid file
        write_agent_file(
            &agents_dir,
            "valid.md",
            r#"---
name: valid
mode: all
---"#,
        );
        // Invalid file (no frontmatter)
        write_agent_file(&agents_dir, "invalid.md", "Just plain text content.");

        let registry = load_agents(&[dir.join(".visp/agents/").as_path()], &[]);
        assert!(registry.get("default").is_some());
        assert!(registry.get("valid").is_some());
        // Invalid file should not produce an entry (only built-ins + valid)
        assert_eq!(registry.list().len(), 4);
    }

    #[test]
    fn test_parse_agent_with_permission_rules() {
        let dir = std::env::temp_dir().join(format!("agent_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        let content = r#"---
name: explorer
mode: subagent
permission: deny edit_file *
permission: allow read_file *
---

You are an explorer.
"#;
        let path = write_agent_file(&dir, "explorer.md", content);
        let def = parse_agent_file(&path).unwrap();

        assert_eq!(def.name, "explorer");
        assert_eq!(def.mode, AgentMode::Subagent);
        assert_eq!(def.permission.len(), 2);
        assert_eq!(def.permission[0].permission, "edit_file");
        assert_eq!(def.permission[0].pattern, "*");
        assert_eq!(def.permission[0].action, PermissionAction::Deny);
        assert_eq!(def.permission[1].permission, "read_file");
        assert_eq!(def.permission[1].pattern, "*");
        assert_eq!(def.permission[1].action, PermissionAction::Allow);
    }

    #[test]
    fn test_parse_agent_permission_defaults_pattern_to_star() {
        let dir = std::env::temp_dir().join(format!("agent_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        let content = r#"---
name: test
mode: subagent
permission: deny bash
---
"#;
        let path = write_agent_file(&dir, "test.md", content);
        let def = parse_agent_file(&path).unwrap();

        assert_eq!(def.permission.len(), 1);
        assert_eq!(def.permission[0].permission, "bash");
        assert_eq!(def.permission[0].pattern, "*");
        assert_eq!(def.permission[0].action, PermissionAction::Deny);
    }

    #[test]
    fn test_parse_agent_invalid_permission_action_skipped() {
        let dir = std::env::temp_dir().join(format!("agent_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        let content = r#"---
name: test
mode: subagent
permission: unknown_action tool *
permission: allow grep *
---
"#;
        let path = write_agent_file(&dir, "test.md", content);
        let def = parse_agent_file(&path).unwrap();

        // First line skipped (unknown action), second line added
        assert_eq!(def.permission.len(), 1);
        assert_eq!(def.permission[0].permission, "grep");
        assert_eq!(def.permission[0].action, PermissionAction::Allow);
    }

    #[test]
    fn test_load_agents_with_config_overrides() {
        let dir = std::env::temp_dir().join(format!("agent_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        let overrides = vec![
            BuiltinAgentOverride {
                name: "explorer".to_string(),
                model: Some("Opencode/deepseek-v4-flash".to_string()),
                temperature: Some(0.05),
                steps: None,
            },
            BuiltinAgentOverride {
                name: "fixer".to_string(),
                model: Some("Anthropic/claude-sonnet-4-20250514".to_string()),
                temperature: None,
                steps: Some(30),
            },
        ];

        let registry = load_agents(&[], &overrides);

        // explorer override applied
        let explorer = registry.get("explorer").unwrap();
        assert_eq!(
            explorer.model.as_deref(),
            Some("Opencode/deepseek-v4-flash")
        );
        assert!((explorer.temperature.unwrap() - 0.05).abs() < f32::EPSILON);

        // fixer override applied
        let fixer = registry.get("fixer").unwrap();
        assert_eq!(
            fixer.model.as_deref(),
            Some("Anthropic/claude-sonnet-4-20250514")
        );
        assert_eq!(fixer.steps, Some(30));
    }

    #[test]
    fn test_load_agents_override_unknown_agent_ignored() {
        let dir = std::env::temp_dir().join(format!("agent_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        let overrides = vec![BuiltinAgentOverride {
            name: "nonexistent".to_string(),
            model: Some("gpt-4o".to_string()),
            temperature: None,
            steps: None,
        }];

        let registry = load_agents(&[], &overrides);
        // Unknown agent should not be registered
        assert!(registry.get("nonexistent").is_none());
        // Built-ins still present
        assert!(registry.get("default").is_some());
        assert_eq!(registry.list().len(), 3);
    }

    #[test]
    fn test_load_agents_multi_dir_priority() {
        // Global dir (lower priority)
        let global_dir =
            std::env::temp_dir().join(format!("agent_global_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&global_dir).unwrap();

        write_agent_file(
            &global_dir,
            "shared.md",
            r#"---
name: shared
mode: all
---
Global prompt.
"#,
        );
        write_agent_file(
            &global_dir,
            "global_only.md",
            r#"---
name: global_only
mode: all
---
Only in global.
"#,
        );

        // Project dir (higher priority)
        let project_dir =
            std::env::temp_dir().join(format!("agent_project_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&project_dir).unwrap();

        write_agent_file(
            &project_dir,
            "shared.md",
            r#"---
name: shared
mode: all
---
Project prompt (overrides global).
"#,
        );
        write_agent_file(
            &project_dir,
            "project_only.md",
            r#"---
name: project_only
mode: all
---
Only in project.
"#,
        );

        let registry = load_agents(&[global_dir.as_path(), project_dir.as_path()], &[]);

        // Both global-only and project-only agents should be present
        assert!(registry.get("global_only").is_some());
        assert!(registry.get("project_only").is_some());

        // "shared" should be overridden by project dir (higher priority)
        let shared = registry.get("shared").unwrap();
        assert!(shared.system_prompt.contains("Project prompt"));
        assert!(!shared.system_prompt.contains("Global prompt"));
    }
}
