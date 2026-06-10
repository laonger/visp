mod app;
mod client;
mod event;
mod theme;
mod ui;

use clap::Parser;
use client::VbwClient;
use visp_proto::visp::LlmConfig as ProtoLlmConfig;

#[derive(Parser)]
#[command(name = "visp", about = "visp CLI — AI coding assistant")]
struct Cli {
    #[arg(short = 'a', long, default_value = "[::1]:50051")]
    addr: String,

    #[arg(short = 'p', long, default_value = ".")]
    project: String,

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

    let mut client = match VbwClient::connect(&cli.addr).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to connect to daemon at {}: {}", cli.addr, e);
            eprintln!("Start daemon with: visp-daemon");
            std::process::exit(1);
        }
    };

    match client.health_check().await {
        Ok(true) => {}
        _ => {
            eprintln!("Daemon is not healthy at {}", cli.addr);
            std::process::exit(1);
        }
    }

    let mut extra = std::collections::HashMap::new();
    if let Some(budget) = cli.thinking_budget {
        extra.insert("thinking_budget_tokens".into(), budget.to_string());
    }
    let config = if cli.model.is_some() || cli.temperature.is_some() || !extra.is_empty() {
        Some(ProtoLlmConfig {
            model: cli.model.clone(),
            temperature: cli.temperature,
            max_tokens: None,
            max_context_tokens: None,
            extra,
        })
    } else {
        None
    };

    let project = std::path::PathBuf::from(&cli.project);
    let project_path = project.canonicalize().unwrap_or(project);
    let project_str = project_path.to_string_lossy().to_string();

    let session = match client.create_session(&project_str, config).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to create session: {}", e);
            std::process::exit(1);
        }
    };

    let chat_handle = match client.chat(&session.session_id).await {
        Ok(ch) => ch,
        Err(e) => {
            eprintln!("Failed to start chat: {}", e);
            std::process::exit(1);
        }
    };

    let model = cli.model.clone().unwrap_or(session.model);
    if let Err(e) = event::run(session.session_id, chat_handle, model, &mut client, &project_str).await {
        eprintln!("Event loop error: {}", e);
    }
}
