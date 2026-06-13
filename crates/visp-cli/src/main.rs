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

    #[arg(short = 's', long)]
    session: Option<String>,

    #[arg(long)]
    list: bool,
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

    // ── --list mode ──────────────────────────────────────────
    if cli.list {
        match client.list_sessions().await {
            Ok(sessions) => {
                if sessions.is_empty() {
                    println!("No sessions found.");
                } else {
                    // Filter by project if -p provided
                    let project_path = std::path::PathBuf::from(&cli.project);
                    let project_str = project_path.to_string_lossy().to_string();
                    let filtered: Vec<_> = sessions
                        .iter()
                        .filter(|s| s.project_path == project_str)
                        .collect();

                    if filtered.is_empty() {
                        println!("No sessions found for project: {}", project_str);
                    } else {
                        println!("Sessions (project: {}):", project_str);
                        for s in &filtered {
                            let short_id: String = s.session_id.chars().take(8).collect();
                            let status = format!("{:?}", s.status);
                            println!("  {}  {}  {}", short_id, status, s.session_id);
                        }
                        println!("\nUse: visp -s <short-id> to resume a session");
                    }
                }
            }
            Err(e) => {
                eprintln!("Failed to list sessions: {}", e);
            }
        }
        return;
    }

    // ── Resolve session (new or existing) ───────────────────
    let session = if let Some(session_id) = &cli.session {
        // Resume existing session
        match client.get_session(session_id).await {
            Ok(s) => {
                // Validate -p matches session's project_path
                let project_path = std::path::PathBuf::from(&cli.project);
                let resolved_path = project_path.canonicalize().unwrap_or(project_path);
                let project_str = resolved_path.to_string_lossy().to_string();
                if s.project_path != project_str {
                    eprintln!(
                        "Error: Session '{}' belongs to project '{}', but -p is '{}'.",
                        s.session_id.chars().take(8).collect::<String>(),
                        s.project_path,
                        project_str
                    );
                    eprintln!("Use the correct project path: visp -p <project> -s <session-id>");
                    std::process::exit(1);
                }
                s
            }
            Err(e) => {
                eprintln!("Session '{}' not found: {}", session_id, e);
                // List recent sessions for guidance
                match client.list_sessions().await {
                    Ok(sessions) if !sessions.is_empty() => {
                        let project_path = std::path::PathBuf::from(&cli.project);
                        let resolved_path = project_path.canonicalize().unwrap_or(project_path);
                        let project_str = resolved_path.to_string_lossy().to_string();
                        let filtered: Vec<_> = sessions
                            .iter()
                            .filter(|s| s.project_path == project_str)
                            .collect();
                        if !filtered.is_empty() {
                            eprintln!("\nRecent sessions for '{}':", project_str);
                            for s in &filtered {
                                let short_id: String = s.session_id.chars().take(8).collect();
                                let status = format!("{:?}", s.status);
                                eprintln!("  {}  {}", short_id, status);
                            }
                            eprintln!("\nUse: visp -s <short-id> to resume");
                        }
                    }
                    _ => {}
                }
                std::process::exit(1);
            }
        }
    } else {
        // Create new session (existing logic)
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

        match client.create_session(&project_str, config).await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Failed to create session: {}", e);
                std::process::exit(1);
            }
        }
    };

    // ── Start chat (shared for both new and resumed) ────────
    let session_id = session.session_id.clone();
    let chat_handle = match client.chat(&session_id).await {
        Ok(ch) => ch,
        Err(e) => {
            eprintln!("Failed to start chat: {}", e);
            std::process::exit(1);
        }
    };

    // Send join message so daemon sends history immediately (before first UserInput)
    chat_handle.send_join();

    let model = cli.model.clone().unwrap_or_else(|| session.model.clone());
    let sid_for_display = session.session_id.clone();
    if let Err(e) = event::run(
        session_id,
        chat_handle,
        model.clone(),
        &mut client,
        session.project_path.as_str(),
        session.available_models.clone(),
    )
    .await
    {
        eprintln!("Event loop error: {}", e);
    }

    eprintln!("Session closed: {}", sid_for_display);
}
