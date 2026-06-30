use std::path::Path;

use visp_core::agent_definition::{AgentDefinition, AgentMode, PermissionAction, PermissionRule};
use visp_core::agent_registry::AgentRegistry;

/// Load agent definitions from `.visp/agents/*.md` files.
/// Falls back to a built-in "default" agent if no files exist.
pub fn load_agents(project_path: &Path) -> AgentRegistry {
    let mut registry = AgentRegistry::new();

    // Register built-in default agent (lowest priority — file-loaded can overwrite)
    let default_agent = AgentDefinition {
        name: "default".to_string(),
        description: "通用 AI 编程助手".to_string(),
        mode: AgentMode::All,
        model: None,
        temperature: None,
        steps: None,
        permission: Vec::new(),
        system_prompt: String::new(),
    };
    registry.register(default_agent).ok();

    // Register built-in code_reader subagent (for reading and understanding code)
    let code_reader = AgentDefinition {
        name: "code_reader".to_string(),
        description: "代码阅读分析子 Agent，擅长阅读、理解和解释源代码，可被 task 工具调用"
            .to_string(),
        mode: AgentMode::Subagent,
        model: None,
        temperature: None,
        steps: None,
        permission: Vec::new(),
        system_prompt: String::new(),
    };
    registry.register(code_reader).ok();

    // Register built-in explorer subagent (fast codebase search, read-only)
    let explorer = AgentDefinition {
        name: "explorer".to_string(),
        description:
            "快速代码库搜索专家。用于查找文件、定位代码模式、回答\"X 在哪里？\"等问题。"
                .to_string(),
        mode: AgentMode::Subagent,
        model: None,
        temperature: Some(0.1),
        steps: None,
        permission: vec![
            PermissionRule {
                permission: "read_file".into(),
                pattern: "*".into(),
                action: PermissionAction::Allow,
            },
            PermissionRule {
                permission: "grep".into(),
                pattern: "*".into(),
                action: PermissionAction::Allow,
            },
            PermissionRule {
                permission: "glob".into(),
                pattern: "*".into(),
                action: PermissionAction::Allow,
            },
            PermissionRule {
                permission: "fetch_web".into(),
                pattern: "*".into(),
                action: PermissionAction::Allow,
            },
            PermissionRule {
                permission: "codegraph_search".into(),
                pattern: "*".into(),
                action: PermissionAction::Allow,
            },
            PermissionRule {
                permission: "codegraph_get_details".into(),
                pattern: "*".into(),
                action: PermissionAction::Allow,
            },
            PermissionRule {
                permission: "codegraph_context".into(),
                pattern: "*".into(),
                action: PermissionAction::Allow,
            },
            PermissionRule {
                permission: "codegraph_trace".into(),
                pattern: "*".into(),
                action: PermissionAction::Allow,
            },
            PermissionRule {
                permission: "codegraph_impact".into(),
                pattern: "*".into(),
                action: PermissionAction::Allow,
            },
        ],
        system_prompt: concat!(
            "你是 Explorer —— 快速代码库导航专家。\n",
            "\n",
            "**角色**：代码库侦察兵。回答\"X 在哪里？\"\"找到 Y\"\"哪个文件有 Z\"。\n",
            "\n",
            "**工具选择**：\n",
            "- 文本/正则搜索（字符串、注释、变量名）：grep\n",
            "- 文件发现（按名称/扩展名查找）：glob\n",
            "- 结构性查询（符号定义、调用关系、影响分析）：codegraph_search、codegraph_get_details、codegraph_context、codegraph_trace、codegraph_impact\n",
            "- 读取文件内容：read_file\n",
            "\n",
            "**行为准则**：\n",
            "- 快速且彻底\n",
            "- 需要时并行发起多个搜索\n",
            "- 返回文件路径和代码片段（含行号）\n",
            "\n",
            "**输出格式**：\n",
            "<results>\n",
            "<files>\n",
            "- 文件路径:行号 — 简要说明\n",
            "</files>\n",
            "<answer>\n",
            "简洁回答\n",
            "</answer>\n",
            "</results>\n",
            "\n",
            "**约束**：\n",
            "- 只读——搜索和报告，不修改文件\n",
            "- 详尽但简洁\n",
            "- 包含行号\n",
        )
        .to_string(),
    };
    registry.register(explorer).ok();

    // Register built-in fixer subagent (fast implementation, read-write)
    let fixer = AgentDefinition {
        name: "fixer".to_string(),
        description:
            "快速实现专家。接收完整上下文和任务规格，高效执行代码变更。".to_string(),
        mode: AgentMode::Subagent,
        model: None,
        temperature: Some(0.2),
        steps: None,
        permission: vec![
            PermissionRule {
                permission: "task".into(),
                pattern: "*".into(),
                action: PermissionAction::Deny,
            },
            PermissionRule {
                permission: "*".into(),
                pattern: "*".into(),
                action: PermissionAction::Allow,
            },
        ],
        system_prompt: concat!(
            "你是 Fixer —— 快速、专注的实现专家。\n",
            "\n",
            "**角色**：高效执行代码变更。你从 Orchestrator 收到完整上下文和明确任务规格。你的工作是实现，不是规划或调研。\n",
            "\n",
            "**行为准则**：\n",
            "- 执行 Orchestrator 提供的任务规格\n",
            "- 使用提供的研究上下文（文件路径、文档、模式）\n",
            "- 使用 edit_file/write_file 之前先 read_file 读取确切内容\n",
            "- 快速直接——不做调研，不委托，不多步规划\n",
            "- 需要时编写或更新测试\n",
            "- 完成后报告变更摘要\n",
            "\n",
            "**约束**：\n",
            "- 不做外部调研（不使用 fetch_web）\n",
            "- 不委托或生成子 agent（不使用 task）\n",
            "- 不做多步研究/规划；最小执行序列即可\n",
            "- 上下文不足时：直接用 grep/glob/read_file 获取，不委托\n",
            "- 只在真正无法自行获取时才请求补充输入\n",
            "\n",
            "**输出格式**：\n",
            "<summary>\n",
            "实现内容简述\n",
            "</summary>\n",
            "<changes>\n",
            "- file1.rs: 将 X 改为 Y\n",
            "- file2.rs: 新增 Z 函数\n",
            "</changes>\n",
            "<verification>\n",
            "- 测试通过: [是/否/跳过原因]\n",
            "- 验证: [通过/失败/跳过原因]\n",
            "</verification>\n",
        )
        .to_string(),
    };
    registry.register(fixer).ok();

    // Scan .visp/agents/*.md
    let agents_dir = project_path.join(".visp/agents/");
    if !agents_dir.exists() {
        return registry;
    }

    if let Ok(dir) = std::fs::read_dir(&agents_dir) {
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

        let registry = load_agents(&dir);

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

        let registry = load_agents(&dir);
        assert!(registry.get("default").is_some());
        assert!(registry.get("code_reader").is_some());
        assert!(registry.get("explorer").is_some());
        assert!(registry.get("fixer").is_some());
        assert_eq!(registry.list().len(), 4);
    }

    #[test]
    fn test_load_agents_no_directory() {
        let dir = std::env::temp_dir().join(format!("agent_test_{}", uuid::Uuid::new_v4()));

        let registry = load_agents(&dir);
        assert!(registry.get("default").is_some());
        assert!(registry.get("code_reader").is_some());
        assert!(registry.get("explorer").is_some());
        assert!(registry.get("fixer").is_some());
        assert_eq!(registry.list().len(), 4);
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

        let registry = load_agents(&dir);
        assert!(registry.get("default").is_some());
        assert!(registry.get("code_reader").is_some());
        assert!(registry.get("valid").is_some());
        // Invalid file should not produce an entry (only built-ins + valid)
        assert_eq!(registry.list().len(), 5);
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
}
