use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use walkdir::WalkDir;

use crate::graph::Edge;
use crate::parser::Parser;
use crate::store::Store;

/// Configuration for indexing a codebase
pub struct CodeGraphConfig {
    pub exclude_dirs: Vec<String>,
    pub supported_extensions: Vec<String>,
}

impl Default for CodeGraphConfig {
    fn default() -> Self {
        Self {
            exclude_dirs: vec![
                "node_modules".into(),
                ".git".into(),
                "dist".into(),
                "build".into(),
            ],
            supported_extensions: vec![
                ".ts".into(),
                ".tsx".into(),
                ".rs".into(),
                ".py".into(),
                ".c".into(),
                ".h".into(),
                ".cpp".into(),
                ".hpp".into(),
                ".cc".into(),
                ".go".into(),
            ],
        }
    }
}

/// Map a file extension to a human-readable language name.
fn language_name(rel_path: &str) -> &'static str {
    if rel_path.ends_with(".rs") {
        "rust"
    } else if rel_path.ends_with(".py") {
        "python"
    } else if rel_path.ends_with(".ts") || rel_path.ends_with(".tsx") {
        "typescript"
    } else if rel_path.ends_with(".c") || rel_path.ends_with(".h") {
        "c"
    } else if rel_path.ends_with(".cpp") || rel_path.ends_with(".hpp") || rel_path.ends_with(".cc")
    {
        "cpp"
    } else if rel_path.ends_with(".go") {
        "go"
    } else {
        "unknown"
    }
}

/// Types of file system events for incremental indexing
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileEvent {
    Created,
    Modified,
    Removed,
}

/// Indexes a codebase into a graph database, supporting full builds and
/// incremental updates with cross-file symbol resolution.
pub struct Indexer {
    store: Arc<Store>,
}

impl Indexer {
    pub fn new(store: Arc<Store>) -> Self {
        Self { store }
    }

    /// Perform a full build: walk the project directory, parse all supported
    /// files, insert symbols/edges/imports/exports, and resolve cross-file edges.
    pub fn build_full(
        &self,
        project_path: &Path,
        config: &CodeGraphConfig,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let files = collect_files(project_path, config)?;

        let mut parser = Parser::new()?;

        for file_path in &files {
            let content = match std::fs::read_to_string(file_path) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("Skipping {}: {}", file_path.display(), e);
                    continue;
                }
            };

            let rel_path = file_path
                .strip_prefix(project_path)
                .unwrap_or(file_path)
                .to_string_lossy()
                .to_string();

