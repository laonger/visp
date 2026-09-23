//! daemon 编排器:端口探测、spawn、health check(含子进程存活监控)、按模式退出收尾。
//!
//! 逻辑移植自 launcher(`crates/visp/src/main.rs`);按设计 §4.4 不抽公共 crate。
//! 关键修订(设计 §6.1 三轮):health check 期间监控子进程存活(TOCTOU 防御)、
//! 退出收尾按启动模式区分(直连默认不 Shutdown)。

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail};
use tokio::process::{Child, Command};
use tonic::transport::Endpoint;
use visp_proto::visp::ShutdownRequest;
use visp_proto::visp::coder_daemon_client::CoderDaemonClient;

/// 自拉起探测的最大递增端口数。
const MAX_PORT_ATTEMPTS: u16 = 1000;

/// 端口探测基准地址(`--addr` 未指定时)。
pub const DEFAULT_LISTEN_ADDR: &str = "[::1]:50051";

// ===== 3a 端口探测 =====

/// 解析地址("[::1]:50051" / "127.0.0.1:9090" / "host:port")为 (host, port)。
pub fn parse_addr(addr: &str) -> anyhow::Result<(String, u16)> {
    // 方括号 IPv6:[::1]:50051
    if let Some(rest) = addr.strip_prefix('[') {
        let bracket_end = rest
            .find(']')
            .ok_or_else(|| anyhow!("invalid address (missing ']'): {addr}"))?;
        let host = &rest[..bracket_end];
        let after = &rest[bracket_end + 1..];
        let port_str = after
            .strip_prefix(':')
            .ok_or_else(|| anyhow!("invalid address (no port after ']'): {addr}"))?;
        let port: u16 = port_str
            .parse()
            .map_err(|_| anyhow!("invalid port in address: {addr}"))?;
        return Ok((host.to_string(), port));
    }
    // 普通 host:port
    let (host, port_str) = addr
        .rsplit_once(':')
        .ok_or_else(|| anyhow!("invalid address (no port): {addr}"))?;
    let port: u16 = port_str
        .parse()
        .map_err(|_| anyhow!("invalid port in address: {addr}"))?;
    Ok((host.to_string(), port))
}

