//! 优雅关停管道（设计 D13）。
//!
//! 三个触发源——Ctrl+C、Shutdown RPC 经 [`Notify`] 的唤醒、SIGTERM——汇入同一
//! 清理路径：停 watcher → 关 MCP → abort gRPC server。
//!
//! hook 侧的**有界 drain** 由可注入的 [`HookDrainHost`] 承接：有 hook 规则时为真实
//! 执行器（`main` 启动接线），零规则时用 [`NoopHookDrain`]（无执行器）。

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use visp_daemon::watch::FileWatcher;
use visp_hooks::HookEvent;
use visp_mcp::manager::McpManager;

/// hook drain 的硬上限（设计 D13：≤2s）。
///
/// 宿主与调用方都以此时长为界：宿主应尽力在此内返回，调用方另加同长硬超时
/// 兜底，宿主即使超时也不会拖住关停。
pub const HOOK_DRAIN_BUDGET: Duration = Duration::from_secs(2);

// hook drain 契约迁入库层 `hook_runtime`（执行器在库层实现该契约），在此重导出以
// 保持既有引用（`service.rs`/`main.rs` 的 `crate::shutdown::HookDrainHost`）不变。
pub use visp_daemon::hook_runtime::HookDrainHost;

/// 默认空实现：无 hook 执行器时立即返回，关停行为与旧版一致。
pub struct NoopHookDrain;

#[async_trait]
impl HookDrainHost for NoopHookDrain {
    async fn drain(&self, _budget: Duration) {}

    async fn emit_terminal(&self, _event: HookEvent) {}
}

/// 关停触发源；三者走同一清理路径（设计 D13）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownTrigger {
    /// 终端 Ctrl+C（SIGINT）。
    CtrlC,
    /// Shutdown RPC 经 [`Notify`] 唤醒 daemon main。
    Rpc,
    /// SIGTERM（Unix）。
    Sigterm,
}

/// 等待任一关停触发源。
///
/// 生产路径由 daemon `main` 调用；返回后调用方执行
/// [`run_shutdown_cleanup`]，三入口共用同一清理。
#[cfg(unix)]
pub async fn wait_for_shutdown(notify: &Notify) -> std::io::Result<ShutdownTrigger> {
    use tokio::signal::unix::{SignalKind, signal};

    let mut sigterm = signal(SignalKind::terminate())?;
    Ok(tokio::select! {
        _ = tokio::signal::ctrl_c() => ShutdownTrigger::CtrlC,
        _ = notify.notified() => ShutdownTrigger::Rpc,
        _ = sigterm.recv() => ShutdownTrigger::Sigterm,
    })
}

/// 等待任一关停触发源（非 Unix：无 SIGTERM）。
#[cfg(not(unix))]
pub async fn wait_for_shutdown(notify: &Notify) -> std::io::Result<ShutdownTrigger> {
    Ok(tokio::select! {
        _ = tokio::signal::ctrl_c() => ShutdownTrigger::CtrlC,
        _ = notify.notified() => ShutdownTrigger::Rpc,
    })
}