            let parse_result = match parser.parse_file(&rel_path, &content) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("Skipping {}: {}", rel_path, e);
                    continue;
                }
            };

            // Insert symbols first to get real DB ids
            let old_ids: Vec<u64> = parse_result.symbols.iter().map(|s| s.id).collect();
            let mut symbols = parse_result.symbols;
            self.store.insert_symbols(&mut symbols)?;

            let id_map: HashMap<u64, u64> = old_ids
                .into_iter()
                .zip(symbols.iter().map(|s| s.id))
                .collect();

            // Remap edge source_ids from temp → real.
            // Skip edges whose source_id wasn't mapped (temp id 0 with no
            // real counterpart – top-level calls outside any symbol).
            let edges: Vec<Edge> = parse_result
                .edges
                .into_iter()
                .filter_map(|mut e| {
                    if let Some(&new_id) = id_map.get(&e.source_id) {
                        e.source_id = new_id;
                        Some(e)
                    } else {
                        None
                    }
                })
                .collect();
            self.store.insert_edges(&edges)?;

            self.store
                .insert_imports(&rel_path, &parse_result.imports)?;
            self.store
                .insert_exports(&rel_path, &parse_result.exports)?;

            let ts = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            self.store.upsert_file(
                &rel_path,
                language_name(&rel_path),
                symbols.len() as u32,
                ts,
            )?;
        }

        // Cross-file resolution
        resolve_cross_file_edges(&self.store)?;

        Ok(())
    }

    /// Incrementally update the index for a single file change.
    ///
    /// For `Created`/`Modified`: removes old data for the file, re-parses and
    /// inserts fresh data, then re-resolves cross-file edges.
    /// For `Removed`: deletes the file's symbols and file record.
    pub fn update_file(
        &self,
        project_path: &Path,
        file_path: &Path,
        event: FileEvent,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let rel_path = file_path
            .strip_prefix(project_path)
            .unwrap_or(file_path)
            .to_string_lossy()
            .to_string();

        match event {
            FileEvent::Created | FileEvent::Modified => {
                // Remove old data
                self.store.delete_by_file(&rel_path)?;

                // Parse the file
                let content = std::fs::read_to_string(file_path)?;
                let mut parser = Parser::new()?;
                let parse_result = parser.parse_file(&rel_path, &content)?;

                // Insert symbols
                let old_ids: Vec<u64> = parse_result.symbols.iter().map(|s| s.id).collect();
                let mut symbols = parse_result.symbols;
                self.store.insert_symbols(&mut symbols)?;

                let id_map: HashMap<u64, u64> = old_ids
                    .into_iter()
                    .zip(symbols.iter().map(|s| s.id))
                    .collect();

                // Remap edges (skip unmapped – see build_full)
                let edges: Vec<Edge> = parse_result
                    .edges
                    .into_iter()
                    .filter_map(|mut e| {
                        if let Some(&new_id) = id_map.get(&e.source_id) {
                            e.source_id = new_id;
                            Some(e)
                        } else {
                            None
                        }
                    })
                    .collect();
                self.store.insert_edges(&edges)?;

                self.store
                    .insert_imports(&rel_path, &parse_result.imports)?;
                self.store
                    .insert_exports(&rel_path, &parse_result.exports)?;

                let ts = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                self.store.upsert_file(
                    &rel_path,
                    language_name(&rel_path),
                    symbols.len() as u32,
                    ts,
                )?;

                // Re-resolve cross-file edges
                resolve_cross_file_edges(&self.store)?;
            }
            FileEvent::Removed => {
                self.store.delete_by_file(&rel_path)?;
                self.store.delete_file_record(&rel_path)?;
            }
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
//  Helpers
// ---------------------------------------------------------------------------

/// Collect all supported files under `project_path`, excluding ignored dirs.
fn collect_files(
    project_path: &Path,
    config: &CodeGraphConfig,
) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    let mut files = Vec::new();

    for entry in WalkDir::new(project_path)
        .into_iter()
        .filter_entry(|e| {
            // Prevent descending into excluded directories
            if e.file_type().is_dir()
                && let Some(name) = e.file_name().to_str()
            {
                return !config.exclude_dirs.iter().any(|d| d == name);
            }
            true
        })
        .filter_map(|e| e.ok())
    {
        let path = entry.path();

        if !entry.file_type().is_file() {
            continue;
        }

        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            let dot_ext = format!(".{}", ext);
            if config.supported_extensions.contains(&dot_ext)
                || config.supported_extensions.contains(&ext.to_string())
            {
                files.push(path.to_path_buf());
            }
        }
    }

    files.sort();
    Ok(files)
}

/// Resolve all unresolved edges by matching target_name against known symbols.
///
/// For each unresolved edge (target_id IS NULL, target_name IS NOT NULL),
/// look up symbols with that name project-wide and resolve the edge.
fn resolve_cross_file_edges(store: &Store) -> Result<(), Box<dyn std::error::Error>> {
    let unresolved = store.get_unresolved_edges()?;
    for (edge_id, target_name) in &unresolved {
        let candidates = store.get_symbols_by_name(target_name)?;
        if let Some(sym) = candidates.first() {
            store.resolve_edge(*edge_id, sym.id)?;
        }
    }
    Ok(())
}

/// Resolve an import source path to an actual file path within the project.
///
/// Given an import like `./bar` from `src/a.ts`, tries:
/// - `src/bar` (exact), `src/bar.ts`, `src/bar.tsx`
/// - `src/bar/index.ts`, `src/bar/index.tsx`
///
/// Returns `None` if no matching file is found.
#[allow(dead_code)]
fn resolve_import_source(importer_dir: &Path, import_source: &str) -> Option<PathBuf> {
    let base = importer_dir.join(import_source);

    let trials = [
        base.clone(),
        base.with_extension("ts"),
        base.with_extension("tsx"),
        base.join("index.ts"),
        base.join("index.tsx"),
    ];

    for p in &trials {
        if p.exists() && p.is_file() {
            return Some(p.to_path_buf());
        }
    }

    None
}

