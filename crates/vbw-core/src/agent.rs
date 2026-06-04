/// Agent 执行配置
#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// 最大迭代轮数
    pub max_iterations: u32,
    /// LLM 调用重试次数
    pub llm_retry_attempts: u32,
    /// LLM 重试基础延迟（毫秒）
    pub llm_retry_base_delay_ms: u64,
    /// bash 工具确认模式（执行高危命令前是否需要用户确认）
    pub bash_confirm_mode: bool,
    /// 文件读取/写入的最大字节数
    pub file_max_size_bytes: u64,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_iterations: 50,
            llm_retry_attempts: 3,
            llm_retry_base_delay_ms: 1000,
            bash_confirm_mode: true,
            file_max_size_bytes: 1048576,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_config_default() {
        let cfg = AgentConfig::default();
        assert_eq!(cfg.max_iterations, 50);
        assert_eq!(cfg.llm_retry_attempts, 3);
        assert_eq!(cfg.llm_retry_base_delay_ms, 1000);
        assert_eq!(cfg.bash_confirm_mode, true);
        assert_eq!(cfg.file_max_size_bytes, 1048576);
    }
}
