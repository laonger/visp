use std::path::Path;

use vbw_codegraph::{CodeGraph, index::CodeGraphConfig};
use vbw_core::message::Message;

/// Base prompt template for /init.
/// `{STEP4}` is replaced with either the update or force-rewrite variant.
const INIT_PROMPT_TEMPLATE: &str = r"You are initializing AGENTS.md for a new project.

Follow these steps to create an appropriate AGENTS.md:

1. Read README.md, Cargo.toml, and other top-level configuration files to understand the project
2. Browse the project structure using `glob` to understand file organization
3. Search symbols using `codegraph_search` to understand the codebase
4. {STEP4}
5. Write AGENTS.md using write_file with the complete content in ONE call.
   IMPORTANT: Write the file exactly once. Do NOT rewrite it after writing.
   Finish with no further tool calls after writing.";

const STEP4_UPDATE: &str =
    "Read the existing AGENTS.md if present — update it rather than rewrite from scratch.";
const STEP4_FORCE: &str = "Ignore any existing AGENTS.md — rewrite it from scratch.";

/// Prepare an init command response.
///
/// Parses `--force` from `text`, creates `.vibewisp/` directories,
/// opens/creates CodeGraph, builds the full index, and returns
/// a `Message` (with `skip_context: true`) plus status messages.
pub async fn prepare(project_path: &Path, text: &str) -> Result<(Message, Vec<String>), String> {
    let force = text.contains("--force");

    // Create .vibewisp subdirectories
    let vbw_dir = project_path.join(".vibewisp");
    for sub in ["rules", "skills", "plans"] {
        std::fs::create_dir_all(vbw_dir.join(sub))
            .map_err(|e| format!("failed to create .vibewisp/{sub}: {e}"))?;
    }

    // Open / create CodeGraph and build the full index
    let cg = CodeGraph::open(project_path)?;
    let config = CodeGraphConfig::default();
    cg.build_full(project_path, &config)
        .await
        .map_err(|e| format!("codegraph build failed: {e}"))?;

    // Build prompt
    let step4 = if force { STEP4_FORCE } else { STEP4_UPDATE };
    let prompt = INIT_PROMPT_TEMPLATE.replace("{STEP4}", step4);

    let msg = Message {
        content: prompt,
        skip_context: true,
        ..Message::user("")
    };

    let statuses = vec![
        "Creating .vibewisp/...".to_string(),
        "Initializing CodeGraph...".to_string(),
    ];

    Ok((msg, statuses))
}
