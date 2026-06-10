//! MCP 服务器管理器
//!
//! 管理多个 MCP 服务器连接的完整生命周期：启动、连接、工具发现、自动重连、关闭。
//! 每个服务器在独立的 tokio task 中运行，连接成功后通过回调注册工具到 ToolRegistry。

use std::collections::HashMap;
use std::sync::Arc;

use std::sync::Mutex as StdMutex;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing;

use visp_core::tool::Tool;

use crate::client::McpSession;
use crate::config::McpServerConfig;
use crate::tool::create_tool_adapters;

/// 工具就绪回调（当 MCP 服务器的工具列表可用时被调用）
///
/// 参数一：服务器名称，参数二：该服务器的所有工具适配器。
pub type OnToolsReady = Arc<dyn Fn(&str, Vec<Box<dyn Tool>>) + Send + Sync>;

/// MCP 服务器管理器
///
/// 负责启动/停止多个 MCP 服务器连接，自动处理重连。
pub struct McpManager {
    /// 服务器配置列表
    configs: Vec<McpServerConfig>,
    /// 运行中的会话（name → McpSession）
    sessions: Mutex<HashMap<String, Arc<Mutex<McpSession>>>>,
    /// 后台任务句柄（name → JoinHandle）
    tasks: StdMutex<HashMap<String, JoinHandle<()>>>,
    /// 工具就绪回调（保存用于重启/重连时使用）
    on_ready: StdMutex<Option<OnToolsReady>>,
}

impl std::fmt::Debug for McpManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpManager")
            .field("config_count", &self.configs.len())
            .field("session_count", &self.sessions.blocking_lock().len())
            .finish()
    }
}

impl McpManager {
    /// 创建新的 MCP 管理器
    pub fn new(configs: Vec<McpServerConfig>) -> Self {
        Self {
            configs,
            sessions: Mutex::new(HashMap::new()),
            tasks: StdMutex::new(HashMap::new()),
            on_ready: StdMutex::new(None),
        }
    }

    /// 启动所有启用的 MCP 服务器
    ///
    /// 每个服务器在独立的后台 task 中初始化连接，连接成功后通过 `on_ready` 回调
    /// 将工具注册到 ToolRegistry。
    ///
    /// 此方法不阻塞——服务器连接在后台异步进行。
    pub async fn start_all(self: &Arc<Self>, on_ready: OnToolsReady) {
        // 保存回调供后续重启/重连使用
        *self.on_ready.lock().unwrap() = Some(on_ready.clone());

        let enabled_configs: Vec<&McpServerConfig> =
            self.configs.iter().filter(|c| c.enabled).collect();

        if enabled_configs.is_empty() {
            tracing::info!("no MCP servers configured, skipping MCP initialization");
            return;
        }

        tracing::info!(count = enabled_configs.len(), "starting MCP servers");

        for config in enabled_configs {
            let name = config.name.clone();
            let task_name = name.clone(); // clone for task insertion
            let config = config.clone();
            let self_arc = self.clone();
            let on_ready = on_ready.clone();

            let handle = tokio::spawn(async move {
                self_arc.connect_server(name, config, on_ready).await;
            });

            self.tasks.lock().unwrap().insert(task_name, handle);
        }
    }

