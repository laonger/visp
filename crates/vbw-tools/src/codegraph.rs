use async_trait::async_trait;
use vbw_core::tool::{Tool, ToolContext, ToolResult};

pub struct CodeGraphSearch;

#[async_trait]
impl Tool for CodeGraphSearch {
    fn name(&self) -> &str {
        "codegraph_search"
    }

    fn description(&self) -> &str {
        "Search symbols in the project codebase by name. Use this to find where functions, classes, interfaces, variables are defined. Returns results with file path, line number, kind, and signature."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Symbol name or partial name to search"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of results",
                    "default": 20
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, arguments: serde_json::Value, context: &ToolContext) -> ToolResult {
        let query = match arguments.get("query").and_then(|v| v.as_str()) {
            Some(q) => q,
            None => return ToolResult::error("Missing required parameter: query"),
        };
        let limit = arguments
            .get("limit")
            .and_then(|v| v.as_i64())
            .unwrap_or(20) as usize;

        let db_path = context.working_dir.join(".vibewisp").join("codegraph.db");
        if !db_path.exists() {
            return ToolResult::error(
                "CodeGraph not initialized (run `vbw init` in the project root).",
            );
        }

        let cg = match vbw_codegraph::CodeGraph::open(&context.working_dir) {
            Ok(cg) => cg,
            Err(e) => return ToolResult::error(format!("CodeGraph open failed: {e}")),
        };

        match cg.search(query, limit) {
            Ok(results) => {
                if results.is_empty() {
                    return ToolResult::success("No symbols found.");
                }
                let mut out = String::new();
                for s in &results {
                    use std::fmt::Write;
                    let _ = writeln!(
                        out,
                        "{}:{}  {}  {}  {}",
                        s.file_path,
                        s.line,
                        s.kind,
                        s.name,
                        s.signature.as_deref().unwrap_or(""),
                    );
                }
                ToolResult::success(out)
            }
            Err(e) => ToolResult::error(format!("codegraph_search failed: {e}")),
        }
    }
}

pub struct CodeGraphGetDetails;

#[async_trait]
impl Tool for CodeGraphGetDetails {
    fn name(&self) -> &str {
        "codegraph_get_details"
    }

    fn description(&self) -> &str {
        "Get detailed information about a symbol, including its signature, docstring, callers, and callees."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Symbol name"
                }
            },
            "required": ["name"]
        })
    }

    async fn execute(&self, arguments: serde_json::Value, context: &ToolContext) -> ToolResult {
        let name = match arguments.get("name").and_then(|v| v.as_str()) {
            Some(n) => n,
            None => return ToolResult::error("Missing required parameter: name"),
        };

        let db_path = context.working_dir.join(".vibewisp").join("codegraph.db");
        if !db_path.exists() {
            return ToolResult::error(
                "CodeGraph not initialized (run `vbw init` in the project root).",
            );
        }

        let cg = match vbw_codegraph::CodeGraph::open(&context.working_dir) {
            Ok(cg) => cg,
            Err(e) => return ToolResult::error(format!("CodeGraph open failed: {e}")),
        };

        match cg.get_details(name) {
            Ok(results) => {
                if results.is_empty() {
                    return ToolResult::success("Symbol not found.");
                }
                let mut out = String::new();
                for s in &results {
                    use std::fmt::Write;
                    let _ = writeln!(out, "{}:{}  {}  {}", s.file_path, s.line, s.kind, s.name,);
                    if let Some(ref sig) = s.signature {
                        let _ = writeln!(out, "  signature: {sig}");
                    }
                    if let Some(ref doc) = s.docstring {
                        let _ = writeln!(out, "  doc: {doc}");
                    }
                    if !s.callers.is_empty() {
                        let _ = write!(out, "  callers: {}", s.callers.join(", "));
                    }
                    if !s.callees.is_empty() {
                        let _ = write!(out, "\n  callees: {}", s.callees.join(", "));
                    }
                }
                ToolResult::success(out)
            }
            Err(e) => ToolResult::error(format!("codegraph_get_details failed: {e}")),
        }
    }
}
