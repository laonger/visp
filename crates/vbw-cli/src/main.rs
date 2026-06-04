mod app;
mod client;
mod display;
mod repl;

use clap::Parser;
use vbw_proto::vibewisp::LlmConfig;

#[derive(Parser)]
#[command(name = "vbw", about = "vibewisp CLI — AI coding assistant")]
struct Cli {
    /// Daemon address
    #[arg(short = 'a', long, default_value = "[::1]:50051")]
    addr: String,

    /// Project path
    #[arg(short = 'p', long, default_value = ".")]
    project: String,

    /// Override model name
    #[arg(long)]
    model: Option<String>,

    /// Override temperature
    #[arg(long)]
    temperature: Option<f64>,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // 1. Connect to daemon
    let mut client = match client::VbwClient::connect(&cli.addr).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("❌ Failed to connect to daemon at {}", cli.addr);
            eprintln!("   Error: {}", e);
            eprintln!("   Start daemon with: vbw-daemon");
            std::process::exit(1);
        }
    };

    // 2. Health check
    match client.health_check().await {
        Ok(true) => {}
        Ok(false) => {
            eprintln!("❌ Daemon is not healthy");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("❌ Health check failed: {}", e);
            std::process::exit(1);
        }
    }

    // 3. Build optional LlmConfig from CLI args
    let config = if cli.model.is_some() || cli.temperature.is_some() {
        Some(LlmConfig {
            model: cli.model,
            temperature: cli.temperature,
            max_tokens: None,
            extra: Default::default(),
        })
    } else {
        None
    };

    // 4. Create session
    let session = match client.create_session(&cli.project, config).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("❌ Failed to create session: {}", e);
            std::process::exit(1);
        }
    };
    let session_id = session.session_id.clone();
    println!("Session: {} (project: {})", &session_id[..8], cli.project);

    // 5. Start Chat
    let chat_handle = match client.chat(&session_id).await {
        Ok(ch) => ch,
        Err(e) => {
            eprintln!("❌ Failed to start chat: {}", e);
            std::process::exit(1);
        }
    };

    // 6. Run REPL
    if let Err(e) = repl::run(session_id, chat_handle).await {
        eprintln!("REPL error: {}", e);
    }
}
