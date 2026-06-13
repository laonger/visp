#![allow(dead_code)]

use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;
use visp_mcp::config::McpConfig;

#[derive(Debug, Clone, Deserialize)]
pub struct DaemonConfig {
    pub daemon: DaemonSection,
    pub llm: LlmSection,
    pub tools: ToolsSection,
    pub agent: AgentSection,
    #[serde(default)]
    pub tool: HashMap<String, toml::Value>,
    #[serde(default)]
    pub mcp: McpConfig,
    #[serde(default = "default_storage_section")]
    pub storage: StorageSection,
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
    /// 驱动协议类型（anthropic / openai），接受旧名 provider
    #[serde(default = "default_provider", alias = "provider")]
    pub protocol: String,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_temperature")]
    pub temperature: f64,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    #[serde(default = "default_max_context_tokens")]
    pub max_context_tokens: u32,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    /// Claude thinking 模式预算 token 数（如 2048）
    #[serde(default)]
    pub thinking_budget_tokens: Option<u32>,
    /// 额外的 provider 特定参数（如 OpenAI 的 seed, response_format 等）
    #[serde(default)]
    pub extra: HashMap<String, String>,
    /// 多模型配置列表（支持多个 LLM）
    #[serde(default)]
    pub models: Vec<LlmModelConfig>,
}

/// 单个 LLM 模型配置
#[derive(Debug, Clone, Deserialize)]
pub struct LlmModelConfig {
    /// 显示名称（在 /model 列表中展示）
    pub name: String,
    /// 驱动协议类型（anthropic / openai）
    pub protocol: String,
    /// 服务商显示名，如 "Anthropic" / "OpenAI"；缺省时使用 protocol
    #[serde(default)]
    pub provider: Option<String>,
    pub model: String,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub max_context_tokens: Option<u32>,
    #[serde(default)]
    pub extra: std::collections::HashMap<String, String>,
}

impl LlmModelConfig {
    /// 全局唯一标识 `[provider].[name]`，作为模型切换的 lookup key
    pub fn key(&self) -> String {
        let p = self.provider.as_deref().unwrap_or(&self.protocol);
        format!("{p}.{}", self.name)
    }
}

impl LlmSection {
    /// 返回有效的模型配置列表
    /// 如果配置了 models 数组则使用，否则从单模型字段推导
    pub fn effective_models(&self) -> Vec<LlmModelConfig> {
        if !self.models.is_empty() {
            return self.models.clone();
        }
        vec![LlmModelConfig {
            name: self.model.clone(),
            protocol: self.protocol.clone(),
            provider: Some(self.protocol.clone()),
            model: self.model.clone(),
            api_key: self.api_key.clone(),
            base_url: self.base_url.clone(),
            temperature: Some(self.temperature),
            max_tokens: Some(self.max_tokens),
            max_context_tokens: Some(self.max_context_tokens),
            extra: self.extra.clone(),
        }]
    }

