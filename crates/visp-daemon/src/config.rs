use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use visp_mcp::config::McpConfig;

#[derive(Debug, Clone, Deserialize)]
pub struct DaemonConfig {
    #[serde(default = "default_daemon_section")]
    pub daemon: DaemonSection,
    #[serde(default)]
    pub llm: LlmSection,
    #[serde(default = "default_tools_section")]
    #[allow(dead_code)]
    pub tools: ToolsSection,
    #[serde(default = "default_agent_section")]
    pub agent: AgentSection,
    #[serde(default)]
    pub tool: HashMap<String, toml::Value>,
    #[serde(default)]
    pub mcp: McpConfig,
    #[serde(default = "default_storage_section")]
    pub storage: StorageSection,
    #[serde(default)]
    #[allow(dead_code)]
    pub observability: ObservabilityConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DaemonSection {
    #[serde(default = "default_listen_addr")]
    pub listen_addr: String,
    #[serde(default = "default_log_level")]
    #[allow(dead_code)]
    pub log_level: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct LlmSection {
    /// Claude thinking 模式预算 token 数（如 2048）
    #[serde(default)]
    pub thinking_budget_tokens: Option<u32>,
    /// 额外的 provider 特定参数（如 OpenAI 的 seed, response_format 等）
    #[serde(default)]
    pub extra: HashMap<String, String>,
    /// 多模型配置列表
    #[serde(default)]
    pub models: Vec<LlmModelConfig>,
    /// 默认模型 key（格式 {provider}.{name}），缺省时使用 models 第一个
    #[serde(default)]
    pub default: Option<String>,
}

/// 单个 LLM 模型配置
#[derive(Debug, Clone, Deserialize)]
pub struct LlmModelConfig {
    /// 模型显示名（在 /model 列表中展示）
    pub name: String,
    /// 驱动协议（openai / anthropic）
    pub protocol: String,
    /// 服务商名字，如 "Anthropic" / "OpenAI"；缺省时使用 protocol
    #[serde(default)]
    pub provider: Option<String>,
    /// 请求时的模型 key
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
    pub thinking_budget_tokens: Option<u32>,
    #[serde(default)]
    #[allow(dead_code)]
    pub extra: std::collections::HashMap<String, String>,
}

impl LlmModelConfig {
    /// 全局唯一标识 `{provider}/{name}`，作为模型切换的 lookup key
    pub fn key(&self) -> String {
        let p = self.provider.as_deref().unwrap_or(&self.protocol);
        format!("{p}/{}", self.name)
    }

    /// 模型别名 `{provider}/{model}`，作为备选 lookup key
    pub fn model_alias(&self) -> String {
        let p = self.provider.as_deref().unwrap_or(&self.protocol);
        format!("{p}/{}", self.model)
    }

    /// 检查给定的 key 是否匹配此模型（匹配 key 或 model_alias）
    pub fn matches_key(&self, candidate: &str) -> bool {
        self.key() == candidate || self.model_alias() == candidate
    }

    /// 判断两个模型配置是否为同一个模型（用于 config merge 时的匹配）
    fn matches_model(&self, other: &LlmModelConfig) -> bool {
        self.matches_key(&other.key())
            || self.matches_key(&other.model_alias())
            || (self.model == other.model && self.protocol == other.protocol)
    }

    /// 用 `override_cfg` 的值覆盖当前配置中已设置（Some）的字段。
    /// 用于 project config 覆盖 global config。
    fn merge_override(&mut self, override_cfg: &LlmModelConfig) {
        // 必填字段：override 总是覆盖
        self.name = override_cfg.name.clone();
        self.protocol = override_cfg.protocol.clone();
        self.model = override_cfg.model.clone();
        // Optional 字段：仅当 override 中为 Some 时覆盖
        if override_cfg.provider.is_some() {
            self.provider = override_cfg.provider.clone();
        }
        if override_cfg.api_key.is_some() {
            self.api_key = override_cfg.api_key.clone();
        }
        if override_cfg.base_url.is_some() {
            self.base_url = override_cfg.base_url.clone();
        }
        if override_cfg.temperature.is_some() {
            self.temperature = override_cfg.temperature;
        }
        if override_cfg.max_tokens.is_some() {
            self.max_tokens = override_cfg.max_tokens;
        }
        if override_cfg.max_context_tokens.is_some() {
            self.max_context_tokens = override_cfg.max_context_tokens;
        }
        if override_cfg.thinking_budget_tokens.is_some() {
            self.thinking_budget_tokens = override_cfg.thinking_budget_tokens;
        }
        // extra: merge map, override entries 优先
        for (k, v) in &override_cfg.extra {
            self.extra.insert(k.clone(), v.clone());
        }
    }
}

impl LlmSection {
    /// 返回有效的模型配置列表
    pub fn effective_models(&self) -> Vec<LlmModelConfig> {
        self.models.clone()
    }

    /// 返回可用的模型显示标签列表
    pub fn available_models(&self) -> Vec<String> {
        if self.models.is_empty() {
            return vec![];
        }
        self.models
            .iter()
            .map(|m| {
                let display_provider = m.provider.as_deref().unwrap_or(&m.protocol);
                format!("{}({})", m.name, display_provider)
            })
            .collect()
    }

    /// 解析默认模型 key（格式 {provider}/{name} 或 {provider}/{model}）。
    /// 优先使用 `default` 字段指定的模型，找不到时回退到 model_configs 第一个。
    pub fn resolve_default_key(&self, model_configs: &[LlmModelConfig]) -> String {
        if let Some(ref default_name) = self.default {
            if let Some(mc) = model_configs.iter().find(|mc| mc.matches_key(default_name)) {
                return mc.key();
            }
            tracing::warn!(
                default = %default_name,
                available = %model_configs.iter().map(|m| m.key()).collect::<Vec<_>>().join(", "),
                "llm.default points to unknown model, falling back to first model"
            );
        }
        model_configs
            .first()
            .map(|mc| mc.key())
            .unwrap_or_else(|| "default".to_string())
    }
}

/// 将 project config 的 LLM 部分合并到 global config 中。
/// 优先级：project config > global config。
/// - project 中新增的模型会被追加到 models 列表。
/// - project 中已存在的模型（key/model_alias/model+protocol 匹配）会用 project 的值覆盖 global 的值。
/// - project 的 default 若设置，则覆盖 global 的 default。
fn merge_llm_sections(global: &mut LlmSection, project: &LlmSection) {
    if project.thinking_budget_tokens.is_some() {
        global.thinking_budget_tokens = project.thinking_budget_tokens;
    }
    if project.default.is_some() {
        global.default = project.default.clone();
    }
    // extra: merge map, project entries 优先
    for (k, v) in &project.extra {
        global.extra.insert(k.clone(), v.clone());
    }
    // models: 对 project 中的每个模型，尝试在 global 中找到匹配项并 merge override，
    // 如果找不到匹配项则追加为新模型
    for project_model in &project.models {
        let mut found = false;
        for global_model in &mut global.models {
            if global_model.matches_model(project_model) {
                global_model.merge_override(project_model);
                found = true;
                break;
            }
        }
        if !found {
            global.models.push(project_model.clone());
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ToolsSection {
    #[allow(dead_code)]
    #[serde(default = "default_bash_timeout")]
    pub bash_timeout_secs: u64,
    #[allow(dead_code)]
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
    #[serde(default = "default_max_depth")]
    pub max_depth: u32,
    /// 内置 agent 覆盖配置（model / temperature / steps）
    #[serde(default)]
    pub builtin: Vec<BuiltinAgentConfig>,
}

/// 内置 agent 配置覆盖项，用于在 daemon.toml 中为内置 agent
/// （如 explorer、fixer）指定 LLM 模型等参数。
///
/// ```toml
/// [[agent.builtin]]
/// name = "explorer"
/// model = "Opencode/deepseek-v4-flash"
/// temperature = 0.1
/// ```
#[derive(Debug, Clone, Deserialize)]
pub struct BuiltinAgentConfig {
    /// 内置 agent 名称，如 "explorer"、"fixer"
    pub name: String,
    /// 模型 key（格式 {provider}/{model} 或 {provider}.{name}）
    #[serde(default)]
    pub model: Option<String>,
    /// 温度
    #[serde(default)]
    pub temperature: Option<f32>,
    /// 最大迭代次数
    #[serde(default)]
    pub steps: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct StorageSection {
    #[serde(default = "default_storage_driver")]
    pub driver: String,
    #[serde(default = "default_storage_path")]
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObservabilityConfig {
    #[serde(default = "default_observability_enabled")]
    pub enabled: bool,
    #[serde(default = "default_observability_level")]
    pub level: String,
    #[serde(default = "default_observability_format")]
    pub format: String,
    #[serde(default = "default_observability_parent_link")]
    pub parent_link: bool,
    #[serde(default = "default_observability_metrics_summary")]
    pub metrics_summary: bool,
    #[serde(default = "default_observability_log_file")]
    pub log_file: Option<String>,
    #[serde(default)]
    pub otlp: OtlpConfig,
    #[serde(default)]
    pub langfuse: LangfuseConfig,
}

/// Langfuse capture switch configuration
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LangfuseCaptureConfig {
    #[serde(default)]
    pub input: bool,
    #[serde(default)]
    pub output: bool,
    /// 最大字符数（超出截断，默认 20000）
    #[serde(default = "default_capture_max_chars")]
    pub max_chars: usize,
    /// 是否脱敏敏感字段（默认 true）
    #[serde(default = "default_capture_redact_secrets")]
    pub redact_secrets: bool,
}

fn default_capture_max_chars() -> usize {
    20_000
}

fn default_capture_redact_secrets() -> bool {
    true
}

impl Default for LangfuseCaptureConfig {
    fn default() -> Self {
        Self {
            input: false,
            output: false,
            max_chars: default_capture_max_chars(),
            redact_secrets: default_capture_redact_secrets(),
        }
    }
}

/// Langfuse 观测配置
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct LangfuseConfig {
    /// 总开关（默认关闭，需显式开启）
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub environment: Option<String>,
    #[serde(default)]
    pub release: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    /// 三态：未设置/true/false（非布尔值报错）
    #[serde(default)]
    pub public: Option<bool>,
    /// 任意元数据（标量原样，数组/table 转紧凑 JSON）
    #[serde(default)]
    pub metadata: Option<toml::value::Table>,
    /// P1 capture 开关（仅解析，功能待实现）
    #[serde(default)]
    pub capture: LangfuseCaptureConfig,
}

impl Default for ObservabilityConfig {
    fn default() -> Self {
        Self {
            enabled: default_observability_enabled(),
            level: default_observability_level(),
            format: default_observability_format(),
            parent_link: default_observability_parent_link(),
            metrics_summary: default_observability_metrics_summary(),
            log_file: default_observability_log_file(),
            otlp: OtlpConfig::default(),
            langfuse: LangfuseConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OtlpConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_otlp_endpoint")]
    pub endpoint: String,
    #[serde(default = "default_otlp_protocol")]
    pub protocol: String,
    #[serde(default = "default_otlp_timeout_secs")]
    pub timeout_secs: u64,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(
        default = "default_sample_rate",
        deserialize_with = "deserialize_clamped_sample_rate"
    )]
    pub sample_rate: f64,
}

impl Default for OtlpConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            endpoint: default_otlp_endpoint(),
            protocol: default_otlp_protocol(),
            timeout_secs: default_otlp_timeout_secs(),
            headers: BTreeMap::new(),
            sample_rate: default_sample_rate(),
        }
    }
}

fn default_otlp_endpoint() -> String {
    "http://localhost:4317".into()
}
fn default_otlp_protocol() -> String {
    "grpc".into()
}
fn default_otlp_timeout_secs() -> u64 {
    10
}
fn default_sample_rate() -> f64 {
    1.0
}

fn deserialize_clamped_sample_rate<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = f64::deserialize(deserializer)?;
    Ok(value.clamp(0.0, 1.0))
}

fn default_listen_addr() -> String {
    "[::1]:50051".into()
}
fn default_log_level() -> String {
    "info".into()
}
fn default_bash_timeout() -> u64 {
    120
}
fn default_file_max_size() -> u64 {
    1048576
}

fn default_max_depth() -> u32 {
    5
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

fn default_observability_enabled() -> bool {
    true
}
fn default_observability_level() -> String {
    "info".into()
}
fn default_observability_format() -> String {
    "json".into()
}
fn default_observability_parent_link() -> bool {
    true
}
fn default_observability_metrics_summary() -> bool {
    true
}
fn default_observability_log_file() -> Option<String> {
    Some("~/.visp/logs".into())
}

pub fn load_config(config_path: Option<&Path>) -> Result<DaemonConfig, String> {
    // 1. 如果通过 CLI 参数指定了配置文件，直接使用该文件（最高优先级，跳过 merge）
    if let Some(path) = config_path {
        return load_from_file(path);
    }

    // 2. 加载全局配置 ~/.config/visp/daemon.toml
    let mut config = if let Ok(home) = std::env::var("HOME") {
        let global_path = std::path::Path::new(&home)
            .join(".config")
            .join("visp")
            .join("daemon.toml");
        if global_path.exists() {
            load_from_file(&global_path)?
        } else {
            default_config()
        }
    } else {
        default_config()
    };

    // 3. 加载项目配置 cwd/.visp/daemon.toml，merge 到全局配置（项目优先级更高）
    if let Ok(cwd) = std::env::current_dir() {
        let project_path = cwd.join(".visp").join("daemon.toml");
        if project_path.exists() {
            tracing::info!(
                path = %project_path.display(),
                "loading project-level config"
            );
            match load_from_file(&project_path) {
                Ok(project_config) => {
                    merge_llm_sections(&mut config.llm, &project_config.llm);
                }
                Err(e) => {
                    tracing::warn!(path = %project_path.display(), error = %e, "failed to load project config, ignoring");
                }
            }
        }
    }

    Ok(config)
}

fn load_from_file(path: &Path) -> Result<DaemonConfig, String> {
    let content = std::fs::read_to_string(path).map_err(|e| format!("read config: {}", e))?;
    toml::from_str(&content).map_err(|e| format!("parse config: {}", e))
}

fn default_daemon_section() -> DaemonSection {
    DaemonSection {
        listen_addr: default_listen_addr(),
        log_level: default_log_level(),
    }
}

fn default_tools_section() -> ToolsSection {
    ToolsSection {
        bash_timeout_secs: default_bash_timeout(),
        file_max_size_bytes: default_file_max_size(),
    }
}

fn default_agent_section() -> AgentSection {
    AgentSection {
        soft_limit: default_soft_limit(),
        doom_loop_threshold: default_doom_loop_threshold(),
        llm_retry_attempts: default_retry_attempts(),
        llm_retry_base_delay_ms: default_retry_delay(),
        bash_confirm_mode: default_bash_confirm(),
        file_max_size_bytes: default_file_max_size(),
        max_depth: default_max_depth(),
        builtin: Vec::new(),
    }
}

fn default_config() -> DaemonConfig {
    DaemonConfig {
        daemon: default_daemon_section(),
        llm: LlmSection {
            thinking_budget_tokens: None,
            extra: HashMap::new(),
            models: Vec::new(),
            default: None,
        },
        tools: default_tools_section(),
        agent: default_agent_section(),
        tool: HashMap::new(),
        mcp: McpConfig::default(),
        storage: StorageSection {
            driver: default_storage_driver(),
            path: default_storage_path(),
        },
        observability: ObservabilityConfig::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;

    #[test]
    fn test_merge_llm_sections_project_overrides_global_api_key_and_base_url() {
        let global_toml = r#"
[daemon]
listen_addr = "[::1]:50051"

[llm]
[[llm.models]]
name = "Sonnet"
protocol = "anthropic"
model = "claude-sonnet-4-20250514"
api_key = "global-key"
base_url = "https://global.example.com"

[tools]

[agent]
"#;
        let project_toml = r#"
[llm]
[[llm.models]]
name = "Sonnet"
protocol = "anthropic"
model = "claude-sonnet-4-20250514"
api_key = "project-key"
base_url = "https://project.example.com"
"#;
        let mut global: DaemonConfig = toml::from_str(global_toml).unwrap();
        let project: DaemonConfig = toml::from_str(project_toml).unwrap();

        merge_llm_sections(&mut global.llm, &project.llm);

        assert_eq!(global.llm.models.len(), 1);
        assert_eq!(global.llm.models[0].api_key.as_deref(), Some("project-key"));
        assert_eq!(
            global.llm.models[0].base_url.as_deref(),
            Some("https://project.example.com")
        );
    }

    #[test]
    fn test_merge_llm_sections_project_adds_new_model() {
        let global_toml = r#"
[daemon]
listen_addr = "[::1]:50051"

[llm]
[[llm.models]]
name = "Sonnet"
protocol = "anthropic"
model = "claude-sonnet-4-20250514"

[tools]

[agent]
"#;
        let project_toml = r#"
[llm]
[[llm.models]]
name = "GPT-4o"
protocol = "openai"
model = "gpt-4o"
api_key = "sk-project-key"
"#;
        let mut global: DaemonConfig = toml::from_str(global_toml).unwrap();
        let project: DaemonConfig = toml::from_str(project_toml).unwrap();

        merge_llm_sections(&mut global.llm, &project.llm);

        assert_eq!(global.llm.models.len(), 2);
        assert_eq!(global.llm.models[0].name, "Sonnet");
        assert_eq!(global.llm.models[1].name, "GPT-4o");
        assert_eq!(
            global.llm.models[1].api_key.as_deref(),
            Some("sk-project-key")
        );
    }

    #[test]
    fn test_merge_llm_sections_project_partial_override_keeps_global_fields() {
        let global_toml = r#"
[daemon]
listen_addr = "[::1]:50051"

[llm]
[[llm.models]]
name = "Sonnet"
protocol = "anthropic"
model = "claude-sonnet-4-20250514"
api_key = "global-key"
base_url = "https://global.example.com"
temperature = 0.5
max_tokens = 8192

[tools]

[agent]
"#;
        // project config only overrides api_key, leaves everything else
        let project_toml = r#"
[llm]
[[llm.models]]
name = "Sonnet"
protocol = "anthropic"
model = "claude-sonnet-4-20250514"
api_key = "project-key"
"#;
        let mut global: DaemonConfig = toml::from_str(global_toml).unwrap();
        let project: DaemonConfig = toml::from_str(project_toml).unwrap();

        merge_llm_sections(&mut global.llm, &project.llm);

        assert_eq!(global.llm.models.len(), 1);
        assert_eq!(global.llm.models[0].api_key.as_deref(), Some("project-key"));
        // base_url should be kept from global
        assert_eq!(
            global.llm.models[0].base_url.as_deref(),
            Some("https://global.example.com")
        );
        // temperature should be kept from global
        assert!((global.llm.models[0].temperature.unwrap() - 0.5).abs() < f64::EPSILON);
        // max_tokens should be kept from global
        assert_eq!(global.llm.models[0].max_tokens, Some(8192));
    }

    #[test]
    fn test_merge_llm_sections_project_overrides_default() {
        let global_toml = r#"
[daemon]
listen_addr = "[::1]:50051"

[llm]
default = "Anthropic/Sonnet"
[[llm.models]]
name = "Sonnet"
provider = "Anthropic"
protocol = "anthropic"
model = "claude-sonnet-4-20250514"

[[llm.models]]
name = "GPT-4o"
provider = "OpenAI"
protocol = "openai"
model = "gpt-4o"

[tools]

[agent]
"#;
        let project_toml = r#"
[llm]
default = "OpenAI/GPT-4o"
"#;
        let mut global: DaemonConfig = toml::from_str(global_toml).unwrap();
        let project: DaemonConfig = toml::from_str(project_toml).unwrap();

        merge_llm_sections(&mut global.llm, &project.llm);

        assert_eq!(global.llm.default.as_deref(), Some("OpenAI/GPT-4o"));
    }

    #[test]
    fn test_merge_llm_sections_match_by_model_alias() {
        // global config uses provider, project config omits provider
        // but model + protocol should still match
        let global_toml = r#"
[daemon]
listen_addr = "[::1]:50051"

[llm]
[[llm.models]]
name = "Sonnet"
provider = "Anthropic"
protocol = "anthropic"
model = "claude-sonnet-4-20250514"
api_key = "global-key"

[tools]

[agent]
"#;
        // project: no provider, but model + protocol match
        let project_toml = r#"
[llm]
[[llm.models]]
name = "Sonnet"
protocol = "anthropic"
model = "claude-sonnet-4-20250514"
api_key = "project-key"
"#;
        let mut global: DaemonConfig = toml::from_str(global_toml).unwrap();
        let project: DaemonConfig = toml::from_str(project_toml).unwrap();

        merge_llm_sections(&mut global.llm, &project.llm);

        // Should merge into one model, not add a second
        assert_eq!(global.llm.models.len(), 1);
        assert_eq!(global.llm.models[0].api_key.as_deref(), Some("project-key"));
    }

    #[test]

    fn test_llm_section_with_api_key() {
        let toml = r#"
[daemon]
listen_addr = "[::1]:50051"

[llm]
[[llm.models]]
name = "gpt-4"
protocol = "openai"
model = "gpt-4"
api_key = "sk-test-key"
base_url = "https://custom.api.com"

[tools]

[agent]
"#;
        let config: DaemonConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.llm.models[0].api_key.as_deref(), Some("sk-test-key"));
        assert_eq!(
            config.llm.models[0].base_url.as_deref(),
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
[[llm.models]]
name = "gpt-4"
protocol = "openai"
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
        assert_eq!(config.llm.models[0].protocol, "openai");
        assert_eq!(config.llm.models[0].model, "gpt-4");
        assert!((config.llm.models[0].temperature.unwrap() - 0.5).abs() < f64::EPSILON);
        assert_eq!(config.llm.models[0].max_tokens.unwrap(), 2048);
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
        assert_eq!(config.llm.models.len(), 0);
        assert!(config.llm.models.is_empty());
        assert_eq!(config.tools.bash_timeout_secs, 120);
        assert_eq!(config.tools.file_max_size_bytes, 1048576);
        assert_eq!(config.agent.soft_limit, 50);
        assert_eq!(config.agent.llm_retry_attempts, 3);
        assert_eq!(config.agent.llm_retry_base_delay_ms, 1000);
        assert!(config.agent.bash_confirm_mode);
        assert_eq!(config.agent.file_max_size_bytes, 1048576);
    }

    #[test]
    fn test_config_with_explicit_max_context_tokens() {
        let toml = r#"
[daemon]
listen_addr = "[::1]:50051"

[llm]
[[llm.models]]
name = "test"
protocol = "anthropic"
model = "claude-3-7-sonnet-20250219"
max_context_tokens = 64000

[tools]

[agent]
"#;
        let config: DaemonConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.llm.models[0].max_context_tokens, Some(64_000));
    }

    #[test]
    fn test_llmmodelconfig_with_thinking_budget_tokens() {
        let toml = r#"
[daemon]
listen_addr = "[::1]:50051"

[llm]
[[llm.models]]
name = "test"
protocol = "anthropic"
model = "claude-3-7-sonnet-20250219"
thinking_budget_tokens = 2048

[tools]

[agent]
"#;
        let config: DaemonConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.llm.models[0].thinking_budget_tokens, Some(2048));
    }

    #[test]
    fn test_llmmodelconfig_without_thinking_budget_defaults_to_none() {
        let toml = r#"
[daemon]
listen_addr = "[::1]:50051"

[llm]
[[llm.models]]
name = "test"
protocol = "anthropic"
model = "claude-3-7-sonnet-20250219"

[tools]

[agent]
"#;
        let config: DaemonConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.llm.models[0].thinking_budget_tokens, None);
    }

    #[test]
    fn test_config_missing_max_context_tokens_defaults() {
        let toml = r#"
[daemon]
listen_addr = "[::1]:50051"

[llm]
[[llm.models]]
name = "test"
protocol = "anthropic"
model = "claude-3-7-sonnet-20250219"

[tools]

[agent]
"#;
        let config: DaemonConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.llm.models[0].max_context_tokens, None);
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
[[llm.models]]
name = "DeepSeek"
protocol = "ollama"
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
        assert_eq!(config.llm.models[0].protocol, "ollama");
        assert_eq!(config.llm.models[0].model, "deepseek-v4-flash");
        assert_eq!(config.llm.models[0].api_key.as_deref(), Some("test-key"));
        assert_eq!(
            config.llm.models[0].base_url.as_deref(),
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

    #[test]
    fn test_observability_config_default() {
        let config = default_config();
        assert!(config.observability.enabled);
        assert_eq!(config.observability.level, "info");
        assert_eq!(config.observability.format, "json");
        assert!(config.observability.parent_link);
        assert!(config.observability.metrics_summary);
        assert_eq!(config.observability.log_file, Some("~/.visp/logs".into()));
    }

    #[test]
    fn test_observability_config_disabled_via_toml() {
        let toml = r#"
[daemon]
listen_addr = "[::1]:50051"

[llm]

[tools]

[agent]

[observability]
enabled = false
"#;
        let config: DaemonConfig = toml::from_str(toml).unwrap();
        assert!(!config.observability.enabled);
        assert_eq!(config.observability.level, "info");
        assert_eq!(config.observability.format, "json");
        assert!(config.observability.parent_link);
        assert!(config.observability.metrics_summary);
        assert_eq!(config.observability.log_file, Some("~/.visp/logs".into()));
    }

    #[test]
    fn test_observability_config_full_override_via_toml() {
        let toml = r#"
[daemon]
listen_addr = "[::1]:50051"

[llm]

[tools]

[agent]

[observability]
enabled = false
level = "debug"
format = "text"
parent_link = false
metrics_summary = false
log_file = "/tmp/test.log"
"#;
        let config: DaemonConfig = toml::from_str(toml).unwrap();
        assert!(!config.observability.enabled);
        assert_eq!(config.observability.level, "debug");
        assert_eq!(config.observability.format, "text");
        assert!(!config.observability.parent_link);
        assert!(!config.observability.metrics_summary);
        assert_eq!(config.observability.log_file, Some("/tmp/test.log".into()));
    }

    #[test]
    fn test_observability_config_log_file_path() {
        let toml = r#"
[daemon]
listen_addr = "[::1]:50051"

[llm]

[tools]

[agent]

[observability]
log_file = "/tmp/test.log"
"#;
        let config: DaemonConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.observability.log_file, Some("/tmp/test.log".into()));
    }

    #[test]
    fn test_daemon_config_default_includes_observability() {
        let config = default_config();
        assert_eq!(config.observability, ObservabilityConfig::default());
    }

    #[test]
    fn test_otlp_config_defaults_disabled() {
        let cfg = OtlpConfig::default();
        assert!(!cfg.enabled);
        assert_eq!(cfg.endpoint, "http://localhost:4317");
        assert_eq!(cfg.protocol, "grpc");
        assert_eq!(cfg.timeout_secs, 10);
        assert!(cfg.headers.is_empty());
        assert!((cfg.sample_rate - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_otlp_config_deserializes_from_toml() {
        let toml = r#"
[daemon]
listen_addr = "[::1]:50051"

[llm]

[tools]

[agent]

[observability]
enabled = true

[observability.otlp]
enabled = true
endpoint = "http://otel.example.com:4318"
protocol = "http/protobuf"
timeout_secs = 30
sample_rate = 0.1

[observability.otlp.headers]
x-api-key = "abc123"
environment = "test"
"#;
        let config: DaemonConfig = toml::from_str(toml).unwrap();
        assert!(config.observability.otlp.enabled);
        assert_eq!(
            config.observability.otlp.endpoint,
            "http://otel.example.com:4318"
        );
        assert_eq!(config.observability.otlp.protocol, "http/protobuf");
        assert_eq!(config.observability.otlp.timeout_secs, 30);
        assert!((config.observability.otlp.sample_rate - 0.1).abs() < f64::EPSILON);
        assert_eq!(
            config.observability.otlp.headers.get("x-api-key").unwrap(),
            "abc123"
        );
        assert_eq!(
            config
                .observability
                .otlp
                .headers
                .get("environment")
                .unwrap(),
            "test"
        );
    }

    #[test]
    fn test_otlp_config_omitted_section_is_default() {
        let toml = r#"
[daemon]
listen_addr = "[::1]:50051"

[llm]

[tools]

[agent]

[observability]
enabled = true
"#;
        let config: DaemonConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.observability.otlp, OtlpConfig::default());
    }

    #[test]
    fn test_otlp_config_headers_kv_pairs() {
        let toml = r#"
[daemon]
listen_addr = "[::1]:50051"

[llm]

[tools]

[agent]

[observability.otlp.headers]
a = "1"
b = "2"
c = "3"
"#;
        let config: DaemonConfig = toml::from_str(toml).unwrap();
        let keys: Vec<&str> = config
            .observability
            .otlp
            .headers
            .keys()
            .map(|s| s.as_str())
            .collect();
        assert_eq!(keys, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_otlp_config_sample_rate_clamped() {
        // Below 0.0 should clamp to 0.0
        let toml = r#"
[daemon]
listen_addr = "[::1]:50051"

[llm]

[tools]

[agent]

[observability.otlp]
sample_rate = -0.5
"#;
        let config: DaemonConfig = toml::from_str(toml).unwrap();
        assert!((config.observability.otlp.sample_rate - 0.0).abs() < f64::EPSILON);

        // Above 1.0 should clamp to 1.0
        let toml2 = r#"
[daemon]
listen_addr = "[::1]:50051"

[llm]

[tools]

[agent]

[observability.otlp]
sample_rate = 2.5
"#;
        let config2: DaemonConfig = toml::from_str(toml2).unwrap();
        assert!((config2.observability.otlp.sample_rate - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_observability_langfuse_config() {
        let toml = r#"
[daemon]
listen_addr = "[::1]:50051"

[llm]

[tools]

[agent]

[observability]
enabled = true

[observability.langfuse]
user_id = "user_456"
tags = ["agent", "weather"]
"#;
        let config: DaemonConfig = toml::from_str(toml).unwrap();
        assert_eq!(
            config.observability.langfuse.user_id.as_deref(),
            Some("user_456")
        );
        assert_eq!(
            config.observability.langfuse.tags,
            vec!["agent".to_string(), "weather".to_string()]
        );
    }

    #[test]
    fn test_observability_langfuse_defaults() {
        let config = default_config();
        assert!(!config.observability.langfuse.enabled);
        assert_eq!(config.observability.langfuse.user_id, None);
        assert!(config.observability.langfuse.tags.is_empty());
        assert_eq!(config.observability.langfuse.environment, None);
        assert_eq!(config.observability.langfuse.release, None);
        assert_eq!(config.observability.langfuse.version, None);
        assert_eq!(config.observability.langfuse.public, None);
        assert_eq!(config.observability.langfuse.metadata, None);
        assert!(!config.observability.langfuse.capture.input);
        assert!(!config.observability.langfuse.capture.output);
        assert_eq!(config.observability.langfuse.capture.max_chars, 20000);
        assert!(config.observability.langfuse.capture.redact_secrets);
    }

    #[test]
    fn test_observability_langfuse_full_capture_config() {
        let toml = r#"
[daemon]
listen_addr = "[::1]:50051"

[llm]

[tools]

[agent]

[observability.langfuse.capture]
input = true
output = true
max_chars = 10000
redact_secrets = false
"#;
        let config: DaemonConfig = toml::from_str(toml).unwrap();
        let capture = &config.observability.langfuse.capture;
        assert!(capture.input);
        assert!(capture.output);
        assert_eq!(capture.max_chars, 10000);
        assert!(!capture.redact_secrets);
    }

    #[test]
    fn test_observability_langfuse_omitted_section_is_default() {
        let toml = r#"
[daemon]
listen_addr = "[::1]:50051"

[llm]

[tools]

[agent]

[observability]
enabled = true
"#;
        let config: DaemonConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.observability.langfuse, LangfuseConfig::default());
    }

    // ── 1a: 扩展 Langfuse 配置 ──────────────────────────────────────────

    #[test]
    fn test_langfuse_config_default() {
        let cfg = LangfuseConfig::default();
        assert!(!cfg.enabled);
        assert_eq!(cfg.user_id, None);
        assert!(cfg.tags.is_empty());
        assert_eq!(cfg.environment, None);
        assert_eq!(cfg.release, None);
        assert_eq!(cfg.version, None);
        assert_eq!(cfg.public, None);
        assert_eq!(cfg.metadata, None);
        assert!(!cfg.capture.input);
        assert!(!cfg.capture.output);
        assert_eq!(cfg.capture.max_chars, 20000);
        assert!(cfg.capture.redact_secrets);
    }

    #[test]
    fn test_langfuse_config_full() {
        let toml = r#"
[daemon]
listen_addr = "[::1]:50051"

[llm]

[tools]

[agent]

[observability.langfuse]
enabled = true
user_id = "user_456"
tags = ["agent", "weather"]
environment = "production"
release = "1.0.0"
version = "abc123"
public = true

[observability.langfuse.metadata]
env = "prod"
count = 42
items = ["a", "b"]

[observability.langfuse.metadata.nested]
key = "val"

[observability.langfuse.capture]
input = true
output = true
max_chars = 15000
redact_secrets = false
"#;
        let config: DaemonConfig = toml::from_str(toml).unwrap();
        let lf = &config.observability.langfuse;
        assert!(lf.enabled);
        assert_eq!(lf.user_id.as_deref(), Some("user_456"));
        assert_eq!(lf.tags, vec!["agent", "weather"]);
        assert_eq!(lf.environment.as_deref(), Some("production"));
        assert_eq!(lf.release.as_deref(), Some("1.0.0"));
        assert_eq!(lf.version.as_deref(), Some("abc123"));
        assert_eq!(lf.public, Some(true));
        let meta = lf.metadata.as_ref().unwrap();
        assert_eq!(meta.get("env").and_then(|v| v.as_str()), Some("prod"));
        assert!(meta.contains_key("count"));
        assert!(meta.contains_key("items"));
        assert!(meta.contains_key("nested"));
        assert!(lf.capture.input);
        assert!(lf.capture.output);
        assert_eq!(lf.capture.max_chars, 15000);
        assert!(!lf.capture.redact_secrets);
    }

    #[test]
    fn test_langfuse_config_public_tri_state() {
        // not set → None
        let config = default_config();
        assert_eq!(config.observability.langfuse.public, None);

        // false → Some(false)
        let toml_false = r#"
[daemon]
listen_addr = "[::1]:50051"

[llm]

[tools]

[agent]

[observability.langfuse]
public = false
"#;
        let cfg_false: DaemonConfig = toml::from_str(toml_false).unwrap();
        assert_eq!(cfg_false.observability.langfuse.public, Some(false));

        // true → Some(true)
        let toml_true = r#"
[daemon]
listen_addr = "[::1]:50051"

[llm]

[tools]

[agent]

[observability.langfuse]
public = true
"#;
        let cfg_true: DaemonConfig = toml::from_str(toml_true).unwrap();
        assert_eq!(cfg_true.observability.langfuse.public, Some(true));
    }

    #[test]
    fn test_langfuse_config_public_non_bool_error() {
        let toml = r#"
[daemon]
listen_addr = "[::1]:50051"

[llm]

[tools]

[agent]

[observability.langfuse]
public = "notabool"
"#;
        let result: Result<DaemonConfig, toml::de::Error> = toml::from_str(toml);
        assert!(result.is_err(), "non-boolean public should fail to parse");
    }

    #[test]
    fn test_langfuse_config_default_disabled() {
        let config = default_config();
        assert!(!config.observability.langfuse.enabled);
    }

    // ── 6a: Collector 示例与示例配置 ─────────────────────────────────────

    /// Helper: 从 crate 目录回溯到 workspace docs 目录
    fn docs_dir() -> PathBuf {
        let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        // crate_dir = .../visp/crates/visp-daemon
        crate_dir
            .parent()
            .expect("crates")
            .parent()
            .expect("visp")
            .join("docs")
    }

    #[test]
    fn test_example_collector_yaml_exists() {
        let path = docs_dir().join("otel-collector-langfuse.example.yaml");
        assert!(
            path.exists(),
            "Collector 示例文件不存在: {}",
            path.display()
        );
    }

    #[test]
    fn test_example_collector_yaml_content() {
        let path = docs_dir().join("otel-collector-langfuse.example.yaml");
        let content = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("读取 {} 失败: {}", path.display(), e));

        // 必须包含 OTLP gRPC receiver
        assert!(content.contains("grpc"), "Collector 示例缺少 gRPC receiver");
        // 必包含 OTLP HTTP exporter
        assert!(
            content.contains("http/protobuf"),
            "Collector 示例缺少 OTLP HTTP exporter（http/protobuf）"
        );
        // 必包含 x-langfuse-ingestion-version=4
        assert!(
            content.contains("x-langfuse-ingestion-version") && content.contains("4"),
            "Collector 示例缺少 x-langfuse-ingestion-version=4"
        );
    }

    #[test]
    fn test_example_collector_yaml_no_real_secrets() {
        let path = docs_dir().join("otel-collector-langfuse.example.yaml");
        let content = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("读取 {} 失败: {}", path.display(), e));

        // 检查 secret key 行：如果有 sk-lf- 必须是占位符 ${...} 或明显示例值（含 example/test/your）
        for line in content.lines() {
            if line.contains("sk-lf-") {
                let trimmed = line.trim();
                assert!(
                    trimmed.contains('$')
                        || trimmed.contains("example")
                        || trimmed.contains("YOUR_")
                        || trimmed.contains("your-")
                        || trimmed.contains("your_"),
                    "Collector 示例包含疑似真实 sk-lf- secret: {}",
                    trimmed
                );
            }
            if line.contains("pk-lf-") {
                let trimmed = line.trim();
                assert!(
                    trimmed.contains('$')
                        || trimmed.contains("example")
                        || trimmed.contains("YOUR_")
                        || trimmed.contains("your-")
                        || trimmed.contains("your_"),
                    "Collector 示例包含疑似真实 pk-lf- public key: {}",
                    trimmed
                );
            }
        }
    }

    #[test]
    fn test_example_daemon_toml_has_langfuse_config() {
        let path = docs_dir().join("daemon.example.toml");
        let content = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("读取 {} 失败: {}", path.display(), e));

        // 必须包含 observability.langfuse 配置段
        assert!(
            content.contains("[observability.langfuse]"),
            "daemon.example.toml 缺少 [observability.langfuse] 段"
        );

        // 必须包含关键配置字段
        let expected_fields = [
            "enabled",
            "user_id",
            "tags",
            "environment",
            "release",
            "version",
            "public",
            "metadata",
            "capture",
        ];
        for field in &expected_fields {
            assert!(
                content.contains(field),
                "daemon.example.toml langfuse 配置缺少字段: {}",
                field
            );
        }
    }

    #[test]
    fn test_example_daemon_toml_collector_disclaimer() {
        let path = docs_dir().join("daemon.example.toml");
        let content = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("读取 {} 失败: {}", path.display(), e));

        // 必须声明 Collector 示例是参考配置
        let keywords = ["Collector", "参考", "示例", "管理"];
        let found = keywords.iter().any(|k| content.contains(k));
        assert!(
            found,
            "daemon.example.toml 未声明 Collector 示例为参考配置（缺少 Collector/参考/示例/管理 关键词）"
        );
    }

    #[test]
    fn test_builtin_agent_config_parsing() {
        let toml = r#"
[daemon]
listen_addr = "[::1]:50051"

[llm]
[[llm.models]]
name = "test"
protocol = "openai"
model = "gpt-4"

[tools]

[agent]
soft_limit = 50

[[agent.builtin]]
name = "explorer"
model = "Opencode/deepseek-v4-flash"
temperature = 0.1

[[agent.builtin]]
name = "fixer"
model = "Anthropic/claude-sonnet-4-20250514"
temperature = 0.2
steps = 30
"#;
        let config: DaemonConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.agent.builtin.len(), 2);

        let explorer = &config.agent.builtin[0];
        assert_eq!(explorer.name, "explorer");
        assert_eq!(
            explorer.model.as_deref(),
            Some("Opencode/deepseek-v4-flash")
        );
        assert!((explorer.temperature.unwrap() - 0.1).abs() < f32::EPSILON);
        assert!(explorer.steps.is_none());

        let fixer = &config.agent.builtin[1];
        assert_eq!(fixer.name, "fixer");
        assert_eq!(
            fixer.model.as_deref(),
            Some("Anthropic/claude-sonnet-4-20250514")
        );
        assert_eq!(fixer.steps, Some(30));
    }

    #[test]
    fn test_builtin_agent_config_defaults_empty() {
        let toml = r#"
[daemon]
listen_addr = "[::1]:50051"

[llm]
[[llm.models]]
name = "test"
protocol = "openai"
model = "gpt-4"

[tools]

[agent]
soft_limit = 50
"#;
        let config: DaemonConfig = toml::from_str(toml).unwrap();
        assert!(config.agent.builtin.is_empty());
    }

    // ── resolve_default_key ────────────────────────────────────

    fn make_model_configs() -> Vec<LlmModelConfig> {
        vec![
            LlmModelConfig {
                name: "ModelA".into(),
                protocol: "anthropic".into(),
                provider: Some("ProviderA".into()),
                model: "model-a-api".into(),
                api_key: None,
                base_url: None,
                temperature: None,
                max_tokens: None,
                max_context_tokens: None,
                thinking_budget_tokens: None,
                extra: Default::default(),
            },
            LlmModelConfig {
                name: "ModelB".into(),
                protocol: "openai".into(),
                provider: Some("ProviderB".into()),
                model: "model-b-api".into(),
                api_key: None,
                base_url: None,
                temperature: None,
                max_tokens: None,
                max_context_tokens: None,
                thinking_budget_tokens: None,
                extra: Default::default(),
            },
        ]
    }

    fn empty_llm_section() -> LlmSection {
        LlmSection {
            thinking_budget_tokens: None,
            extra: std::collections::HashMap::new(),
            models: vec![],
            default: None,
        }
    }

    #[test]
    fn test_resolve_default_key_matches_default() {
        let models = make_model_configs();
        let section = LlmSection {
            default: Some("ProviderB/ModelB".into()),
            ..empty_llm_section()
        };
        assert_eq!(section.resolve_default_key(&models), "ProviderB/ModelB");
    }

    #[test]
    fn test_resolve_default_key_not_found_falls_back_to_first() {
        let models = make_model_configs();
        let section = LlmSection {
            default: Some("Unknown/Model".into()),
            ..empty_llm_section()
        };
        // default 不匹配任何模型，回退到第一个
        assert_eq!(section.resolve_default_key(&models), "ProviderA/ModelA");
    }

    #[test]
    fn test_resolve_default_key_none_falls_back_to_first() {
        let models = make_model_configs();
        let section = LlmSection {
            default: None,
            ..empty_llm_section()
        };
        assert_eq!(section.resolve_default_key(&models), "ProviderA/ModelA");
    }

    #[test]
    fn test_resolve_default_key_empty_models() {
        let section = LlmSection {
            default: None,
            ..empty_llm_section()
        };
        assert_eq!(section.resolve_default_key(&[]), "default");
    }

    // ── model_alias / matches_key ───────────────────────────────

    #[test]
    fn test_model_alias_format() {
        let mc = LlmModelConfig {
            name: "Display Name".into(),
            protocol: "anthropic".into(),
            provider: Some("ProviderX".into()),
            model: "api-model-v1".into(),
            ..make_model_configs()[0].clone()
        };
        assert_eq!(mc.model_alias(), "ProviderX/api-model-v1");
    }

    #[test]
    fn test_model_alias_falls_back_to_protocol() {
        let mc = LlmModelConfig {
            name: "Display Name".into(),
            protocol: "anthropic".into(),
            provider: None,
            model: "api-model-v1".into(),
            ..make_model_configs()[0].clone()
        };
        assert_eq!(mc.model_alias(), "anthropic/api-model-v1");
    }

    #[test]
    fn test_matches_key_by_key() {
        let models = make_model_configs();
        assert!(models[0].matches_key("ProviderA/ModelA"));
    }

    #[test]
    fn test_matches_key_by_model_alias() {
        let models = make_model_configs();
        assert!(models[0].matches_key("ProviderA/model-a-api"));
    }

    #[test]
    fn test_matches_key_no_match() {
        let models = make_model_configs();
        assert!(!models[0].matches_key("Unknown/Model"));
    }

    // ── resolve_default_key with model_alias ────────────────────

    #[test]
    fn test_resolve_default_key_matches_model_alias() {
        let models = make_model_configs();
        let section = LlmSection {
            default: Some("ProviderB/model-b-api".into()),
            ..empty_llm_section()
        };
        // 使用 {provider}/{model} 格式也能匹配
        assert_eq!(section.resolve_default_key(&models), "ProviderB/ModelB");
    }
}