/// 重组 host+port 为地址字符串(IPv6 host 加方括号)。
pub fn format_addr(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

/// 从基准地址开始探测空闲端口(试绑验证),占用则递增。
pub fn find_available_addr(base_addr: &str) -> anyhow::Result<String> {
    find_available_addr_from(base_addr, MAX_PORT_ATTEMPTS)
}

fn find_available_addr_from(base_addr: &str, max_attempts: u16) -> anyhow::Result<String> {
    let (host, base_port) = parse_addr(base_addr)?;
    for offset in 0..max_attempts {
        let port = base_port.saturating_add(offset);
        let addr_str = format_addr(&host, port);
        let sock_addr: SocketAddr = addr_str
            .parse()
            .map_err(|e| anyhow!("invalid address {addr_str}: {e}"))?;
        // 试绑验证:成功立即释放,真实绑定由 daemon 完成
        if std::net::TcpListener::bind(sock_addr).is_ok() {
            return Ok(addr_str);
        }
    }
    bail!("could not find an available port starting from {base_port} ({max_attempts} attempts)")
}

// ===== 3b spawn =====

/// 解析兄弟二进制路径(同目录优先,回退 PATH)。
pub fn resolve_bin(name: &str) -> PathBuf {
    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent()
    {
        let sibling = parent.join(name);
        if sibling.is_file() {
            return sibling;
        }
    }
    PathBuf::from(name)
}

/// daemon 日志文件路径(`{log_dir}/daemon-{timestamp}.log`)。
pub fn daemon_log_path() -> anyhow::Result<PathBuf> {
    let dir = visp_config::path::log_dir().unwrap_or_else(std::env::temp_dir);
    let timestamp = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S");
    Ok(dir.join(format!("daemon-{timestamp}.log")))
}

/// 启动 daemon:注入 `VISP_LISTEN_ADDR`,stdout/stderr 重定向到日志文件,
/// `--config-dir` 透传(额外参数用于测试注入假进程)。
pub async fn spawn_daemon(
    daemon_bin: &Path,
    extra_args: &[String],
    addr: &str,
    log_path: &Path,
    config_dir: Option<&Path>,
) -> anyhow::Result<Child> {
    let log_file = tokio::fs::File::create(log_path)
        .await
        .map_err(|e| anyhow!("failed to create log file {}: {e}", log_path.display()))?;
    let log_stdout = log_file.try_clone().await?;
    let log_stderr = log_file.try_clone().await?;

    let mut cmd = Command::new(daemon_bin);
    for arg in extra_args {
        cmd.arg(arg);
    }
    if let Some(dir) = config_dir {
        cmd.arg("--config-dir").arg(dir);
    }
    cmd.env("VISP_LISTEN_ADDR", addr)
        .stdout(log_stdout.into_std().await)
        .stderr(log_stderr.into_std().await);

    cmd.spawn()
        .map_err(|e| anyhow!("failed to start daemon {}: {e}", daemon_bin.display()))
}

// ===== 3c health check =====

/// 轮询 HealthCheck 直至就绪(带超时)。
///
/// **每轮先检查子进程存活**(`try_wait`):子进程已退出(典型为端口绑定失败,
/// 并发启动 TOCTOU)→ 立即报错并附带 `startup_error_file()` 诊断——
/// 不得继续对探测端口做 health check,否则会误连占用该端口的其他 daemon 实例。
pub async fn wait_for_health_ready(
    child: &mut Child,
    addr: &str,
    timeout: Duration,
) -> anyhow::Result<()> {
    let endpoint = format!("http://{addr}");
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        // TOCTOU 防御:子进程已退出 → 立即失败,不继续探测
        if let Some(status) = child.try_wait()? {
            let diag = take_startup_error().unwrap_or_default();
            bail!(
                "daemon exited early (status: {status}){}",
                if diag.is_empty() {
                    String::new()
                } else {
                    format!("; startup error: {diag}")
                }
            );
        }
        if tokio::time::Instant::now() >= deadline {
            bail!("daemon did not become ready within {timeout:?}");
        }
        if matches!(connect_and_check(&endpoint).await, Ok(true)) {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn connect_and_check(endpoint: &str) -> anyhow::Result<bool> {
    let ch = Endpoint::new(endpoint.to_string())
        .map_err(|e| anyhow!("endpoint: {e}"))?
        .connect()
        .await
        .map_err(|e| anyhow!("connect: {e}"))?;
    let mut client = CoderDaemonClient::new(ch);
    let resp = client
        .health_check(())
        .await
        .map_err(|e| anyhow!("health: {e}"))?;
    Ok(resp.into_inner().alive)
}

/// 读取并删除 daemon 启动错误文件(存在且非空时)。
fn take_startup_error() -> Option<String> {
    let path = visp_config::path::startup_error_file()?;
    let msg = std::fs::read_to_string(&path).ok()?;
    if msg.is_empty() {
        return None;
    }
    let _ = std::fs::remove_file(&path);
    Some(msg)
}

// ===== 3d 退出收尾 =====

/// daemon 启动模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchMode {
    /// 本进程拉起(默认路径)
    Spawned,
    /// `--addr` 直连既有 daemon
    Attached,
}

/// 退出收尾策略(§6.1 步骤 6,按模式区分)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownPolicy {
    /// 发 Shutdown,超时强杀子进程(仅自拉起模式)
    SendThenKill,
    /// 发 Shutdown,但无子进程可杀(直连 + 显式 --shutdown-on-exit)
    SendOnly,
    /// 不发 Shutdown、不强杀(直连默认:daemon 可能正被其他客户端使用)
    Leave,
}

pub fn shutdown_policy(mode: LaunchMode, shutdown_on_exit: bool) -> ShutdownPolicy {
    match mode {
        LaunchMode::Spawned => ShutdownPolicy::SendThenKill,
        LaunchMode::Attached if shutdown_on_exit => ShutdownPolicy::SendOnly,
        LaunchMode::Attached => ShutdownPolicy::Leave,
    }
}

/// 执行退出收尾。`child` 仅自拉起模式持有;直连模式传 `None`。
pub async fn exit_daemon(
    child: Option<&mut Child>,
    addr: &str,
    policy: ShutdownPolicy,
) -> anyhow::Result<()> {
    match policy {
        ShutdownPolicy::Leave => Ok(()),
        ShutdownPolicy::SendOnly => send_shutdown(addr).await,
        ShutdownPolicy::SendThenKill => {
            send_shutdown(addr).await?;
            let Some(child) = child else {
                return Ok(());
            };
            // 优雅退出窗口;超时强杀
            match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
                Ok(_) => Ok(()),
                Err(_) => kill(child).await,
            }
        }
    }
}

/// 向 daemon 发送 Shutdown 请求。
pub async fn send_shutdown(addr: &str) -> anyhow::Result<()> {
    let ch = Endpoint::new(format!("http://{addr}"))
        .map_err(|e| anyhow!("endpoint: {e}"))?
        .connect()
        .await
        .map_err(|e| anyhow!("connect: {e}"))?;
    let mut client = CoderDaemonClient::new(ch);
    client
        .shutdown(ShutdownRequest { force: false })
        .await
        .map_err(|e| anyhow!("shutdown: {e}"))?;
    Ok(())
}