    /// 返回可用的模型名称列表
    /// 如果配置了 models 数组则返回其中的 name，否则用单 model 字段
    pub fn available_models(&self) -> Vec<String> {
        let models = self.effective_models();
        if !models.is_empty() {
            models
                .iter()
                .map(|m| {
                    let display_provider = m.provider.as_deref().unwrap_or(&m.protocol);
                    format!("{} ({})", m.name, display_provider)
                })
                .collect()
        } else {
            vec![self.model.clone()]
        }
    }
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
    #[serde(default = "default_soft_limit", alias = "max_iterations")]
    pub soft_limit: u32,
    #[serde(default = "default_doom_loop_threshold")]
    pub doom_loop_threshold: u32,
    #[serde(default = "default_retry_attempts")]
    pub llm_retry_attempts: u32,
    #[serde(default = "default_retry_delay")]
    pub llm_retry_base_delay_ms: u64,
    #[serde(default = "default_bash_confirm")]
    pub bash_confirm_mode: bool,
    #[serde(default = "default_file_max_size")]
    pub file_max_size_bytes: u64,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct StorageSection {
    #[serde(default = "default_storage_driver")]
    pub driver: String,
    #[serde(default = "default_storage_path")]
    pub path: String,
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
pub(crate) fn default_model() -> String {
    "claude-3-7-sonnet-20250219".into()
}
fn default_temperature() -> f64 {
    0.7
}
fn default_max_tokens() -> u32 {
    4096
}
fn default_max_context_tokens() -> u32 {
    128_000
}
fn default_bash_timeout() -> u64 {
    120
}
fn default_file_max_size() -> u64 {
    1048576
}
fn default_soft_limit() -> u32 {
    50
}
fn default_doom_loop_threshold() -> u32 {
    5
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

fn default_storage_driver() -> String {
    "sqlite".into()
}

fn default_storage_path() -> String {
    "~/.visp/data/visp.db".into()
}

fn default_storage_section() -> StorageSection {
    StorageSection {
        driver: default_storage_driver(),
        path: default_storage_path(),
    }
}

pub fn load_config(config_path: Option<&Path>) -> Result<DaemonConfig, String> {
    if let Some(path) = config_path {
        return load_from_file(path);
    }

    // 尝试默认路径 ~/.config/visp/daemon.toml
    if let Ok(home) = std::env::var("HOME") {
        let default_path = std::path::Path::new(&home)
            .join(".config")
            .join("visp")
            .join("daemon.toml");
        if default_path.exists() {
            return load_from_file(&default_path);
        }
    }

    Ok(default_config())
}

fn load_from_file(path: &Path) -> Result<DaemonConfig, String> {
    let content = std::fs::read_to_string(path).map_err(|e| format!("read config: {}", e))?;
    toml::from_str(&content).map_err(|e| format!("parse config: {}", e))
}

fn default_config() -> DaemonConfig {
    DaemonConfig {
        daemon: DaemonSection {
            listen_addr: default_listen_addr(),
            log_level: default_log_level(),
        },
        llm: LlmSection {
            protocol: default_provider(),
            model: default_model(),
            temperature: default_temperature(),
            max_tokens: default_max_tokens(),
            max_context_tokens: default_max_context_tokens(),
            api_key: None,
            base_url: None,
            thinking_budget_tokens: None,
            extra: HashMap::new(),
            models: Vec::new(),
        },
        tools: ToolsSection {
            bash_timeout_secs: default_bash_timeout(),
            file_max_size_bytes: default_file_max_size(),
        },
        agent: AgentSection {
            soft_limit: default_soft_limit(),
            doom_loop_threshold: default_doom_loop_threshold(),
            llm_retry_attempts: default_retry_attempts(),
            llm_retry_base_delay_ms: default_retry_delay(),
            bash_confirm_mode: default_bash_confirm(),
            file_max_size_bytes: default_file_max_size(),
        },
        tool: HashMap::new(),
        mcp: McpConfig::default(),
        storage: StorageSection {
            driver: default_storage_driver(),
            path: default_storage_path(),
        },
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
soft_limit = 10
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
        assert_eq!(config.llm.protocol, "openai");
        assert_eq!(config.llm.model, "gpt-4");
        assert!((config.llm.temperature - 0.5).abs() < f64::EPSILON);
        assert_eq!(config.llm.max_tokens, 2048);
        assert_eq!(config.tools.bash_timeout_secs, 60);
        assert_eq!(config.tools.file_max_size_bytes, 512000);
        assert_eq!(config.agent.soft_limit, 10);
        assert_eq!(config.agent.llm_retry_attempts, 5);
        assert_eq!(config.agent.llm_retry_base_delay_ms, 500);
        assert!(!config.agent.bash_confirm_mode);
        assert_eq!(config.agent.file_max_size_bytes, 256000);
    }

    #[test]
    fn test_default_config_values() {
        let config = default_config();
        assert_eq!(config.daemon.listen_addr, "[::1]:50051");
        assert_eq!(config.daemon.log_level, "info");
        assert_eq!(config.llm.protocol, "anthropic");
        assert_eq!(config.llm.model, "claude-3-7-sonnet-20250219");
        assert!((config.llm.temperature - 0.7).abs() < f64::EPSILON);
        assert_eq!(config.llm.max_tokens, 4096);
        assert_eq!(config.tools.bash_timeout_secs, 120);
        assert_eq!(config.tools.file_max_size_bytes, 1048576);
        assert_eq!(config.agent.soft_limit, 50);
        assert_eq!(config.agent.llm_retry_attempts, 3);
        assert_eq!(config.agent.llm_retry_base_delay_ms, 1000);
        assert!(config.agent.bash_confirm_mode);
        assert_eq!(config.agent.file_max_size_bytes, 1048576);
    }

    #[test]
    fn test_default_max_context_tokens() {
        assert_eq!(default_max_context_tokens(), 128_000);
    }

    #[test]
    fn test_config_with_explicit_max_context_tokens() {
        let toml = r#"
[daemon]
listen_addr = "[::1]:50051"

[llm]
provider = "anthropic"
model = "claude-3-7-sonnet-20250219"
max_context_tokens = 64000

[tools]

[agent]
"#;
        let config: DaemonConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.llm.max_context_tokens, 64_000);
    }

    #[test]
    fn test_config_missing_max_context_tokens_defaults() {
        let toml = r#"
[daemon]
listen_addr = "[::1]:50051"

[llm]
provider = "anthropic"
model = "claude-3-7-sonnet-20250219"

[tools]

[agent]
"#;
        let config: DaemonConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.llm.max_context_tokens, 128_000);
    }

    #[test]
    fn test_default_config_max_context_tokens() {
        let config = default_config();
        assert_eq!(config.llm.max_context_tokens, 128_000);
    }

    #[test]
    fn test_load_config_auto_find() {
        use std::fs;

        // 创建一个临时 HOME 目录，里面放默认配置文件
        let tmp = tempfile::tempdir().unwrap();
        let config_dir = tmp.path().join(".config").join("visp");
        fs::create_dir_all(&config_dir).unwrap();
        let config_file = config_dir.join("daemon.toml");
        fs::write(
            &config_file,
            r#"
[daemon]
listen_addr = "0.0.0.0:8080"
log_level = "warn"

[llm]
provider = "ollama"
model = "deepseek-v4-flash"
temperature = 0.1
max_tokens = 2048
api_key = "test-key"
base_url = "http://localhost:11434"

[tools]
bash_timeout_secs = 30
file_max_size_bytes = 512000

[agent]
soft_limit = 5
llm_retry_attempts = 1
llm_retry_base_delay_ms = 100
bash_confirm_mode = false
file_max_size_bytes = 256000
"#,
        )
        .unwrap();

        // 设置 HOME 指向临时目录
        let original_home = std::env::var("HOME").ok();
        // safety: 测试中单线程执行，env var 操作安全
        unsafe { std::env::set_var("HOME", tmp.path()) };

        let result = load_config(None);

        // 恢复 HOME
        // safety: 同上
        if let Some(home) = original_home {
            unsafe { std::env::set_var("HOME", &home) };
        } else {
            unsafe { std::env::remove_var("HOME") };
        }

        let config = result.unwrap();
        assert_eq!(config.daemon.listen_addr, "0.0.0.0:8080");
        assert_eq!(config.daemon.log_level, "warn");
        assert_eq!(config.llm.protocol, "ollama");
        assert_eq!(config.llm.model, "deepseek-v4-flash");
        assert_eq!(config.llm.api_key.as_deref(), Some("test-key"));
        assert_eq!(
            config.llm.base_url.as_deref(),
            Some("http://localhost:11434")
        );
        assert_eq!(config.tools.bash_timeout_secs, 30);
        assert_eq!(config.agent.soft_limit, 5);
    }

    #[test]
    fn soft_limit_config() {
        let toml = r#"
[daemon]
listen_addr = "[::1]:50051"

[llm]

[tools]

[agent]
soft_limit = 30
"#;
        let config: DaemonConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.agent.soft_limit, 30);
    }