/// 进程级关停清理（设计 D13）：停 watcher → 关 MCP → abort 服务端任务。
///
/// 三入口（Ctrl+C / Shutdown RPC / SIGTERM）共用，避免重复实现。
/// `mcp.shutdown_all` 幂等（`manager.rs`），与 Shutdown RPC 内的调用双调无副作用。
pub async fn run_shutdown_cleanup(
    watcher: Option<FileWatcher>,
    mcp: Arc<McpManager>,
    server: &JoinHandle<()>,
) {
    // 先停 watcher：关闭路径上不再有 reload 触发（设计 §5.6 生命周期）。
    // stop 为尽力而为（abort 后台任务 + 释放监听），不阻断后续 shutdown。
    if let Some(watcher) = watcher {
        watcher.stop();
    }

    // Gracefully shut down MCP connections before aborting the server.
    mcp.shutdown_all().await;

    server.abort();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    /// 等 abort 生效（`abort` 是异步的，任务须被 runtime 轮询后才标记完成）。
    async fn wait_aborted(handle: &JoinHandle<()>) {
        let deadline = Instant::now() + Duration::from_secs(1);
        while !handle.is_finished() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }

    /// 1. Notify 唤醒走 Rpc 触发源。
    #[tokio::test]
    async fn notify_wakes_shutdown_path() {
        let notify = Arc::new(Notify::new());
        let notify2 = notify.clone();
        let task = tokio::spawn(async move { wait_for_shutdown(&notify2).await });
        notify.notify_one();

        let trigger = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("wait_for_shutdown should wake on notify")
            .unwrap()
            .unwrap();
        assert_eq!(trigger, ShutdownTrigger::Rpc);
    }

    /// 2. SIGTERM 与 Ctrl+C 汇入同一 `wait_for_shutdown`（同清理路径入口）。
    #[cfg(unix)]
    #[serial_test::serial]
    #[tokio::test]
    async fn sigterm_uses_same_shutdown_path() {
        use tokio::signal::unix::{SignalKind, signal};

        // 先安装处理器：既让 wait_for_shutdown 可订阅，也避免信号默认终止测试进程。
        let mut guard = signal(SignalKind::terminate()).unwrap();
        let notify = Notify::new();
        let task = tokio::spawn(async move { wait_for_shutdown(&notify).await });

        // 等待订阅方就绪后触发 SIGTERM。测试进程 handler 已就绪，提前 raise 也安全。
        let deadline = Instant::now() + Duration::from_secs(2);
        while !task.is_finished() && Instant::now() < deadline {
            // SAFETY: `raise` 仅向当前进程发送 SIGTERM，handler 已安装。
            unsafe { libc::raise(libc::SIGTERM) };
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        let trigger = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("wait_for_shutdown should wake on SIGTERM")
            .unwrap()
            .unwrap();
        assert_eq!(trigger, ShutdownTrigger::Sigterm);
        assert!(guard.recv().await.is_some(), "SIGTERM 应实际到达进程");
    }

    /// 3. 关停清理三步：abort server 任务（watcher 为 None、MCP 无会话时幂等）。
    #[tokio::test]
    async fn cleanup_aborts_server_task() {
        let server = tokio::spawn(async {
            std::future::pending::<()>().await;
        });
        let mcp = Arc::new(McpManager::new(vec![]));

        run_shutdown_cleanup(None, mcp, &server).await;
        wait_aborted(&server).await;

        assert!(server.is_finished(), "server task 应被 abort");
    }

    /// 4. Notify 唤醒 + 清理三步一体：main 的关停路径可被 Notify 驱动并跑完清理。
    #[tokio::test]
    async fn notify_drives_cleanup_pipeline() {
        let notify = Arc::new(Notify::new());
        let server = tokio::spawn(async {
            std::future::pending::<()>().await;
        });
        let mcp = Arc::new(McpManager::new(vec![]));
        let notify2 = notify.clone();

        let pipeline = tokio::spawn(async move {
            let trigger = wait_for_shutdown(&notify2).await.unwrap();
            run_shutdown_cleanup(None, mcp, &server).await;
            wait_aborted(&server).await;
            (trigger, server.is_finished())
        });

        notify.notify_one();

        let (trigger, finished) = tokio::time::timeout(Duration::from_secs(2), pipeline)
            .await
            .expect("shutdown pipeline should complete")
            .unwrap();
        assert_eq!(trigger, ShutdownTrigger::Rpc);
        assert!(finished, "清理须 abort server 任务");
    }

    /// 5. NoopHookDrain 立即返回（无 hook 时关停行为不变）。
    #[tokio::test]
    async fn noop_drain_returns_immediately() {
        let started = Instant::now();
        time_budget_ok(&NoopHookDrain).await;
        assert!(started.elapsed() < HOOK_DRAIN_BUDGET);
    }

    async fn time_budget_ok(host: &dyn HookDrainHost) {
        host.drain(HOOK_DRAIN_BUDGET).await;
    }
}
