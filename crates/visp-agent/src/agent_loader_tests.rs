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
    let global_dir = std::env::temp_dir().join(format!("agent_global_{}", uuid::Uuid::new_v4()));
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
    let project_dir = std::env::temp_dir().join(format!("agent_project_{}", uuid::Uuid::new_v4()));
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

#[test]
fn test_file_agent_merges_model_with_builtin_override() {
    let dir = std::env::temp_dir().join(format!("agent_test_{}", uuid::Uuid::new_v4()));
    let agents_dir = dir.join(".visp/agents/");
    std::fs::create_dir_all(&agents_dir).unwrap();

    // Write explorer.md WITHOUT model field — tests that model from
    // [[agent.builtin]] override is preserved during field-level merge
    write_agent_file(
        &agents_dir,
        "explorer.md",
        r#"---
name: explorer
description: Custom explorer
mode: subagent
temperature: 0.7
permission: deny edit_file *
permission: allow read_file *
---

You are a custom explorer.
"#,
    );

    let overrides = vec![BuiltinAgentOverride {
        name: "explorer".to_string(),
        model: Some("Opencode/deepseek-v4-flash".to_string()),
        temperature: Some(0.05),
        steps: None,
    }];

    let registry = load_agents(&[agents_dir.as_path()], &overrides);

    let explorer = registry.get("explorer").unwrap();

    // model should be preserved from override (.md has no model field)
    assert_eq!(
        explorer.model.as_deref(),
        Some("Opencode/deepseek-v4-flash")
    );

    // temperature should come from .md (explicitly set, overrides override)
    assert!((explorer.temperature.unwrap() - 0.7).abs() < f32::EPSILON);

    // description should come from .md
    assert_eq!(explorer.description, "Custom explorer");

    // system_prompt should come from .md body
    assert!(
        explorer
            .system_prompt
            .contains("You are a custom explorer.")
    );

    // permission should come from .md
    assert_eq!(explorer.permission.len(), 2);
    assert_eq!(explorer.permission[0].permission, "edit_file");
    assert_eq!(explorer.permission[0].action, PermissionAction::Deny);
    assert_eq!(explorer.permission[1].permission, "read_file");
    assert_eq!(explorer.permission[1].action, PermissionAction::Allow);
}

#[test]
fn test_file_agent_model_overrides_builtin_when_specified() {
    let dir = std::env::temp_dir().join(format!("agent_test_{}", uuid::Uuid::new_v4()));
    let agents_dir = dir.join(".visp/agents/");
    std::fs::create_dir_all(&agents_dir).unwrap();

    // Write explorer.md WITH explicit model field — tests that .md can
    // still override the [[agent.builtin]] model when explicitly specified
    write_agent_file(
        &agents_dir,
        "explorer.md",
        r#"---
name: explorer
description: Custom explorer with explicit model
mode: subagent
model: Opencode/deepseek-v3
temperature: 0.3
---

You are a custom explorer with explicit model.
"#,
    );

    let overrides = vec![BuiltinAgentOverride {
        name: "explorer".to_string(),
        model: Some("Opencode/deepseek-v4-flash".to_string()),
        temperature: None,
        steps: None,
    }];

    let registry = load_agents(&[agents_dir.as_path()], &overrides);

    let explorer = registry.get("explorer").unwrap();

    // model should come from .md (explicitly specified, overrides override)
    assert_eq!(explorer.model.as_deref(), Some("Opencode/deepseek-v3"));

    // temperature should come from .md
    assert!((explorer.temperature.unwrap() - 0.3).abs() < f32::EPSILON);

    // system_prompt should come from .md body
    assert!(
        explorer
            .system_prompt
            .contains("You are a custom explorer with explicit model.")
    );
}