// ---------------------------------------------------------------------------
//  Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn create_store(dir: &tempfile::TempDir) -> Store {
        let db_path = dir.path().join("test.db");
        Store::open(&db_path).expect("Failed to open store")
    }

    fn create_indexer(store: &Arc<Store>) -> Indexer {
        Indexer::new(store.clone())
    }

    fn default_config() -> CodeGraphConfig {
        CodeGraphConfig {
            exclude_dirs: vec![
                "node_modules".into(),
                ".git".into(),
                "dist".into(),
                "build".into(),
            ],
            supported_extensions: vec![".ts".into(), ".tsx".into()],
        }
    }

    fn write_file(root: &Path, rel: &str, content: &str) {
        let full = root.join(rel);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&full, content).unwrap();
    }

    // ------------------------------------------------------------------
    //  Step 5a: Full build tests
    // ------------------------------------------------------------------

    #[test]
    fn test_full_build_basic() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();

        write_file(&project, "a.ts", "export function foo() { return 1; }\n");
        write_file(&project, "b.ts", "function bar() { return 2; }\n");
        write_file(
            &project,
            "c.ts",
            "export const baz = () => { return 3; };\n",
        );

        let store = Arc::new(create_store(&tmp));
        let indexer = create_indexer(&store);
        let config = default_config();

        indexer.build_full(&project, &config).unwrap();

        let symbols = store.search_symbols("", 100).unwrap();
        assert_eq!(symbols.len(), 3);
    }

    #[test]
    fn test_full_build_excludes_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("project");
        std::fs::create_dir_all(project.join("node_modules")).unwrap();
        std::fs::create_dir_all(project.join("src")).unwrap();

        write_file(&project, "src/valid.ts", "export function hello() {}\n");
        write_file(
            &project,
            "node_modules/ignored.ts",
            "export function ignored() {}\n",
        );

        let store = Arc::new(create_store(&tmp));
        let indexer = create_indexer(&store);
        let config = default_config();

        indexer.build_full(&project, &config).unwrap();

        let symbols = store.search_symbols("", 100).unwrap();
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "hello");
    }

    #[test]
    fn test_full_build_cross_file_resolution() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();

        write_file(&project, "a.ts", "export function foo() {}\n");
        write_file(&project, "b.ts", "import { foo } from './a';\nfoo();\n");

        let store = Arc::new(create_store(&tmp));
        let indexer = create_indexer(&store);
        let config = default_config();

        indexer.build_full(&project, &config).unwrap();

        let foo_symbols = store.get_symbols_by_name("foo").unwrap();
        assert!(!foo_symbols.is_empty(), "foo symbol should exist");

        let unresolved = store.get_unresolved_edges().unwrap();
        assert!(
            unresolved.is_empty(),
            "all edges should be resolved, got {} unresolved",
            unresolved.len()
        );
    }

    #[test]
    fn test_full_build_skip_parse_error() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();

        write_file(&project, "valid.ts", "export function fine() {}\n");
        write_file(&project, "broken.ts", "export function { bad syntax here\n");

        let store = Arc::new(create_store(&tmp));
        let indexer = create_indexer(&store);
        let config = default_config();

        indexer.build_full(&project, &config).unwrap();

        let symbols = store.search_symbols("", 100).unwrap();
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "fine");
    }

    // ------------------------------------------------------------------
    //  Step 5b: Incremental update tests
    // ------------------------------------------------------------------

    #[test]
    fn test_incremental_add_file() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();

        write_file(&project, "a.ts", "export function existing() {}\n");

        let store = Arc::new(create_store(&tmp));
        let indexer = create_indexer(&store);
        let config = default_config();

        indexer.build_full(&project, &config).unwrap();

        // Add a new file
        let new_path = project.join("b.ts");
        write_file(&project, "b.ts", "export function added() {}\n");
        indexer
            .update_file(&project, &new_path, FileEvent::Created)
            .unwrap();

        let symbols = store.search_symbols("", 100).unwrap();
        assert_eq!(symbols.len(), 2);
    }

    #[test]
    fn test_incremental_modify_file() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();

        let file_path = project.join("a.ts");
        write_file(&project, "a.ts", "export function original() {}\n");

        let store = Arc::new(create_store(&tmp));
        let indexer = create_indexer(&store);
        let config = default_config();

        indexer.build_full(&project, &config).unwrap();

        // Replace symbol
        write_file(&project, "a.ts", "export function modified() {}\n");
        indexer
            .update_file(&project, &file_path, FileEvent::Modified)
            .unwrap();

        let symbols = store.search_symbols("", 100).unwrap();
        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(!names.contains(&"original"), "original should be gone");
        assert!(names.contains(&"modified"), "modified should exist");
    }

    #[test]
    fn test_incremental_delete_file() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();

        write_file(&project, "a.ts", "export function keep() {}\n");
        let file_to_delete = project.join("b.ts");
        write_file(&project, "b.ts", "export function remove() {}\n");

        let store = Arc::new(create_store(&tmp));
        let indexer = create_indexer(&store);
        let config = default_config();

        indexer.build_full(&project, &config).unwrap();

        // Delete b.ts
        std::fs::remove_file(&file_to_delete).unwrap();
        indexer
            .update_file(&project, &file_to_delete, FileEvent::Removed)
            .unwrap();

        let symbols = store.search_symbols("", 100).unwrap();
        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(symbols.len(), 1);
        assert!(names.contains(&"keep"));
        assert!(!names.contains(&"remove"));
    }
}
