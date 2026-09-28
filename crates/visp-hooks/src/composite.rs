//! 复合 [`Handler`]：按 `rule.id` 把分发路由到内置绑定或脚本执行器（设计 D8）。
//!
//! 内置规则以 `builtin:` 为保留前缀（见 [`BUILTIN_RULE_PREFIX`]）。路由规则：
//!
//! - `rule.id` 命中已注册的内置绑定 → 交给该绑定（进程内，如 herdr）。
//! - `rule.id` 以 `builtin:` 开头但未注册 → 静默跳过（不误落脚本路径）。
//! - 其余 → 交给脚本 [`Handler`]（如 [`crate::SpawnHandler`]）。
//!
//! 这样内置消费者与脚本规则**共用同一 executor 接口**（[`Handler::run`]），执行器无需
//! 知道规则是进程内还是子进程。

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;

use crate::dispatcher::{DispatchInput, DispatchRule, Handler};

/// 内置规则的保留 `id` 前缀。
pub const BUILTIN_RULE_PREFIX: &str = "builtin:";

/// 按 `rule.id` 路由的复合 Handler。
pub struct CompositeHandler {
    scripts: Arc<dyn Handler>,
    builtins: HashMap<String, Arc<dyn Handler>>,
}

impl CompositeHandler {
    /// 以脚本 Handler 与内置绑定表（`id` → 绑定）构造。
    pub fn new(scripts: Arc<dyn Handler>, builtins: HashMap<String, Arc<dyn Handler>>) -> Self {
        Self { scripts, builtins }
    }

    /// 注册一个内置绑定（链式）；`id` 须为完整规则 id（如 `builtin:herdr`）。
    pub fn with_builtin(mut self, id: impl Into<String>, handler: Arc<dyn Handler>) -> Self {
        self.builtins.insert(id.into(), handler);
        self
    }
}

#[async_trait]
impl Handler for CompositeHandler {
    async fn run(&self, rule: &DispatchRule, event: &DispatchInput) {
        if let Some(builtin) = self.builtins.get(&rule.id) {
            builtin.run(rule, event).await;
        } else if rule.id.starts_with(BUILTIN_RULE_PREFIX) {
            tracing::debug!(rule_id = %rule.id, "未注册的内置 hook 绑定，已跳过");
        } else {
            self.scripts.run(rule, event).await;
        }
    }
}
