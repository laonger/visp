# AGENTS.md — vibewisp Agent Behavior Definition

You are **vibewisp**, a lightweight AI coding assistant running on a Rust backend.

This document defines how you, the vibewisp agent, should behave when coding on this project.

---

## Project Overview

**vibewisp** is an AI-powered coding assistant backend written in Rust. It uses a daemon architecture (frontend-backend separation) and communicates via gRPC. The project is a full rewrite of OpenCode from Node.js to Rust, aiming for lower CPU usage and better performance through Rust's zero-cost abstractions.

### Architecture

```
┌─────────────────────────────────────────────────────────┐
│  Frontend Layer (CLI / VSCode / Web)                    │
│       │ gRPC (tonic)                                    │
├───────┼─────────────────────────────────────────────────┤
│  Backend Daemon (Rust)                                  │
│  ┌─────────────────────────────────────────────────┐    │
│  │             gRPC Server (tonic)                 │    │
│  ├─────────────────────────────────────────────────┤    │
│  │  Agent Loop (input → LLM → Tool → LLM → ...)    │    │
│  │  ├─ Session Manager                             │    │
│  │  ├─ Prompt Builder                              │    │
│  │  ├─ Rule Engine                                 │    │
│  │  ├─ Tool Registry                               │    │
│  │  ├─ LLM Provider (Anthropic Claude)             │    │
│  │  ├─ Tool Executors (file/bash/search)           │    │
│  │  └─ CodeGraph Engine (tree-sitter + SQLite)     │    │
│  └─────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────┘
```

### Project Structure

```
vibewisp/
├── Cargo.toml                    # Workspace root
├── AGENTS.md                     # ← This file (agent behavior definition)
├── rust-toolchain.toml           # Rust toolchain config
├── docs/                         # Design docs & plans
│   ├── design/                   # Phase design documents
│   └── plans/                    # Phase development plans
├── crates/
│   ├── vbw-core/                 # Core logic (no I/O deps)
│   ├── vbw-proto/                # gRPC protocol definitions
│   ├── vbw-llm/                  # LLM provider integrations
│   ├── vbw-tools/                # Built-in tool implementations
│   ├── vbw-codegraph/            # Code intelligence engine
│   ├── vbw-daemon/               # Backend daemon process
│   └── vbw-cli/                  # CLI frontend (TUI)
└── .vibewisp/                    # Project-level config
    └── rules/                    # Rule files directory
```

---

## Key Modules & Responsibilities

### `vbw-core` — Core Logic (no I/O dependencies)

| File | Responsibility |
|---|---|
| `agent.rs` | Agent loop orchestrator: input → LLM → tool call → LLM → ... Implements `run_agent_loop` with streaming, parallel tool execution, retry logic, cancellation, and user approval. |
| `session.rs` | Session lifecycle management (`Idle → Running → Completed/Error`), `SessionManager` with `SessionStore` trait (currently `InMemorySessionStore`). |
| `message.rs` | Message model: `Role` (System/User/Assistant/Tool), `Message`, `ToolCallRequest`, `ToolDefinition`. |
| `prompt.rs` | `PromptBuilder` — assembles system prompt from template + rules + conversation history. |
| `provider.rs` | `LlmProvider` trait and `ChatEvent` enum. Defines the LLM provider interface. |
| `tool.rs` | `Tool` trait with `async_trait`. Defines tool interface with name/description/parameters/execute. |
| `tool_registry.rs` | `ToolRegistry` — register tools, get definitions for LLM function calling, execute by name. |
| `rules.rs` | `RuleEngine` — loads markdown rule files from `.vibewisp/rules/` (project) and `~/.config/vibewisp/rules/` (global) with `alwaysApply: true` frontmatter filter. |
| `error.rs` | Error type hierarchy: `CoreError`, `LlmError`, `SessionError`, `AgentErrorCode`. |

### `vbw-proto` — gRPC Protocol

| File | Description |
|---|---|
| `proto/vibewisp.proto` | protobuf service definition: `CoderDaemon` service with `Chat` (bidirectional stream), `CreateSession`, `ReadFile`, `SearchSymbols`, etc. |
| `build.rs` / `lib.rs` | Generated tonic/prost code via `prost-build` and `tonic-build`. |

### `vbw-llm` — LLM Provider

| File | Responsibility |
|---|---|
| `anthropic.rs` | Anthropic Claude API integration. Handles message format conversion, SSE stream parsing, retry/rate-limit handling, thinking mode, tool call input accumulation. |
| `streaming.rs` | SSE event parser (`parse_sse_events`). |
| `mock.rs` | Mock provider for testing. |

### `vbw-tools` — Built-in Tools

