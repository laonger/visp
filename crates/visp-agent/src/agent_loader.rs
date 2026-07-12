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
                            // For existing agents (e.g. built-in), do field-level merge to
                            // preserve [[agent.builtin]] overrides (model/temperature/steps)
                            // when the .md file doesn't explicitly specify them.
                            if let Some(existing) = registry.get_mut(&def.name) {
                                // Always overwrite: .md is the source of truth for these
                                existing.description = def.description;
                                existing.mode = def.mode;
                                existing.permission = def.permission;
                                existing.system_prompt = def.system_prompt;

                                // Only overwrite if .md explicitly provides them:
                                // preserves [[agent.builtin]] overrides when absent.
                                if let Some(model) = def.model {
                                    existing.model = Some(model);
                                }
                                if let Some(temp) = def.temperature {
                                    existing.temperature = Some(temp);
                                }
                                if let Some(steps) = def.steps {
                                    existing.steps = Some(steps);
                                }
                            } else {
                                // New custom agent — register directly
                                registry.register_or_replace(def);
                            }
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
        allowed_sub_agents: Vec::new(),
        system_prompt: body,
    })
}

#[cfg(test)]
#[path = "agent_loader_tests.rs"]
mod tests;
