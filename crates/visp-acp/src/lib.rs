//! visp-acp — visp 的 ACP(Agent Client Protocol)agent。
//!
//! 由 ACP 客户端(如 Zed)以子进程方式启动,stdin/stdout 走 JSON-RPC(ACP),
//! 内部通过 gRPC 连接(默认自行拉起)`visp-daemon`。日志一律走 stderr,
//! stdout 仅输出 ACP 协议消息。

use std::net::SocketAddr;
use std::path::PathBuf;

use clap::Parser;

/// 自拉起模式的端口探测基准地址。
pub const DEFAULT_LISTEN_ADDR: &str = "[::1]:50051";

/// visp-acp 命令行参数。
#[derive(Parser, Debug)]
#[command(name = "visp-acp", version, about = "visp ACP agent (stdio)")]
pub struct Cli {
    /// 项目路径(默认当前目录)
    #[arg(short = 'p', long, default_value = ".")]
    pub project: PathBuf,

    /// 直连的 daemon gRPC 地址;缺省时自动探测空闲端口并拉起 daemon
    #[arg(short = 'a', long, value_parser = clap::value_parser!(SocketAddr))]
    pub addr: Option<SocketAddr>,

    /// 独立 config 目录(默认隔离;显式指向同一目录可与 CLI 共享会话存储)
    #[arg(long)]
    pub config_dir: Option<PathBuf>,

    /// 直连模式下退出时也向 daemon 发送 Shutdown(默认直连不关 daemon)
    #[arg(long)]
    pub shutdown_on_exit: bool,
}

impl Cli {
    /// 自拉起模式的端口探测基准地址(未指定 `--addr` 时)。
    pub fn base_addr(&self) -> SocketAddr {
        self.addr
            .unwrap_or_else(|| DEFAULT_LISTEN_ADDR.parse().expect("valid default addr"))
    }
}

/// 初始化 stderr 日志(ACP 纪律:stdout 仅输出协议消息,日志绝不写 stdout)。
pub fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

/// visp-acp 主流程(Wave 2+:daemon 编排 → ACP 事件循环)。
pub fn run(cli: Cli) -> anyhow::Result<()> {
    tracing::info!(
        project = %cli.project.display(),
        addr = ?cli.addr,
        config_dir = ?cli.config_dir,
        shutdown_on_exit = cli.shutdown_on_exit,
        "visp-acp starting (skeleton)"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    fn parse<const N: usize>(args: [&str; N]) -> Cli {
        Cli::parse_from(args)
    }

    #[test]
    fn project_defaults_to_current_dir() {
        let cli = parse(["visp-acp"]);
        assert_eq!(cli.project, PathBuf::from("."));
    }

    #[test]
    fn addr_defaults_to_probe_mode_with_base_addr() {
        let cli = parse(["visp-acp"]);
        assert!(cli.addr.is_none(), "缺省应处于自拉起探测模式");
        assert_eq!(
            cli.base_addr(),
            SocketAddr::new(Ipv6Addr::LOCALHOST.into(), 50051)
        );
    }

    #[test]
    fn addr_direct_connect_when_given() {
        let cli = parse(["visp-acp", "--addr", "127.0.0.1:9999"]);
        assert_eq!(
            cli.addr,
            Some(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 9999))
        );
        assert_eq!(cli.base_addr(), cli.addr.unwrap());
    }

    #[test]
    fn addr_rejects_invalid_value() {
        let result = Cli::try_parse_from(["visp-acp", "--addr", "not-an-address"]);
        assert!(result.is_err(), "非法地址应解析失败");
    }

    #[test]
    fn config_dir_and_shutdown_on_exit_parse() {
        let cli = parse([
            "visp-acp",
            "--config-dir",
            "/tmp/visp-test",
            "--shutdown-on-exit",
        ]);
        assert_eq!(cli.config_dir, Some(PathBuf::from("/tmp/visp-test")));
        assert!(cli.shutdown_on_exit);
    }

    #[test]
    fn shutdown_on_exit_defaults_to_false() {
        let cli = parse(["visp-acp"]);
        assert!(!cli.shutdown_on_exit);
    }
}
