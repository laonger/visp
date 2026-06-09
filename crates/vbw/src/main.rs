use std::path::PathBuf;

use clap::Parser;
use tokio::process::{Child, Command};
use tonic::transport::Endpoint;
use vbw_proto::vibewisp::ShutdownRequest;
use vbw_proto::vibewisp::coder_daemon_client::CoderDaemonClient;

/// vibewisp launcher — starts daemon + CLI in one command
#[derive(Parser)]
#[command(name = "vbw")]
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
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // Resolve sibling binary paths (same dir as launcher for cargo run, or PATH)
    let daemon_bin = resolve_bin("vbw-daemon");
    let cli_bin = resolve_bin("vbw-cli");

    // 1. Create log directory
    let log_dir = get_log_dir();
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

    // 3. Start daemon
    eprintln!("[vbw] Starting daemon (log: {})...", log_path.display());
    let mut daemon = match Command::new(&daemon_bin)
        .arg("--listen-addr")
        .arg(&cli.addr)
        .stdout(log_file_stdout.into_std().await)
        .stderr(log_file_stderr.into_std().await)
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            eprintln!("[vbw] Failed to start daemon: {e}");
            eprintln!("[vbw] Make sure 'vbw-daemon' is installed. Try: cargo build");
            std::process::exit(1);
        }
    };

    // 4. Health check with timeout
    eprintln!("[vbw] Waiting for daemon to be ready...");
    let addr = cli.addr.clone();
    let health_ok =
        tokio::time::timeout(std::time::Duration::from_secs(15), wait_for_health(&addr)).await;

    match health_ok {
        Ok(true) => eprintln!("[vbw] Daemon is ready."),
        Ok(false) => {
            eprintln!("[vbw] Daemon health check failed.");
            kill_daemon(&mut daemon).await;
            std::process::exit(1);
        }
        Err(_) => {
            eprintln!("[vbw] Daemon did not become ready within 15s.");
            kill_daemon(&mut daemon).await;
            std::process::exit(1);
        }
    }

    // 5. Build CLI args
    let mut cli_args = vec![
        "vbw-cli".to_string(),
        "--addr".to_string(),
        cli.addr.clone(),
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

    // 6. Start CLI
    let mut cli_child = match Command::new(&cli_bin)
        .args(&cli_args[1..]) // skip the program name
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            eprintln!("[vbw] Failed to start CLI: {e}");
            eprintln!("[vbw] Make sure 'vbw-cli' is installed.");
            kill_daemon(&mut daemon).await;
            std::process::exit(1);
        }
    };

    // 7. Wait for CLI to exit
    let cli_status = cli_child.wait().await.unwrap_or_else(|e| {
        eprintln!("[vbw] Failed to wait for CLI: {e}");
        std::process::exit(1);
    });
    let exit_code = cli_status.code().unwrap_or(1);

    // 8. Send shutdown to daemon via gRPC
    eprintln!("[vbw] Shutting down daemon...");
    if let Err(e) = send_shutdown(&cli.addr).await {
        eprintln!("[vbw] gRPC shutdown failed: {e}");
    }

    // 9. Wait for daemon to exit (5s timeout)
    let daemon_exit = tokio::time::timeout(std::time::Duration::from_secs(5), daemon.wait()).await;
    match daemon_exit {
        Ok(Ok(_)) => eprintln!("[vbw] Daemon stopped."),
        Ok(Err(e)) => eprintln!("[vbw] Daemon wait error: {e}"),
        Err(_) => {
            eprintln!("[vbw] Daemon did not stop in time, killing...");
            let _ = kill_daemon(&mut daemon).await;
        }
    }

    std::process::exit(exit_code);
}

fn get_log_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".vibewisp").join("logs")
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