    #[test]
    fn doom_loop_threshold_config() {
        let toml = r#"
[daemon]
listen_addr = "[::1]:50051"

[llm]

[tools]

[agent]
doom_loop_threshold = 3
"#;
        let config: DaemonConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.agent.doom_loop_threshold, 3);
    }

    #[test]
    fn backward_compat_max_iterations() {
        let toml = r#"
[daemon]
listen_addr = "[::1]:50051"

[llm]

[tools]

[agent]
max_iterations = 50
"#;
        let config: DaemonConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.agent.soft_limit, 50);
    }

    #[test]
    fn default_doom_loop_threshold() {
        let config = default_config();
        assert_eq!(config.agent.doom_loop_threshold, 5);
    }

    #[test]
    fn test_storage_default_sqlite() {
        let config = default_config();
        assert_eq!(config.storage.driver, "sqlite");
    }

    #[test]
    fn test_storage_default_path() {
        let config = default_config();
        assert_eq!(config.storage.path, "~/.visp/data/visp.db");
    }

    #[test]
    fn test_storage_memory_mode() {
        let toml = r#"
[daemon]
listen_addr = "[::1]:50051"

[llm]

[tools]

[agent]

[storage]
driver = "memory"
"#;
        let config: DaemonConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.storage.driver, "memory");
    }

    #[test]
    fn test_storage_custom_path() {
        let toml = r#"
[daemon]
listen_addr = "[::1]:50051"

[llm]

[tools]

[agent]

[storage]
driver = "sqlite"
path = "/tmp/custom/visp.db"
"#;
        let config: DaemonConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.storage.path, "/tmp/custom/visp.db");
    }
}
