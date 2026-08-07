use std::path::PathBuf;

use clap::Parser;
use tokio::process::{Child, Command};
use tonic::transport::Endpoint;
use visp_proto::visp::ShutdownRequest;
use visp_proto::visp::coder_daemon_client::CoderDaemonClient;

/// visp launcher — starts daemon + CLI in one command
#[derive(Parser)]
#[command(name = "visp")]
struct Cli {
    #[arg(short = 'p', long, default_value = ".")]
    project: String,

    #[arg(short = 'a', long, default_value = "[::1]:50051")]
    addr: String,

    #[arg(long)]
    model: Option<String>,

    #[arg(long)]
    temperature: Option<f64>,

    #[arg(long)]
    thinking_budget: Option<u32>,

    #[arg(short = 's', long)]
    session: Option<String>,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // Resolve sibling binary paths (same dir as launcher for cargo run, or PATH)
    let daemon_bin = resolve_bin("visp-daemon");
    let cli_bin = resolve_bin("visp-cli");

    // 1. Create log directory
    let log_dir = visp_config::path::log_dir().unwrap_or_else(|| PathBuf::from("."));
    tokio::fs::create_dir_all(&log_dir)
        .await
        .unwrap_or_else(|e| {
            eprintln!("Failed to create log directory {}: {e}", log_dir.display());
            std::process::exit(1);
        });

    // 2. Prepare daemon log file
    let timestamp = format_timestamp();
    let log_path = log_dir.join(format!("daemon-{timestamp}.log"));
    let log_file = tokio::fs::File::create(&log_path)
        .await
        .unwrap_or_else(|e| {
            eprintln!("Failed to create log file {}: {e}", log_path.display());
            std::process::exit(1);
        });
    let log_file_stdout = log_file.try_clone().await.unwrap();
    let log_file_stderr = log_file.try_clone().await.unwrap();

    // 3. Find an available port for the daemon.
    let addr = find_available_addr(&cli.addr).unwrap_or_else(|e| {
        eprintln!("[visp] {e}");
        std::process::exit(1);
    });
    eprintln!("[visp] Daemon will listen on {addr}");

    // 4. Start daemon, passing the selected address via env var.
    eprintln!("[visp] Starting daemon (log: {})...", log_path.display());
    let mut daemon = match Command::new(&daemon_bin)
        .env("VISP_LISTEN_ADDR", &addr)
        .stdout(log_file_stdout.into_std().await)
        .stderr(log_file_stderr.into_std().await)
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            eprintln!("[visp] Failed to start daemon: {e}");
            eprintln!("[visp] Make sure 'visp-daemon' is built. Try: cargo build");
            std::process::exit(1);
        }
    };

    // 5. Health check with timeout
    eprintln!("[visp] Waiting for daemon to be ready...");
    let health_ok =
        tokio::time::timeout(std::time::Duration::from_secs(15), wait_for_health(&addr)).await;

    match health_ok {
        Ok(true) => eprintln!("[visp] Daemon is ready."),
        Ok(false) => {
            eprintln!("[visp] Daemon health check failed.");
            kill_daemon(&mut daemon).await;
            std::process::exit(1);
        }
        Err(_) => {
            eprintln!("[visp] Daemon did not become ready within 15s.");
            kill_daemon(&mut daemon).await;
            std::process::exit(1);
        }
    }

    // 6. Build CLI args
    let mut cli_args = vec![
        "visp-cli".to_string(),
        "--addr".to_string(),
        addr.clone(),
        "-p".to_string(),
        cli.project,
    ];
    if let Some(model) = &cli.model {
        cli_args.push("--model".to_string());
        cli_args.push(model.clone());
    }
    if let Some(temp) = cli.temperature {
        cli_args.push("--temperature".to_string());
        cli_args.push(temp.to_string());
    }
    if let Some(budget) = cli.thinking_budget {
        cli_args.push("--thinking-budget".to_string());
        cli_args.push(budget.to_string());
    }
    if let Some(session) = &cli.session {
        cli_args.push("--session".to_string());
        cli_args.push(session.clone());
    }

    // 7. Start CLI
    let mut cli_child = match Command::new(&cli_bin)
        .args(&cli_args[1..]) // skip the program name
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            eprintln!("[visp] Failed to start CLI: {e}");
            eprintln!("[visp] Make sure 'visp-cli' is installed.");
            kill_daemon(&mut daemon).await;
            std::process::exit(1);
        }
    };

    // 8. Wait for CLI to exit
    let cli_status = cli_child.wait().await.unwrap_or_else(|e| {
        eprintln!("[visp] Failed to wait for CLI: {e}");
        std::process::exit(1);
    });
    let exit_code = cli_status.code().unwrap_or(1);

    // 9. Send shutdown to daemon via gRPC
    eprintln!("[visp] Shutting down daemon...");
    if let Err(e) = send_shutdown(&addr).await {
        eprintln!("[visp] gRPC shutdown failed: {e}");
    }

    // 10. Wait for daemon to exit (5s timeout)
    let daemon_exit = tokio::time::timeout(std::time::Duration::from_secs(5), daemon.wait()).await;
    match daemon_exit {
        Ok(Ok(_)) => eprintln!("[visp] Daemon stopped."),
        Ok(Err(e)) => eprintln!("[visp] Daemon wait error: {e}"),
        Err(_) => {
            eprintln!("[visp] Daemon did not stop in time, killing...");
            let _ = kill_daemon(&mut daemon).await;
        }
    }

    std::process::exit(exit_code);
}

