use async_trait::async_trait;
use visp_core::tool::{Tool, ToolContext, ToolResult};

/// Rebuild the CodeGraph index from scratch.
pub struct CodeGraphRebuild;

#[async_trait]
impl Tool for CodeGraphRebuild {
    fn name(&self) -> &str {
        "codegraph_rebuild"
    }

    fn category(&self) -> &str {
        "analyze"
    }

    fn description(&self) -> &str {
        "Rebuild the CodeGraph index from scratch for the current project. \
         This rescans all supported source files and updates the symbol database. \
         Use this after making significant changes to the codebase, or when the index is stale. \
         Note: this may take a while depending on the project size."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": {}, "required": [] })
    }

    async fn execute(&self, _arguments: serde_json::Value, context: &ToolContext) -> ToolResult {
        let cg = match visp_codegraph::CodeGraph::open(&context.working_dir) {
            Ok(cg) => cg,
            Err(e) => return ToolResult::error(format!("CodeGraph open failed: {e}")),
        };
        let config = visp_codegraph::CodeGraphConfig::default();
        match cg.build_full(&context.working_dir, &config).await {
            Ok(()) => ToolResult::success("CodeGraph index rebuilt successfully."),
            Err(e) => ToolResult::error(format!("CodeGraph rebuild failed: {e}")),
        }
    }
}

pub struct CodeGraphSearch;

impl CodeGraphSearch {
    /// 从 daemon 配置构造（目前无配置项）
    pub fn from_toml(_raw: Option<&toml::Value>) -> Self {
        Self
    }
}

#[async_trait]
impl Tool for CodeGraphSearch {
    fn name(&self) -> &str {
        "codegraph_search"
    }

    fn category(&self) -> &str {
        "analyze"
    }

