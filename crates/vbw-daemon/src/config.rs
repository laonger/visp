#![allow(dead_code)]

use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
pub struct DaemonConfig {
    pub daemon: DaemonSection,
    pub llm: LlmSection,
    pub tools: ToolsSection,
    pub agent: AgentSection,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DaemonSection {
    #[serde(default = "default_listen_addr")]
    pub listen_addr: String,
    #[serde(default = "default_log_level")]
    pub log_level: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LlmSection {
    #[serde(default = "default_provider")]
    pub provider: String,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_temperature")]
    pub temperature: f64,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ToolsSection {
    #[serde(default = "default_bash_timeout")]
    pub bash_timeout_secs: u64,
    #[serde(default = "default_file_max_size")]
    pub file_max_size_bytes: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentSection {
    #[serde(default = "default_max_iterations")]
    pub max_iterations: u32,
    #[serde(default = "default_retry_attempts")]
    pub llm_retry_attempts: u32,
    #[serde(default = "default_retry_delay")]
    pub llm_retry_base_delay_ms: u64,
    #[serde(default = "default_bash_confirm")]
    pub bash_confirm_mode: bool,
    #[serde(default = "default_file_max_size")]
    pub file_max_size_bytes: u64,
}

fn default_listen_addr() -> String {
    "[::1]:50051".into()
}
fn default_log_level() -> String {
    "info".into()
}
fn default_provider() -> String {
    "anthropic".into()
}
fn default_model() -> String {
    "claude-sonnet-4-20250514".into()
}
fn default_temperature() -> f64 {
    0.7
}
fn default_max_tokens() -> u32 {
    4096
}
fn default_bash_timeout() -> u64 {
    120
}
fn default_file_max_size() -> u64 {
    1048576
}
fn default_max_iterations() -> u32 {
    50
}
fn default_retry_attempts() -> u32 {
    3
}
fn default_retry_delay() -> u64 {
    1000
}
fn default_bash_confirm() -> bool {
    true
}

pub fn load_config(config_path: Option<&Path>) -> Result<DaemonConfig, String> {
    if let Some(path) = config_path {
        let content = std::fs::read_to_string(path).map_err(|e| format!("read config: {}", e))?;
        toml::from_str(&content).map_err(|e| format!("parse config: {}", e))
    } else {
        Ok(DaemonConfig {
            daemon: DaemonSection {
                listen_addr: default_listen_addr(),
                log_level: default_log_level(),
            },
            llm: LlmSection {
                provider: default_provider(),
                model: default_model(),
                temperature: default_temperature(),
                max_tokens: default_max_tokens(),
                api_key: None,
                base_url: None,
            },
            tools: ToolsSection {
                bash_timeout_secs: default_bash_timeout(),
                file_max_size_bytes: default_file_max_size(),
            },
            agent: AgentSection {
                max_iterations: default_max_iterations(),
                llm_retry_attempts: default_retry_attempts(),
                llm_retry_base_delay_ms: default_retry_delay(),
                bash_confirm_mode: default_bash_confirm(),
                file_max_size_bytes: default_file_max_size(),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_llm_section_with_api_key() {
        let toml = r#"
[daemon]
listen_addr = "[::1]:50051"

[llm]
api_key = "sk-test-key"
base_url = "https://custom.api.com"

[tools]

[agent]
"#;
        let config: DaemonConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.llm.api_key.as_deref(), Some("sk-test-key"));
        assert_eq!(
            config.llm.base_url.as_deref(),
            Some("https://custom.api.com")
        );
    }

    #[test]
    fn test_load_config_from_file() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        write!(
            file,
            r#"
[daemon]
listen_addr = "127.0.0.1:9090"
log_level = "debug"

[llm]
provider = "openai"
model = "gpt-4"
temperature = 0.5
max_tokens = 2048

[tools]
bash_timeout_secs = 60
file_max_size_bytes = 512000

[agent]
max_iterations = 10
llm_retry_attempts = 5
llm_retry_base_delay_ms = 500
bash_confirm_mode = false
file_max_size_bytes = 256000
"#,
        )
        .unwrap();

        let config = load_config(Some(file.path())).unwrap();
        assert_eq!(config.daemon.listen_addr, "127.0.0.1:9090");
        assert_eq!(config.daemon.log_level, "debug");
        assert_eq!(config.llm.provider, "openai");
        assert_eq!(config.llm.model, "gpt-4");
        assert!((config.llm.temperature - 0.5).abs() < f64::EPSILON);
        assert_eq!(config.llm.max_tokens, 2048);
        assert_eq!(config.tools.bash_timeout_secs, 60);
        assert_eq!(config.tools.file_max_size_bytes, 512000);
        assert_eq!(config.agent.max_iterations, 10);
        assert_eq!(config.agent.llm_retry_attempts, 5);
        assert_eq!(config.agent.llm_retry_base_delay_ms, 500);
        assert!(!config.agent.bash_confirm_mode);
        assert_eq!(config.agent.file_max_size_bytes, 256000);
    }

    #[test]
    fn test_load_config_defaults() {
        let config = load_config(None).unwrap();
        assert_eq!(config.daemon.listen_addr, "[::1]:50051");
        assert_eq!(config.daemon.log_level, "info");
        assert_eq!(config.llm.provider, "anthropic");
        assert_eq!(config.llm.model, "claude-sonnet-4-20250514");
        assert!((config.llm.temperature - 0.7).abs() < f64::EPSILON);
        assert_eq!(config.llm.max_tokens, 4096);
        assert_eq!(config.tools.bash_timeout_secs, 120);
        assert_eq!(config.tools.file_max_size_bytes, 1048576);
        assert_eq!(config.agent.max_iterations, 50);
        assert_eq!(config.agent.llm_retry_attempts, 3);
        assert_eq!(config.agent.llm_retry_base_delay_ms, 1000);
        assert!(config.agent.bash_confirm_mode);
        assert_eq!(config.agent.file_max_size_bytes, 1048576);
    }
}
