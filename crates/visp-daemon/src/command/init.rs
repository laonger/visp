use std::path::Path;

use visp_codegraph::{CodeGraph, index::CodeGraphConfig};
use visp_core::message::Message;

/// Base prompt template for /init.
/// `{STEP4}` is replaced with either the update or force-rewrite variant.
#[allow(dead_code)]
const INIT_PROMPT_TEMPLATE: &str = r"You are initializing AGENTS.md for a new project.

Follow these steps to create an appropriate AGENTS.md:

1. Read README.md, Cargo.toml, and other top-level configuration files to understand the project
2. Browse the project structure using `glob` to understand file organization
3. Search symbols using `codegraph_search` to understand the codebase
4. {STEP4}
5. Write AGENTS.md using write_file with the complete content in ONE call.
   IMPORTANT: Write the file exactly once. Do NOT rewrite it after writing.
   Finish with no further tool calls after writing.";

#[allow(dead_code)]
const STEP4_UPDATE: &str =
    "Read the existing AGENTS.md if present — update it rather than rewrite from scratch.";
#[allow(dead_code)]
const STEP4_FORCE: &str = "Ignore any existing AGENTS.md — rewrite it from scratch.";

/// Prepare an init command response.
///
/// Parses `--force` from `text`, creates `.visp/` directories,
/// opens/creates CodeGraph, builds the full index, and returns
/// a `Message` (with `skip_context: true`) plus status messages.
#[allow(dead_code)]
pub async fn prepare(project_path: &Path, text: &str) -> Result<(Message, Vec<String>), String> {
    let force = text.contains("--force");

    // Create .visp subdirectories
    let visp_dir = project_path.join(".visp");
    for sub in ["rules", "skills", "plans"] {
        std::fs::create_dir_all(visp_dir.join(sub))
            .map_err(|e| format!("failed to create .visp/{sub}: {e}"))?;
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
        "Creating .visp/...".to_string(),
        "Initializing CodeGraph...".to_string(),
    ];

    Ok((msg, statuses))
}