| File | Tool | Description |
|---|---|---|
| `file.rs` | `ReadFile` | Read file (1MB limit, binary detection, path validation) |
| `file.rs` | `WriteFile` | Write file (overwrite, auto-create parent dirs, path validation) |
| `file.rs` | `EditFile` | String replacement (atomic write via temp+rename, reject multi-match) |
| `bash.rs` | `Bash` | Shell execution (command blacklist, timeout, stdin null) |
| `search.rs` | `Grep` | Regex content search (prefers ripgrep, auto-excludes binary/dirs) |
| `search.rs` | `Glob` | Filename glob search (prefers ripgrep) |
| `codegraph.rs` | `CodeGraphSearch` | Symbol search via CodeGraph engine |
| `codegraph.rs` | `CodeGraphGetDetails` | Symbol details (callers/callees/docstring) |
| `path.rs` | — | Path safety validation (prevent directory traversal) |
| `truncate.rs` | — | Output truncation helper |

### `vbw-codegraph` — Code Intelligence Engine

| File | Responsibility |
|---|---|
| `graph.rs` | Data structures: `Symbol`, `Edge`, `FileInfo`, `SymbolKind`, `EdgeKind`. |
| `parser.rs` | tree-sitter based parser for TypeScript/TSX. |
| `store.rs` | SQLite persistence layer via `rusqlite`. |
| `index.rs` | Full and incremental index builder. |
| `query.rs` | Symbol query engine (prefix search, details with callers/callees). |
| `watcher.rs` | File system watcher for incremental updates via `notify`. |
| `lib.rs` | `CodeGraph` struct — public API (open/build/search/get_details/watch/shutdown). |

### `vbw-daemon` — Backend Daemon

| File | Responsibility |
|---|---|
| `main.rs` | Entry point: init tracing, load config, create provider/tools/rules/sessions, start gRPC server. |
| `server.rs` | gRPC server setup with tonic. |
| `service.rs` | `CoderDaemonService` — implements all gRPC methods. Handles session CRUD, chat bidirectional stream, file reads, codegraph queries, health check. |
| `config.rs` | TOML config loader (daemon/llm/tools/agent sections). Default config at `~/.config/vibewisp/daemon.toml`. |
| `command/init.rs` | `/init` command handler — project initialization. |

### `vbw-cli` — CLI Frontend

| File | Responsibility |
|---|---|
| `main.rs` | Entry point: parse args, connect to daemon, create session, start REPL. |
| `client.rs` | gRPC client wrapper for tonic. |
| `app.rs` | TUI application state: message cache, markdown rendering, syntax highlighting (syntect), chat line management, scrolling. |
| `event.rs` | Event loop: handles server events (text delta, tool call, user query, done). |
| `theme.rs` | Color theme definitions. |
| `ui.rs` | Ratatui rendering: layout, blocks, scroll view. |

---

## Build & Test Commands

```bash
# Build everything
cargo build --release

# Run daemon
cargo run --release --bin vbw-daemon

# Run CLI (connect to running daemon)
cargo run --release --bin vbw -- --project /path/to/project

# Run all tests
cargo test

# Run tests for a specific crate
cargo test -p vbw-core
cargo test -p vbw-tools
cargo test -p vbw-llm
cargo test -p vbw-codegraph
cargo test -p vbw-daemon
cargo test -p vbw-cli
cargo test -p vbw-proto

# Run a specific test
cargo test test_name

# Clippy (lint)
cargo clippy --all-targets -- -D warnings

# Format
cargo fmt --all
```

---

## Coding Conventions & Design Patterns

### General Principles

- **Idiomatic Rust**: Use `Result<T, E>` for fallible operations, `Option<T>` for optional values, proper error propagation with `?`.
- **async-first**: All I/O operations are async (tokio). Use `#[async_trait]` for trait async methods.
- **No panics**: Prefer `Result`/`Option` over `unwrap`/`expect` in production code. Panics only in test code or infallible paths.
- **Type-driven design**: Use enums for fixed variants, newtypes for domain concepts, trait abstractions for polymorphism.

### Error Handling

- Use `thiserror` for library error enums (`#[derive(Error)]`).
- Error hierarchy flows upward: low-level errors get wrapped into higher-level types.
- `CoreError` is the top-level error type aggregating all subsystem errors.
- `anyhow::Result` used in binary entry points (daemon main), not in library code.
- `AgentErrorCode` enum for structured error events sent to the frontend.

### Session & State Management

- Sessions have a strict state machine: `Idle → Running → Completed | Error`.
- `SessionManager` wraps a `SessionStore` trait (currently `InMemorySessionStore`).
- `CancellationToken` from `tokio_util` is used for cancellation propagation.
- Agent loop context is consumed, not shared — each session runs its own loop.

### Tool System

- All tools implement the `Tool` trait (`name`, `description`, `parameters`, `execute`, `requires_approval`).
- Tools are registered in `ToolRegistry` at startup.
- Tool parameters use JSON Schema format for LLM function calling.
- `ToolResult` has `success()` and `error()` constructors.
- Path safety: `validate_path()` ensures all file operations stay within the working directory.
- Atomic writes: `EditFile` writes to a temp file, then renames.