    /// 连接单个 MCP 服务器（循环尝试连接 + 重连）
    async fn connect_server(&self, name: String, config: McpServerConfig, on_ready: OnToolsReady) {
        let max_retries = 3u32;
        let mut retry_count = 0u32;

        loop {
            tracing::info!(server = %name, "connecting to MCP server");

            // 创建新会话
            let mut session = McpSession::new(&config);

            // 建立连接
            match session.connect().await {
                Ok(()) => {
                    tracing::info!(server = %name, "MCP server connected");
                    retry_count = 0;

                    // 发现工具
                    match session.list_tools().await {
                        Ok(tools) => {
                            tracing::info!(
                                server = %name,
                                tool_count = tools.len(),
                                "MCP tools discovered"
                            );

                            let session = Arc::new(Mutex::new(session));

                            // 保存到 sessions 表
                            self.sessions
                                .lock()
                                .await
                                .insert(name.clone(), session.clone());

                            // 创建工具适配器并回调
                            let adapters = create_tool_adapters(
                                &tools,
                                config.tool_prefix.as_deref(),
                                config.tool_timeout_secs,
                                session,
                                &name,
                            );

                            if !adapters.is_empty() {
                                tracing::info!(
                                    server = %name,
                                    count = adapters.len(),
                                    "registering MCP tools"
                                );
                                on_ready(&name, adapters);
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                server = %name,
                                error = %e,
                                "failed to list MCP tools, will retry"
                            );
                        }
                    }

                    // 等待断开连接（目前通过 session 被 drop 或错误检测）
                    // 由于我们没有连接监控机制，暂时简单地保持 task 存活
                    // 未来可以加入 child.wait() 或 ping 监控来检测断开
                    loop {
                        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                        // 简单检查：如果 session 不再 connected，触发重连
                        let disconnected = {
                            let sessions = self.sessions.lock().await;
                            let mut disconnected = true;
                            if let Some(s) = sessions.get(&name) {
                                disconnected = !s.lock().await.is_connected();
                            }
                            disconnected
                        };
                        if disconnected {
                            tracing::warn!(server = %name, "MCP server disconnected, reconnecting...");
                            // 从 sessions 中移除已断开的 session
                            self.sessions.lock().await.remove(&name);
                            break;
                        }
                    }
                }
                Err(e) => {
                    retry_count += 1;
                    tracing::warn!(
                        server = %name,
                        error = %e,
                        retry = retry_count,
                        max_retries = max_retries,
                        "MCP server connection failed"
                    );

                    if retry_count >= max_retries {
                        tracing::error!(
                            server = %name,
                            "MCP server failed after {max_retries} retries, giving up"
                        );
                        return;
                    }

                    // 指数退避重试
                    let delay = std::time::Duration::from_secs(1u64 << retry_count);
                    tracing::info!(server = %name, delay_ms = delay.as_millis(), "retrying connection");
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }

    /// 重启指定名称的 MCP 服务器
    ///
    /// 移除旧会话 → 重建连接 → 重新发现工具 → 通过已保存的 `on_ready` 回调注册工具。
    /// 重启期间该服务器的工具短暂不可用。
    ///
    /// 如果服务器不存在或未启用，返回 Err。
    pub async fn restart(self: &Arc<Self>, name: &str) -> Result<(), String> {
        // 查找配置
        let config = self
            .configs
            .iter()
            .find(|c| c.name == name)
            .ok_or_else(|| format!("MCP server '{name}' not found in config"))?;

        if !config.enabled {
            return Err(format!("MCP server '{name}' is disabled"));
        }

        tracing::info!(server = %name, "restarting MCP server");

        // 关闭现有会话
        self.shutdown(name).await;

        // 获取保存的回调
        let on_ready =
            self.on_ready.lock().unwrap().clone().ok_or_else(|| {
                "McpManager not started, no on_ready callback registered".to_string()
            })?;

        let config = config.clone();
        let name = name.to_string();
        let task_name = name.clone();
        let self_arc = self.clone();

        let handle = tokio::spawn(async move {
            self_arc.connect_server(name, config, on_ready).await;
        });

        self.tasks.lock().unwrap().insert(task_name, handle);

        Ok(())
    }
    pub async fn shutdown(&self, name: &str) {
        tracing::info!(server = %name, "shutting down MCP server");

        // 移除并关闭会话
        if let Some(session) = self.sessions.lock().await.remove(name) {
            let mut sess = session.lock().await;
            sess.shutdown().await;
        }

        // 取消后台 task
        if let Some(handle) = self.tasks.lock().unwrap().remove(name) {
            handle.abort();
        }
    }

    /// 关闭所有 MCP 服务器
    pub async fn shutdown_all(&self) {
        let names: Vec<String> = {
            let sessions = self.sessions.lock().await;
            sessions.keys().cloned().collect()
        };

        if names.is_empty() {
            tracing::info!("no active MCP sessions to shut down");
            return;
        }

        tracing::info!(count = names.len(), "shutting down all MCP servers");

        for name in &names {
            self.shutdown(name).await;
        }
    }

    /// 获取已连接的服务器名称列表
    pub async fn connected_servers(&self) -> Vec<String> {
        let sessions = self.sessions.lock().await;
        let mut result = Vec::new();
        for (name, s) in sessions.iter() {
            if s.lock().await.is_connected() {
                result.push(name.clone());
            }
        }
        result.sort();
        result
    }

    /// 获取指定服务器的连接状态
    pub async fn is_connected(&self, name: &str) -> bool {
        let sessions = self.sessions.lock().await;
        if let Some(s) = sessions.get(name) {
            s.lock().await.is_connected()
        } else {
            false
        }
    }
}

impl Drop for McpManager {
    fn drop(&mut self) {
        // 中止所有后台 task，不等待（Drop 中不能 await）
        let tasks = self.tasks.get_mut().unwrap();
        for (name, handle) in tasks.iter() {
            handle.abort();
            tracing::debug!(server = %name, "aborted MCP server task on drop");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::McpTransport;
    use std::collections::HashMap;

    fn test_config(name: &str) -> McpServerConfig {
        McpServerConfig {
            name: name.to_string(),
            transport: McpTransport::Stdio {
                command: "echo".into(),
                args: vec![],
                env: HashMap::new(),
            },
            enabled: true,
            tool_prefix: None,
            tool_timeout_secs: 60,
        }
    }

    #[tokio::test]
    async fn test_manager_new_empty() {
        let mgr = McpManager::new(vec![]);
        assert!(mgr.connected_servers().await.is_empty());
    }

    #[tokio::test]
    async fn test_manager_new_with_configs() {
        let configs = vec![test_config("server1"), test_config("server2")];
        let mgr = McpManager::new(configs);
        assert!(mgr.connected_servers().await.is_empty());
    }

    #[tokio::test]
    async fn test_manager_connect_and_shutdown() {
        let configs = vec![test_config("echo-server")];
        let mgr = Arc::new(McpManager::new(configs));

        let tool_collector = Arc::new(Mutex::new(Vec::new()));
        let collector = tool_collector.clone();

        let on_ready: OnToolsReady = Arc::new(move |_server_name, tools| {
            let mut collected = collector.blocking_lock();
            for t in tools {
                collected.push(t.name().to_string());
            }
        });

        // 启动所有服务器（echo 不是有效的 MCP 服务器，连接会失败）
        mgr.start_all(on_ready).await;

        // 等待连接尝试
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // echo 无法作为 MCP 服务器，所以应该没有连接成功
        assert!(!mgr.is_connected("echo-server").await);

        // 关闭
        mgr.shutdown_all().await;
        assert!(mgr.connected_servers().await.is_empty());
    }

    #[tokio::test]
    async fn test_manager_double_shutdown() {
        let configs = vec![test_config("test")];
        let mgr = Arc::new(McpManager::new(configs));

        let on_ready: OnToolsReady = Arc::new(|_server_name, _| {});

        mgr.start_all(on_ready).await;
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        // 调用两次 shutdown_all 不影响
        mgr.shutdown_all().await;
        mgr.shutdown_all().await;
    }

    #[tokio::test]
    async fn test_manager_shutdown_nonexistent() {
        let mgr = McpManager::new(vec![]);
        mgr.shutdown("nonexistent").await; // 不应 panic
    }

    #[tokio::test]
    async fn test_manager_disabled_server() {
        let config = McpServerConfig {
            name: "disabled-server".into(),
            transport: McpTransport::Stdio {
                command: "echo".into(),
                args: vec![],
                env: HashMap::new(),
            },
            enabled: false, // 禁用的服务器不应启动
            tool_prefix: None,
            tool_timeout_secs: 60,
        };
        let mgr = Arc::new(McpManager::new(vec![config]));

        let started = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let started_clone = started.clone();
        let on_ready: OnToolsReady = Arc::new(move |_server_name, _| {
            started_clone.store(true, std::sync::atomic::Ordering::SeqCst);
        });

        mgr.start_all(on_ready).await;
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        assert!(!started.load(std::sync::atomic::Ordering::SeqCst));
        assert!(mgr.connected_servers().await.is_empty());
    }

    #[tokio::test]
    async fn test_manager_connected_servers_list() {
        let mgr = McpManager::new(vec![test_config("b"), test_config("a")]);

        // 尚未连接
        let names = mgr.connected_servers().await;
        assert!(names.is_empty());
    }

    // ── restart tests ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_manager_restart_nonexistent() {
        let mgr = McpManager::new(vec![]);
        let mgr = Arc::new(mgr);
        let result = mgr.restart("nonexistent").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_manager_restart_disabled() {
        let config = McpServerConfig {
            name: "disabled".into(),
            transport: McpTransport::Stdio {
                command: "echo".into(),
                args: vec![],
                env: HashMap::new(),
            },
            enabled: false,
            tool_prefix: None,
            tool_timeout_secs: 60,
        };
        let mgr = Arc::new(McpManager::new(vec![config]));
        let result = mgr.restart("disabled").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_manager_restart_without_start() {
        let mgr = Arc::new(McpManager::new(vec![test_config("test-server")]));
        // restart 需要在 start_all 之后调用（保存 on_ready 回调）
        let result = mgr.restart("test-server").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("on_ready callback registered"));
    }

    #[tokio::test]
    async fn test_manager_restart_creates_new_task() {
        let mgr = Arc::new(McpManager::new(vec![test_config("test-server")]));

        let on_ready: OnToolsReady = Arc::new(|_server_name, _| {});
        mgr.start_all(on_ready).await;
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        // 重启
        let result = mgr.restart("test-server").await;
        assert!(result.is_ok(), "restart should succeed: {:?}", result);
    }

    #[tokio::test]
    async fn test_manager_restart_isolated_per_server() {
        let configs = vec![test_config("server-a"), test_config("server-b")];
        let mgr = Arc::new(McpManager::new(configs));

        let started = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let started_clone = started.clone();
        let on_ready: OnToolsReady = Arc::new(move |_sn, _| {
            started_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });

        mgr.start_all(on_ready).await;
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        // 重启 server-a 不应影响 server-b
        let _ = mgr.restart("server-a").await;
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // server-b 的 task 应该还活着
        assert!(mgr.tasks.lock().unwrap().contains_key("server-b"));
    }
}