    fn description(&self) -> &str {
        "Search for symbols in the codebase using AST-aware indexing. \
         Use this to find function definitions, class declarations, variable references, etc. \
         Supports prefix-based search. \
         Only available after the codebase has been indexed by CodeGraph."
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

        let db_path = context.working_dir.join(".visp").join("codegraph.db");
        if !db_path.exists() {
            return ToolResult::error(
                "CodeGraph not initialized (run `visp init` in the project root).",
            );
        }

        let cg = match visp_codegraph::CodeGraph::open(&context.working_dir) {
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

impl CodeGraphGetDetails {
    /// 从 daemon 配置构造（目前无配置项）
    pub fn from_toml(_raw: Option<&toml::Value>) -> Self {
        Self
    }
}

#[async_trait]
impl Tool for CodeGraphGetDetails {
    fn name(&self) -> &str {
        "codegraph_get_details"
    }

    fn category(&self) -> &str {
        "analyze"
    }

    fn description(&self) -> &str {
        "Get detailed information about a specific symbol, including its callers and callees. \
         Use this to understand how a function/class is used across the codebase. \
         Returns: definition location, source code, list of callers (what calls it), \
         list of callees (what it calls). \
         Requires the codebase to be indexed by CodeGraph. \
         For finding a symbol in the first place, use CodeGraphSearch first."
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

        let db_path = context.working_dir.join(".visp").join("codegraph.db");
        if !db_path.exists() {
            return ToolResult::error(
                "CodeGraph not initialized (run `visp init` in the project root).",
            );
        }

        let cg = match visp_codegraph::CodeGraph::open(&context.working_dir) {
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

pub struct CodeGraphContext;

impl CodeGraphContext {
    pub fn from_toml(_raw: Option<&toml::Value>) -> Self {
        Self
    }
}

#[async_trait]
impl Tool for CodeGraphContext {
    fn name(&self) -> &str {
        "codegraph_context"
    }

    fn category(&self) -> &str {
        "analyze"
    }

    fn description(&self) -> &str {
        "Get comprehensive context about a topic in the codebase. Returns entry points, \
         call relationships, and source code. Use this when you need to understand how a \
         module works or find related code. Set detail=\"full\" to also get complete source \
         files (equivalent to reading multiple files at once)."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "task": {
                    "type": "string",
                    "description": "Space-separated keywords to search for"
                },
                "detail": {
                    "type": "string",
                    "description": "Detail level: \"overview\" (default) or \"full\"",
                    "default": "overview"
                },
                "max_nodes": {
                    "type": "integer",
                    "description": "Maximum number of symbols to include",
                    "default": 20
                }
            },
            "required": ["task"]
        })
    }

    async fn execute(&self, arguments: serde_json::Value, context: &ToolContext) -> ToolResult {
        let task = match arguments.get("task").and_then(|v| v.as_str()) {
            Some(t) => t,
            None => return ToolResult::error("Missing required parameter: task"),
        };
        let detail = arguments
            .get("detail")
            .and_then(|v| v.as_str())
            .unwrap_or("overview");
        let max_nodes = arguments
            .get("max_nodes")
            .and_then(|v| v.as_i64())
            .unwrap_or(20) as usize;

        let db_path = context.working_dir.join(".visp").join("codegraph.db");
        if !db_path.exists() {
            return ToolResult::error(
                "CodeGraph not initialized (run `visp init` in the project root).",
            );
        }

        let cg = match visp_codegraph::CodeGraph::open(&context.working_dir) {
            Ok(cg) => cg,
            Err(e) => return ToolResult::error(format!("CodeGraph open failed: {e}")),
        };

        let keywords: Vec<&str> = task.split_whitespace().collect();
        let mut all_names = Vec::new();
        let mut seen = std::collections::HashSet::new();

        for keyword in keywords {
            if keyword.is_empty() {
                continue;
            }
            if all_names.len() >= max_nodes {
                break;
            }
            if let Ok(results) = cg.search(keyword, 5) {
                for r in results {
                    let key = (r.name.clone(), r.file_path.clone());
                    if seen.insert(key) {
                        all_names.push(r.name.clone());
                        if all_names.len() >= max_nodes {
                            break;
                        }
                    }
                }
            }
        }

        if all_names.is_empty() {
            return ToolResult::success("No symbols found.");
        }

        let mut out = String::new();
        let mut entry_points = Vec::new();
        let mut related = Vec::new();
        let mut call_chain_parts = Vec::new();
        let mut file_symbols: std::collections::BTreeMap<String, Vec<(u32, String, String)>> =
            std::collections::BTreeMap::new();

        for name in &all_names {
            if let Ok(details) = cg.get_details(name) {
                for d in details {
                    entry_points.push(format!(
                        "  {}:{}  {}  {}",
                        d.file_path, d.line, d.kind, d.name
                    ));
                    if !d.callers.is_empty() || !d.callees.is_empty() {
                        let callers_str = if d.callers.is_empty() {
                            "none".to_string()
                        } else {
                            d.callers.join(", ")
                        };
                        let callees_str = if d.callees.is_empty() {
                            "none".to_string()
                        } else {
                            d.callees.join(", ")
                        };
                        call_chain_parts.push(format!(
                            "  {}  callers: {}  |  callees: {}",
                            d.name, callers_str, callees_str
                        ));
                    }
                    file_symbols.entry(d.file_path.clone()).or_default().push((
                        d.line,
                        d.name.clone(),
                        d.source.clone(),
                    ));
                    if d.file_path != name.clone() || d.name != *name {
                        related.push(format!(
                            "  {}:{}  {}  {}",
                            d.file_path, d.line, d.kind, d.name
                        ));
                    }
                }
            }
        }

        use std::fmt::Write;
        let _ = writeln!(out, "Entry points:");
        for ep in &entry_points {
            let _ = writeln!(out, "{ep}");
        }

        if !related.is_empty() {
            let _ = writeln!(out, "\nRelated symbols ({} total):", related.len());
            for r in &related {
                let _ = writeln!(out, "{r}");
            }
        }

        if !call_chain_parts.is_empty() {
            let _ = writeln!(out, "\nCall relationships:");
            for cc in &call_chain_parts {
                let _ = writeln!(out, "{cc}");
            }
        }

        match detail {
            "full" => {
                for (file_path, symbols) in &file_symbols {
                    let _ = writeln!(out, "\nSource ({file_path}):");
                    let content = match std::fs::read_to_string(file_path) {
                        Ok(c) => c,
                        Err(_) => continue,
                    };
                    let lines: Vec<&str> = content.lines().collect();
                    let sym_lines: std::collections::HashSet<u32> =
                        symbols.iter().map(|(line, _, _)| *line).collect();
                    let mut shown_lines = std::collections::HashSet::new();
                    for sym_line in &sym_lines {
                        let start = sym_line.saturating_sub(2) as usize;
                        let end = (lines.len()).min(*sym_line as usize + 8);
                        for i in start..end {
                            let line_no = i + 1;
                            if shown_lines.insert(line_no) {
                                let marker = if sym_lines.contains(&(line_no as u32)) {
                                    ">"
                                } else {
                                    " "
                                };
                                let _ = writeln!(
                                    out,
                                    "  {marker}{line_no:>4}  {}",
                                    lines.get(i).unwrap_or(&"")
                                );
                            }
                        }
                    }
                }
            }
            _ => {
                for (file_path, symbols) in &file_symbols {
                    let _ = writeln!(out, "\nSource ({file_path}):");
                    for (line, name, src) in symbols {
                        let _ = writeln!(out, "  {name} at line {line}:");
                        let _ = writeln!(out, "    {src}");
                    }
                }
            }
        }

        ToolResult::success(out)
    }
}

pub struct CodeGraphTrace;

impl CodeGraphTrace {
    pub fn from_toml(_raw: Option<&toml::Value>) -> Self {
        Self
    }
}

#[async_trait]
impl Tool for CodeGraphTrace {
    fn name(&self) -> &str {
        "codegraph_trace"
    }

    fn category(&self) -> &str {
        "analyze"
    }

    fn description(&self) -> &str {
        "Trace the call path from one symbol to another across the codebase. \
         Returns each function call in the chain with file:line locations and source code \
         snippets. Handles cross-file calls automatically. If no static path exists \
         (e.g. callbacks, dynamic dispatch), the result indicates where the chain breaks."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "from": {
                    "type": "string",
                    "description": "Starting symbol name"
                },
                "to": {
                    "type": "string",
                    "description": "Target symbol name"
                }
            },
            "required": ["from", "to"]
        })
    }