### LLM Provider Architecture

- `LlmProvider` trait with `chat_stream()` returning a stream of `ChatEvent`.
- Event types: `TextDelta`, `ToolCall`, `ThinkingBlock`, `UsageInfo`, `Done`.
- SSE parsing is provider-specific (Anthropic uses SSE with content block deltas).
- Retry logic in `run_agent_loop` with exponential backoff for network/rate-limit errors.

### Message Protocol

- `Message` model with `role`, `content`, `tool_call_id`, `tool_calls`, `extra_blocks`, `skip_context`.
- `skip_context` flag lets system messages (like `/init`) be injected without polluting conversation history.
- `PromptBuilder` filters out `skip_context` messages and combines system template with rules.

### CodeGraph Design

- `SymbolKind` enum: `Function`, `Method`, `Class`, `Interface`, `TypeAlias`, `Variable`, `Enum`.
- `EdgeKind` enum: `Call`, `Reference`, `Implementation`, `Inheritance`.
- SQLite-backed with rusqlite, tree-sitter for parsing (TypeScript/TSX).
- Lazy initialization: CodeGraph is opened on first query, not at daemon startup.
- Background full-index build on first access, followed by file watcher for incremental updates.

### Configuration

- TOML config with `[daemon]`, `[llm]`, `[tools]`, `[agent]` sections.
- Config loaded from `~/.config/vibewisp/daemon.toml` by default, or explicit `--config` path.
- Each section has sensible Rust defaults via `Default` trait / default functions.

### Testing Philosophy

- Unit tests in `#[cfg(test)] mod tests` at bottom of each module.
- Integration-style tests in test modules with `#[tokio::test]` for async tests.
- Mock provider (`TestProvider`) and mock tools used for agent loop tests.
- `tempfile::TempDir` for file system tests (cleanup on drop).
- Test coverage on: CRUD operations, state transitions, error paths, edge cases (empty/truncated/binary).

---

## Important Configuration & Environment Setup

### Environment Variables

| Variable | Purpose |
|---|---|
| `ANTHROPIC_API_KEY` | API key for Anthropic Claude (required). Can also be set in config file. |
| `RUST_LOG` | Controls tracing log level (e.g., `info`, `debug`, `warn`). |
| `HOME` | Used to locate `~/.config/vibewisp/` config and rules. |

### Configuration File (`~/.config/vibewisp/daemon.toml`)

```toml
[daemon]
listen_addr = "[::1]:50051"
log_level = "info"

[llm]
provider = "anthropic"
model = "claude-sonnet-4-20250514"
api_key = "sk-ant-..."
base_url = "https://api.anthropic.com"      # optional, for custom endpoints
temperature = 0.7
max_tokens = 4096
thinking_budget_tokens = 2048                 # optional, enables Claude thinking

[tools]
bash_timeout_secs = 120
file_max_size_bytes = 1048576

[agent]
max_iterations = 50
llm_retry_attempts = 3
llm_retry_base_delay_ms = 1000
bash_confirm_mode = true
file_max_size_bytes = 1048576
```

### Project-level Configuration

- `.vibewisp/rules/*.md` — rule files (only those with `alwaysApply: true` frontmatter are active)
- `.vibewisp/system-prompt.md` — custom system prompt template (overrides built-in default)
- `.vibewisp/codegraph.db` — CodeGraph SQLite database (auto-generated)

### CLI Commands (REPL)

| Command | Purpose |
|---|---|
| `/quit` / `/exit` | Quit REPL |
| `/clear` | Clear screen |
| `/temp <val>` | Set temperature (e.g., `/temp 0.5`) |
| `/model <name>` | Switch model (e.g., `/model claude-sonnet-4-20250514`) |
| `/help` | Display help |
| `/init` | Initialize project (setup CodeGraph, etc.) |

### Rust Toolchain

- Channel: `stable`
- Edition: 2024 (workspace-level)
- Required components: `rustc`, `cargo`, `clippy`, `rustfmt`
- See `rust-toolchain.toml` for exact specification

### Key Dependencies

| Crate | Version | Purpose |
|---|---|---|
| tokio | 1 (full) | Async runtime |
| tonic | 0.13 | gRPC server/client |
| prost | 0.13 | protobuf codegen |
| serde / serde_json | 1 | Serialization |
| async-trait | 0.1 | Async trait support |
| thiserror | 2 | Error derive macros |
| anyhow | 1 | Flexible error type |
| uuid | 1 (v4) | Session ID generation |
| tracing | 0.1 | Structured logging |
| reqwest | 0.12 | HTTP client (LLM API) |
| notify | 7 | File system watcher |
| tree-sitter | — | Code parsing |
| rusqlite | — | SQLite bindings |
| ratatui | — | TUI rendering (CLI) |
| syntect | — | Syntax highlighting (CLI) |
| clap | — | CLI argument parsing |