/// 强杀子进程并等待退出。
pub async fn kill(child: &mut Child) -> anyhow::Result<()> {
    let _ = child.kill().await;
    let _ = child.wait().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn parse_addr_ipv6_bracketed() {
        assert_eq!(parse_addr("[::1]:50051").unwrap(), ("::1".into(), 50051));
    }

    #[test]
    fn parse_addr_ipv4() {
        assert_eq!(
            parse_addr("127.0.0.1:9090").unwrap(),
            ("127.0.0.1".into(), 9090)
        );
    }

    #[test]
    fn parse_addr_rejects_invalid() {
        assert!(parse_addr("no-port").is_err());
        assert!(parse_addr("[::1]:notaport").is_err());
        assert!(parse_addr("[::1").is_err());
    }

    #[test]
    fn format_addr_brackets_ipv6_only() {
        assert_eq!(format_addr("::1", 50051), "[::1]:50051");
        assert_eq!(format_addr("127.0.0.1", 9090), "127.0.0.1:9090");
    }

    #[test]
    fn default_listen_addr_is_valid() {
        let (host, port) = parse_addr(DEFAULT_LISTEN_ADDR).unwrap();
        let sock: SocketAddr = format_addr(&host, port).parse().unwrap();
        assert_eq!(sock.ip(), Ipv6Addr::LOCALHOST);
        assert_eq!(sock.port(), 50051);
    }

    #[tokio::test]
    async fn find_available_addr_skips_occupied_port() {
        // 占住一个冷门端口,探测应从下一个端口找到
        let listener = std::net::TcpListener::bind("127.0.0.1:59918").unwrap();
        let base = listener.local_addr().unwrap().to_string();
        let found = find_available_addr_from(&base, 10).unwrap();
        let found: SocketAddr = found.parse().unwrap();
        assert_eq!(found.ip(), Ipv4Addr::LOCALHOST);
        assert_eq!(found.port(), 59919, "应跳过被占端口递增");
        drop(listener);
    }

    #[test]
    fn find_available_addr_gives_up_after_max_attempts() {
        // 0 次尝试:即使端口空闲也直接放弃(测试 Err 路径)
        assert!(find_available_addr_from("127.0.0.1:59930", 0).is_err());
    }

    #[tokio::test]
    async fn spawn_injects_env_and_redirects_output() {
        let dir = std::env::temp_dir().join(format!("visp-acp-sup-{}", std::process::id()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let log_path = dir.join("daemon-test.log");

        let mut child = spawn_daemon(
            Path::new("/bin/bash"),
            &["-c".into(), "echo VISP=$VISP_LISTEN_ADDR".into()],
            "127.0.0.1:59931",
            &log_path,
            None,
        )
        .await
        .unwrap();
        child.wait().await.unwrap();

        let log = tokio::fs::read_to_string(&log_path).await.unwrap();
        assert!(
            log.contains("VISP=127.0.0.1:59931"),
            "env 未注入或输出未重定向, log: {log}"
        );
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn health_check_fails_fast_when_child_exits() {
        let dir = std::env::temp_dir().join(format!("visp-acp-sup2-{}", std::process::id()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let log_path = dir.join("daemon-exit.log");
        // 立即退出的假 daemon(端口上无服务)
        let mut child = spawn_daemon(
            Path::new("/bin/bash"),
            &["-c".into(), "exit 1".into()],
            "127.0.0.1:59932",
            &log_path,
            None,
        )
        .await
        .unwrap();

        let start = std::time::Instant::now();
        let result =
            wait_for_health_ready(&mut child, "127.0.0.1:59932", Duration::from_secs(10)).await;
        let elapsed = start.elapsed();

        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("exited early"),
            "应因子进程退出快速失败, got: {err}"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "存活监控应立即失败而非等待 10s 超时, elapsed: {elapsed:?}"
        );
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn health_check_times_out_when_child_alive_but_no_service() {
        let dir = std::env::temp_dir().join(format!("visp-acp-sup3-{}", std::process::id()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let log_path = dir.join("daemon-sleep.log");
        // 活着但不监听的假 daemon
        let mut child = spawn_daemon(
            Path::new("/bin/bash"),
            &["-c".into(), "sleep 30".into()],
            "127.0.0.1:59933",
            &log_path,
            None,
        )
        .await
        .unwrap();

        let result =
            wait_for_health_ready(&mut child, "127.0.0.1:59933", Duration::from_millis(700)).await;
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("did not become ready"),
            "无服务时应以超时失败"
        );
        let _ = child.kill().await;
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[test]
    fn shutdown_policy_matrix() {
        assert_eq!(
            shutdown_policy(LaunchMode::Spawned, false),
            ShutdownPolicy::SendThenKill
        );
        assert_eq!(
            shutdown_policy(LaunchMode::Spawned, true),
            ShutdownPolicy::SendThenKill
        );
        assert_eq!(
            shutdown_policy(LaunchMode::Attached, false),
            ShutdownPolicy::Leave
        );
        assert_eq!(
            shutdown_policy(LaunchMode::Attached, true),
            ShutdownPolicy::SendOnly
        );
    }
}