    async fn execute(&self, arguments: serde_json::Value, context: &ToolContext) -> ToolResult {
        let from = match arguments.get("from").and_then(|v| v.as_str()) {
            Some(f) => f,
            None => return ToolResult::error("Missing required parameter: from"),
        };
        let to = match arguments.get("to").and_then(|v| v.as_str()) {
            Some(t) => t,
            None => return ToolResult::error("Missing required parameter: to"),
        };

        let db_path = context.working_dir.join(".visp").join("codegraph.db");
        if !db_path.exists() {
            return ToolResult::error(
                "CodeGraph not initialized (run `visp init` in the project root).",
            );
        }

        let cg = match visp_codegraph::CodeGraph::open(&context.working_dir) {
            Ok(cg) => cg,
            Err(e) => return ToolResult::error(format!("CodeGraph open failed: {e}")),
        };

        match cg.trace(from, to) {
            Ok(path) => {
                if path.is_empty() {
                    return ToolResult::success(format!("No path found from {from} to {to}"));
                }
                let mut out = format!("Path from {from} to {to}:\n");
                for hop in &path {
                    use std::fmt::Write;
                    let _ = writeln!(out, "  {}:{}    {}", hop.file_path, hop.line, hop.name);
                }
                ToolResult::success(out)
            }
            Err(e) => ToolResult::error(format!("codegraph_trace failed: {e}")),
        }
    }
}

pub struct CodeGraphImpact;

impl CodeGraphImpact {
    pub fn from_toml(_raw: Option<&toml::Value>) -> Self {
        Self
    }
}

#[async_trait]
impl Tool for CodeGraphImpact {
    fn name(&self) -> &str {
        "codegraph_impact"
    }

    fn category(&self) -> &str {
        "analyze"
    }

    fn description(&self) -> &str {
        "Analyze what would be affected if you change a symbol. Returns all functions \
         that call it (callers) and all functions it calls (callees). Use this before \
         refactoring to understand the blast radius. depth controls recursion: 1 = direct \
         only (default), 2 = one level indirect."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "symbol": {
                    "type": "string",
                    "description": "Symbol name to analyze"
                },
                "depth": {
                    "type": "integer",
                    "description": "Recursion depth: 1 = direct only, 2 = one level indirect",
                    "default": 1
                }
            },
            "required": ["symbol"]
        })
    }

    async fn execute(&self, arguments: serde_json::Value, context: &ToolContext) -> ToolResult {
        let symbol = match arguments.get("symbol").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => return ToolResult::error("Missing required parameter: symbol"),
        };
        let depth = arguments.get("depth").and_then(|v| v.as_i64()).unwrap_or(1) as usize;

        let db_path = context.working_dir.join(".visp").join("codegraph.db");
        if !db_path.exists() {
            return ToolResult::error(
                "CodeGraph not initialized (run `visp init` in the project root).",
            );
        }

        let cg = match visp_codegraph::CodeGraph::open(&context.working_dir) {
            Ok(cg) => cg,
            Err(e) => return ToolResult::error(format!("CodeGraph open failed: {e}")),
        };

        match cg.impact(symbol, depth) {
            Ok(impact) => {
                let mut out = format!("Impact analysis for {}:\n", impact.symbol_name);

                use std::fmt::Write;

                if impact.callers.is_empty() {
                    let _ = writeln!(out, "  Called by (callers): none");
                } else {
                    let _ = writeln!(out, "  Called by (callers):");
                    for c in &impact.callers {
                        let _ =
                            writeln!(out, "    [depth={}]  {}:{}", c.depth, c.file_path, c.name);
                    }
                }

                if impact.callees.is_empty() {
                    let _ = writeln!(out, "  Calls (callees): none");
                } else {
                    let _ = writeln!(out, "  Calls (callees):");
                    for c in &impact.callees {
                        let _ =
                            writeln!(out, "    [depth={}]  {}:{}", c.depth, c.file_path, c.name);
                    }
                }

                ToolResult::success(out)
            }
            Err(e) => ToolResult::error(format!("codegraph_impact failed: {e}")),
        }
    }
}

#[cfg(test)]
#[path = "codegraph_tests.rs"]
mod tests;