/// 查找二进制路径：优先同目录（cargo run 场景），其次 PATH。
fn resolve_bin(name: &str) -> PathBuf {
    // 检查 launcher 同目录
    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent()
    {
        let sibling = parent.join(name);
        if sibling.is_file() {
            return sibling;
        }
    }
    // 回退到 PATH
    PathBuf::from(name)
}

/// Parse an address like "[::1]:50051" or "127.0.0.1:9090" into (host, port).
fn parse_addr(addr: &str) -> Result<(String, u16), String> {
    // Handle bracketed IPv6: [::1]:50051
    if let Some(rest) = addr.strip_prefix('[') {
        if let Some(bracket_end) = rest.find(']') {
            let host = &rest[..bracket_end];
            let after = &rest[bracket_end + 1..];
            if let Some(port_str) = after.strip_prefix(':') {
                let port: u16 = port_str
                    .parse()
                    .map_err(|_| format!("invalid port in address: {addr}"))?;
                return Ok((host.to_string(), port));
            }
        }
        return Err(format!("invalid address: {addr}"));
    }
    // Handle plain host:port
    let (host, port_str) = addr
        .rsplit_once(':')
        .ok_or_else(|| format!("invalid address (no port): {addr}"))?;
    let port: u16 = port_str
        .parse()
        .map_err(|_| format!("invalid port in address: {addr}"))?;
    Ok((host.to_string(), port))
}

/// Reassemble host+port into an address string, bracketing IPv6 hosts.
fn format_addr(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

/// Find an available address starting from `base_addr`. Tries the given port
/// first; if it is already in use, increments the port up to 1000 attempts.
fn find_available_addr(base_addr: &str) -> Result<String, String> {
    let (host, base_port) = parse_addr(base_addr)?;
    for offset in 0..1000u16 {
        let port = base_port.saturating_add(offset);
        let addr = format_addr(&host, port);
        let sock_addr: std::net::SocketAddr = addr
            .parse()
            .map_err(|e| format!("invalid address {addr}: {e}"))?;
        if std::net::TcpListener::bind(sock_addr).is_ok() {
            return Ok(addr);
        }
        // Port is in use; try the next one.
        if offset == 0 {
            eprintln!("[visp] Port {port} is in use, trying next available port...");
        }
    }
    Err(format!(
        "could not find an available port starting from {base_port}"
    ))
}

async fn wait_for_health(addr: &str) -> bool {
    let endpoint = format!("http://{addr}");
    for _ in 0..30 {
        if let Ok(true) = connect_and_check(&endpoint).await {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    false
}

async fn connect_and_check(endpoint: &str) -> Result<bool, String> {
    let ch = Endpoint::new(endpoint.to_string())
        .map_err(|e| format!("endpoint: {e}"))?
        .connect()
        .await
        .map_err(|e| format!("connect: {e}"))?;
    let mut client = CoderDaemonClient::new(ch);
    let resp = client
        .health_check(())
        .await
        .map_err(|e| format!("health: {e}"))?;
    Ok(resp.into_inner().alive)
}

async fn send_shutdown(addr: &str) -> Result<(), String> {
    let endpoint = format!("http://{addr}");
    let ch = Endpoint::new(endpoint)
        .map_err(|e| format!("endpoint: {e}"))?
        .connect()
        .await
        .map_err(|e| format!("connect: {e}"))?;
    let mut client = CoderDaemonClient::new(ch);
    client
        .shutdown(ShutdownRequest { force: false })
        .await
        .map_err(|e| format!("shutdown: {e}"))?;
    Ok(())
}

fn format_timestamp() -> String {
    chrono::Local::now().format("%Y-%m-%d_%H-%M-%S").to_string()
}

async fn kill_daemon(daemon: &mut Child) {
    let _ = daemon.kill().await;
    let _ = daemon.wait().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_short_flag_passthrough() {
        let cli = Cli::try_parse_from(["visp", "-s", "abc"]).unwrap();
        assert_eq!(cli.session.as_deref(), Some("abc"));
    }

    #[test]
    fn session_long_flag_passthrough() {
        let cli = Cli::try_parse_from(["visp", "--session", "xyz"]).unwrap();
        assert_eq!(cli.session.as_deref(), Some("xyz"));
    }

    #[test]
    fn no_session_defaults_to_none() {
        let cli = Cli::try_parse_from(["visp"]).unwrap();
        assert!(cli.session.is_none());
    }

    #[test]
    fn session_with_project_short() {
        let cli = Cli::try_parse_from(["visp", "-s", "abc", "-p", "/tmp"]).unwrap();
        assert_eq!(cli.session.as_deref(), Some("abc"));
        assert_eq!(cli.project, "/tmp");
    }

    #[test]
    fn session_with_project_long() {
        let cli = Cli::try_parse_from(["visp", "--session", "abc", "--project", "/tmp"]).unwrap();
        assert_eq!(cli.session.as_deref(), Some("abc"));
        assert_eq!(cli.project, "/tmp");
    }

    #[test]
    fn session_alone_uses_default_project() {
        let cli = Cli::try_parse_from(["visp", "-s", "abc"]).unwrap();
        assert_eq!(cli.session.as_deref(), Some("abc"));
        assert_eq!(cli.project, ".");
    }
}
