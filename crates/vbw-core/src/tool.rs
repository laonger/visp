use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 工具执行上下文
#[derive(Debug, Clone)]
pub struct ToolContext {
    /// 当前工作目录
    pub working_dir: PathBuf,
}

/// 工具执行结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    /// 结果内容（成功时为输出，失败时为错误描述）
    pub content: String,
    /// 是否为错误
    pub is_error: bool,
}

impl ToolResult {
    pub fn success(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self {
            content: message.into(),
            is_error: true,
        }
    }
}

/// 工具抽象 trait
/// 所有工具（内置 + MCP）都必须实现此 trait
#[async_trait]
pub trait Tool: Send + Sync {
    /// 工具名称
    fn name(&self) -> &str;

    /// 工具描述（给 LLM 看的说明）
    fn description(&self) -> &str;

    /// 参数定义（JSON Schema 格式，用于 LLM function calling）
    fn parameters(&self) -> serde_json::Value;

    /// 执行工具
    async fn execute(&self, arguments: serde_json::Value, context: &ToolContext) -> ToolResult;
}
