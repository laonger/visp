use std::collections::HashSet;
use std::error::Error;
use std::path::Path;

use tree_sitter::{Language, Node, Parser as TsParser};

use crate::graph::{Edge, EdgeKind, Symbol, SymbolKind};

fn language_for_ext(ext: &str) -> Option<Language> {
    match ext {
        ".ts" | ".tsx" => Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
        ".rs" => Some(tree_sitter_rust::LANGUAGE.into()),
        ".py" => Some(tree_sitter_python::LANGUAGE.into()),
        ".c" | ".h" => Some(tree_sitter_c::LANGUAGE.into()),
        ".cpp" | ".hpp" | ".cc" => Some(tree_sitter_cpp::LANGUAGE.into()),
        ".go" => Some(tree_sitter_go::LANGUAGE.into()),
        _ => None,
    }
}

/// Return a short language identifier string for use in match arms.
fn lang_str_for_ext(ext: &str) -> Option<&'static str> {
    match ext {
        ".ts" | ".tsx" => Some("typescript"),
        ".rs" => Some("rust"),
        ".py" => Some("python"),
        ".c" | ".h" => Some("c"),
        ".cpp" | ".hpp" | ".cc" => Some("cpp"),
        ".go" => Some("go"),
        _ => None,
    }
}

pub struct Parser {
    parser: TsParser,
    current_lang: Option<Language>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParseResult {
    pub symbols: Vec<Symbol>,
    pub edges: Vec<Edge>,
    pub imports: Vec<(String, String)>,
    pub exports: Vec<(String, Option<u64>, Option<String>)>,
}

impl Parser {
    pub fn new() -> Result<Self, Box<dyn Error>> {
        Ok(Self {
            parser: TsParser::new(),
            current_lang: None,
        })
    }

    pub fn parse_file(
        &mut self,
        file_path: &str,
        content: &str,
    ) -> Result<ParseResult, Box<dyn Error>> {
        let ext = Path::new(file_path)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| format!(".{e}"))
            .unwrap_or_default();
        let lang =
            language_for_ext(&ext).ok_or_else(|| format!("unsupported file extension: {ext}"))?;
        let lang_str =
            lang_str_for_ext(&ext).ok_or_else(|| format!("unsupported file extension: {ext}"))?;
        if self.current_lang.as_ref() != Some(&lang) {
            self.parser.set_language(&lang)?;
            self.current_lang = Some(lang);
        }

        let tree = self
            .parser
            .parse(content, None)
            .ok_or("Parser returned None (timeout or no language set)")?;

        let root = tree.root_node();

        if root.has_error() {
            return Err("Syntax error in source".into());
        }

        let source = content.as_bytes();
        let mut symbols: Vec<Symbol> = Vec::new();
        let mut edges: Vec<Edge> = Vec::new();
        let mut imports: Vec<(String, String)> = Vec::new();
        let mut exports: Vec<(String, Option<u64>, Option<String>)> = Vec::new();
        let mut dedup: HashSet<(String, String)> = HashSet::new();
        let mut next_id: u64 = 0;

        walk_children(
            root,
            source,
            file_path,
            lang_str,
            &mut symbols,
            &mut edges,
            &mut imports,
            &mut exports,
            &mut dedup,
            &mut next_id,
            None,
        );

        Ok(ParseResult {
            symbols,
            edges,
            imports,
            exports,
        })
    }
}

/// Walk a subtree looking for an identifier node, returning its text.
/// Used for C/C++ function_definition where the name is nested inside declarator.
fn find_identifier_in_node(node: Node, source: &[u8]) -> Option<String> {
    if let Some(name_node) = node.child_by_field_name("name")
        && let Ok(name) = name_node.utf8_text(source)
    {
        return Some(name.to_string());
    }
    // Fall back: iterate children looking for identifier
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            if child.kind() == "identifier"
                && let Ok(name) = child.utf8_text(source)
            {
                return Some(name.to_string());
            }
            // Recurse into named children (skip punctuation)
            if child.is_named()
                && let Some(found) = find_identifier_in_node(child, source)
            {
                return Some(found);
            }
        }
    }
    None
}

// ------------------------------------------------------------------
//  Free helper functions (no Self:: needed, avoids too_many_arguments on Parser impl)
// ------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn walk_children(
    node: Node,
    source: &[u8],
    file_path: &str,
    lang: &str,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
    imports: &mut Vec<(String, String)>,
    exports: &mut Vec<(String, Option<u64>, Option<String>)>,
    dedup: &mut HashSet<(String, String)>,
    next_id: &mut u64,
    current_sym_id: Option<u64>,
) {
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            walk_node(
                child,
                source,
                file_path,
                lang,
                symbols,
                edges,
                imports,
                exports,
                dedup,
                next_id,
                current_sym_id,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn walk_node(
    node: Node,
    source: &[u8],
    file_path: &str,
    lang: &str,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
    imports: &mut Vec<(String, String)>,
    exports: &mut Vec<(String, Option<u64>, Option<String>)>,
    dedup: &mut HashSet<(String, String)>,
    next_id: &mut u64,
    current_sym_id: Option<u64>,
) {
    match (node.kind(), lang) {
        // --- Symbol declarations ---
        // C/C++ function definitions store name inside declarator, not as name field
        // Walk the declarator subtree to find the identifier node
        ("function_definition", "c" | "cpp") => {
            let new_id = find_identifier_in_node(node, source).map(|name| {
                add_symbol(
                    &name,
                    SymbolKind::Function,
                    file_path,
                    node,
                    symbols,
                    dedup,
                    next_id,
                )
            });
            let sym_id = new_id.or(current_sym_id);
            walk_children(
                node, source, file_path, lang, symbols, edges, imports, exports, dedup, next_id,
                sym_id,
            );
        }

        ("function_declaration" | "function_item" | "function_definition", _) => {
            let new_id = node.child_by_field_name("name").and_then(|n| {
                n.utf8_text(source).ok().map(|name| {
                    let is_python_method = lang == "python"
                        && current_sym_id
                            .and_then(|id| symbols.iter().find(|s: &&Symbol| s.id == id))
                            .is_some_and(|s| s.kind == SymbolKind::Class);
                    add_symbol(
                        name,
                        if is_python_method {
                            SymbolKind::Method
                        } else {
                            SymbolKind::Function
                        },
                        file_path,
                        node,
                        symbols,
                        dedup,
                        next_id,
                    )
                })
            });
            let sym_id = new_id.or(current_sym_id);
            walk_children(
                node, source, file_path, lang, symbols, edges, imports, exports, dedup, next_id,
                sym_id,
            );
        }

        // TS/JS generators: function* gen() {}
        ("generator_function_declaration" | "generator_function", _) => {
            let new_id = node.child_by_field_name("name").and_then(|n| {
                n.utf8_text(source).ok().map(|name| {
                    add_symbol(
                        name,
                        SymbolKind::Function,
                        file_path,
                        node,
                        symbols,
                        dedup,
                        next_id,
                    )
                })
            });
            let sym_id = new_id.or(current_sym_id);
            walk_children(
                node, source, file_path, lang, symbols, edges, imports, exports, dedup, next_id,
                sym_id,
            );
        }

        (
            "class_declaration" | "struct_item" | "class_definition" | "struct_specifier"
            | "class_specifier",
            _,
        ) => {
            let new_id = node.child_by_field_name("name").and_then(|n| {
                n.utf8_text(source).ok().map(|name| {
                    add_symbol(
                        name,
                        SymbolKind::Class,
                        file_path,
                        node,
                        symbols,
                        dedup,
                        next_id,
                    )
                })
            });
            let sym_id = new_id.or(current_sym_id);
            if node.kind() == "class_declaration" {
                walk_class_body(
                    node, source, file_path, lang, symbols, edges, imports, exports, dedup,
                    next_id, sym_id,
                );
            } else {
                walk_children(
                    node, source, file_path, lang, symbols, edges, imports, exports, dedup,
                    next_id, sym_id,
                );
            }
        }

        ("method_definition" | "method_declaration", _) => {
            let new_id = node.child_by_field_name("name").and_then(|n| {
                n.utf8_text(source).ok().map(|name| {
                    add_symbol(
                        name,
                        SymbolKind::Method,
                        file_path,
                        node,
                        symbols,
                        dedup,
                        next_id,
                    )
                })
            });
            let sym_id = new_id.or(current_sym_id);
            walk_children(
                node, source, file_path, lang, symbols, edges, imports, exports, dedup, next_id,
                sym_id,
            );
        }

        // TS/JS interface method signatures: interface I { foo(): void }
        ("method_signature", _) => {
            if let Some(name_node) = node.child_by_field_name("name")
                && let Ok(name) = name_node.utf8_text(source)
            {
                add_symbol(
                    name,
                    SymbolKind::Method,
                    file_path,
                    node,
                    symbols,
                    dedup,
                    next_id,
                );
            }
        }

        ("interface_declaration" | "trait_item" | "interface_type", _) => {
            node.child_by_field_name("name").and_then(|n| {
                n.utf8_text(source).ok().map(|name| {
                    add_symbol(
                        name,
                        SymbolKind::Interface,
                        file_path,
                        node,
                        symbols,
                        dedup,
                        next_id,
                    );
                })
            });
            walk_children(
                node,
                source,
                file_path,
                lang,
                symbols,
                edges,
                imports,
                exports,
                dedup,
                next_id,
                current_sym_id,
            );
        }

        (
            "type_alias_declaration" | "type_item" | "type_alias_statement" | "type_declaration",
            _,
        ) => {
            node.child_by_field_name("name").and_then(|n| {
                n.utf8_text(source).ok().map(|name| {
                    add_symbol(
                        name,
                        SymbolKind::TypeAlias,
                        file_path,
                        node,
                        symbols,
                        dedup,
                        next_id,
                    );
                })
            });
            walk_children(
                node,
                source,
                file_path,
                lang,
                symbols,
                edges,
                imports,
                exports,
                dedup,
                next_id,
                current_sym_id,
            );
        }

        ("enum_declaration" | "enum_item" | "enum_specifier", _) => {
            node.child_by_field_name("name").and_then(|n| {
                n.utf8_text(source).ok().map(|name| {
                    add_symbol(
                        name,
                        SymbolKind::Enum,
                        file_path,
                        node,
                        symbols,
                        dedup,
                        next_id,
                    );
                })
            });
            walk_children(
                node,
                source,
                file_path,
                lang,
                symbols,
                edges,
                imports,
                exports,
                dedup,
                next_id,
                current_sym_id,
            );
        }

        // Variable / lexical declarations – check for arrow_function values
        ("lexical_declaration" | "variable_declaration" | "let_declaration", _) => {
            handle_variable_declaration(
                node,
                source,
                file_path,
                lang,
                symbols,
                edges,
                imports,
                exports,
                dedup,
                next_id,
                current_sym_id,
            );
        }

        // Go const declaration — contains const_spec children with name/type/value
        ("const_declaration", "go") => {
            for i in 0..node.child_count() {
                if let Some(spec) = node.child(i as u32)
                    && spec.kind() == "const_spec"
                    && let Some(name_node) = spec.child_by_field_name("name")
                    && let Ok(name) = name_node.utf8_text(source)
                {
                    let id = add_symbol(
                        name,
                        SymbolKind::Variable,
                        file_path,
                        spec,
                        symbols,
                        dedup,
                        next_id,
                    );
                    // Walk children for value expressions
                    walk_children(
                        spec,
                        source,
                        file_path,
                        lang,
                        symbols,
                        edges,
                        imports,
                        exports,
                        dedup,
                        next_id,
                        Some(id),
                    );
                }
            }
        }

        // Rust const_item and static_item — emit as Variable
        ("const_item" | "static_item", "rust") => {
            if let Some(name_node) = node.child_by_field_name("name")
                && let Ok(name) = name_node.utf8_text(source)
            {
                let id = add_symbol(
                    name,
                    SymbolKind::Variable,
                    file_path,
                    node,
                    symbols,
                    dedup,
                    next_id,
                );
                walk_children(
                    node,
                    source,
                    file_path,
                    lang,
                    symbols,
                    edges,
                    imports,
                    exports,
                    dedup,
                    next_id,
                    Some(id),
                );
            }
        }

        // Arrow functions assigned to variables are handled above.
        // A bare arrow_function node gets walked for inner edges only.
        ("arrow_function", _) => {
            walk_children(
                node,
                source,
                file_path,
                lang,
                symbols,
                edges,
                imports,
                exports,
                dedup,
                next_id,
                current_sym_id,
            );
        }

        // --- Edges ---
        ("call_expression", _) => {
            if let Some(func) = node.child_by_field_name("function")
                && let Ok(name) = func.utf8_text(source)
            {
                add_edge(current_sym_id, EdgeKind::Call, name, edges);
            }
            walk_children(
                node,
                source,
                file_path,
                lang,
                symbols,
                edges,
                imports,
                exports,
                dedup,
                next_id,
                current_sym_id,
            );
        }

        ("new_expression", _) => {
            if let Some(ctor) = node.child_by_field_name("constructor")
                && let Ok(name) = ctor.utf8_text(source)
            {
                add_edge(current_sym_id, EdgeKind::Call, name, edges);
            }
            walk_children(
                node,
                source,
                file_path,
                lang,
                symbols,
                edges,
                imports,
                exports,
                dedup,
                next_id,
                current_sym_id,
            );
        }

        ("extends_clause", _) => {
            for i in 0..node.child_count() {
                if let Some(child) = node.child(i as u32)
                    && child.is_named()
                    && let Ok(name) = child.utf8_text(source)
                {
                    add_edge(current_sym_id, EdgeKind::Inheritance, name, edges);
                }
            }
        }

        ("implements_clause", _) => {
            for i in 0..node.child_count() {
                if let Some(child) = node.child(i as u32)
                    && child.is_named()
                    && let Ok(name) = child.utf8_text(source)
                {
                    add_edge(current_sym_id, EdgeKind::Implementation, name, edges);
                }
            }
        }

        // --- Imports ---
        ("import_statement" | "use_declaration" | "import_declaration", _) => {
            handle_import(node, source, imports);
            walk_children(
                node,
                source,
                file_path,
                lang,
                symbols,
                edges,
                imports,
                exports,
                dedup,
                next_id,
                current_sym_id,
            );
        }

        // --- Exports ---
        ("export_statement", _) => {
            handle_export(
                node,
                source,
                file_path,
                lang,
                symbols,
                edges,
                imports,
                exports,
                dedup,
                next_id,
                current_sym_id,
            );
        }

        (_, _) => {
            walk_children(
                node,
                source,
                file_path,
                lang,
                symbols,
                edges,
                imports,
                exports,
                dedup,
                next_id,
                current_sym_id,
            );
        }
    }
}

/// Add a symbol and return its new id.
fn add_symbol(
    name: &str,
    kind: SymbolKind,
    file_path: &str,
    node: Node,
    symbols: &mut Vec<Symbol>,
    dedup: &mut HashSet<(String, String)>,
    next_id: &mut u64,
) -> u64 {
    let key = (name.to_string(), file_path.to_string());
    if dedup.contains(&key) {
        symbols
            .iter()
            .find(|s| s.name == name && s.file_path == file_path)
            .map(|s| s.id)
            .unwrap_or(*next_id)
    } else {
        dedup.insert(key);
        let pos = node.start_position();
        let id = *next_id;
        *next_id += 1;
        symbols.push(Symbol {
            id,
            name: name.to_string(),
            kind,
            file_path: file_path.to_string(),
            line: pos.row as u32 + 1,
            column: pos.column as u32 + 1,
            signature: None,
            docstring: None,
        });
        id
    }
}

fn add_edge(source_id: Option<u64>, kind: EdgeKind, target_name: &str, edges: &mut Vec<Edge>) {
    edges.push(Edge {
        source_id: source_id.unwrap_or(0),
        target_id: None,
        target_name: Some(target_name.to_string()),
        kind,
    });
}

// ------------------------------------------------------------------
//  Variable declaration handling
// ------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn handle_variable_declaration(
    node: Node,
    source: &[u8],
    file_path: &str,
    lang: &str,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
    imports: &mut Vec<(String, String)>,
    exports: &mut Vec<(String, Option<u64>, Option<String>)>,
    dedup: &mut HashSet<(String, String)>,
    next_id: &mut u64,
    current_sym_id: Option<u64>,
) {
    let mut last_sym_id = current_sym_id;

    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32)
            && child.kind() == "variable_declarator"
        {
            let has_arrow = (0..child.child_count()).any(|j| {
                child
                    .child(j as u32)
                    .map(|c| c.kind() == "arrow_function")
                    .unwrap_or(false)
            });

            let name = child
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(source).ok());

            if let Some(name) = name {
                if has_arrow {
                    let id = add_symbol(
                        name,
                        SymbolKind::Function,
                        file_path,
                        child,
                        symbols,
                        dedup,
                        next_id,
                    );
                    last_sym_id = Some(id);
                } else if !dedup.contains(&(name.to_string(), file_path.to_string())) {
                    let id = add_symbol(
                        name,
                        SymbolKind::Variable,
                        file_path,
                        child,
                        symbols,
                        dedup,
                        next_id,
                    );
                    last_sym_id = Some(id);
                }
            }

            // Walk the declarator children for edges inside (e.g. arrow body)
            walk_children(
                child,
                source,
                file_path,
                lang,
                symbols,
                edges,
                imports,
                exports,
                dedup,
                next_id,
                last_sym_id,
            );
        }
    }
}

// ------------------------------------------------------------------
//  Import handling
// ------------------------------------------------------------------

fn handle_import(node: Node, source: &[u8], imports: &mut Vec<(String, String)>) {
    let source_str = node
        .child_by_field_name("source")
        .and_then(|n| n.utf8_text(source).ok())
        .map(|s| s.trim_matches('\'').trim_matches('"').to_string());

    let Some(source_str) = source_str else {
        return;
    };

    let clause = (0..node.child_count())
        .find_map(|i| node.child(i as u32).filter(|c| c.kind() == "import_clause"));

    if let Some(clause) = clause {
        // named_imports: import { foo, bar } from '...'
        if let Some(named) = (0..clause.child_count()).find_map(|i| {
            clause
                .child(i as u32)
                .filter(|c| c.kind() == "named_imports")
        }) {
            for j in 0..named.child_count() {
                if let Some(spec) = named.child(j as u32)
                    && spec.kind() == "import_specifier"
                    && let Some(ident) = spec.child_by_field_name("name")
                    && let Ok(name) = ident.utf8_text(source)
                {
                    imports.push((name.to_string(), source_str.clone()));
                }
            }
        }
        // namespace_import: import * as foo from '...'
        if let Some(ns) = (0..clause.child_count()).find_map(|i| {
            clause
                .child(i as u32)
                .filter(|c| c.kind() == "namespace_import")
        }) && let Some(ident) = ns.child_by_field_name("name")
            && let Ok(name) = ident.utf8_text(source)
        {
            imports.push((name.to_string(), source_str.clone()));
        }
        // default import: import foo from '...'
        if let Some(default) = (0..clause.child_count())
            .find_map(|i| clause.child(i as u32).filter(|c| c.kind() == "identifier"))
            && let Ok(name) = default.utf8_text(source)
        {
            imports.push((name.to_string(), source_str));
        }
    }
}

// ------------------------------------------------------------------
//  Export handling
// ------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn handle_export(
    node: Node,
    source: &[u8],
    file_path: &str,
    lang: &str,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
    imports: &mut Vec<(String, String)>,
    exports: &mut Vec<(String, Option<u64>, Option<String>)>,
    dedup: &mut HashSet<(String, String)>,
    next_id: &mut u64,
    current_sym_id: Option<u64>,
) {
    // Check for re-export: export { foo } from './other'
    if let Some(source_node) = node.child_by_field_name("source")
        && let Ok(src) = source_node.utf8_text(source)
    {
        let src = src.trim_matches('\'').trim_matches('"');
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i as u32)
                && child.kind() == "export_clause"
            {
                for j in 0..child.child_count() {
                    if let Some(spec) = child.child(j as u32)
                        && spec.kind() == "export_specifier"
                        && let Some(ident) = spec.child_by_field_name("name")
                        && let Ok(name) = ident.utf8_text(source)
                    {
                        exports.push((name.to_string(), None, Some(src.to_string())));
                    }
                }
            }
        }
        return;
    }

    // Check for export clause without 'from' (named exports of existing symbols)
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32)
            && child.kind() == "export_clause"
        {
            for j in 0..child.child_count() {
                if let Some(spec) = child.child(j as u32)
                    && spec.kind() == "export_specifier"
                    && let Some(ident) = spec.child_by_field_name("name")
                    && let Ok(name) = ident.utf8_text(source)
                {
                    exports.push((name.to_string(), None, None));
                }
            }
        }
    }

    // Check for declaration inside export: export function foo() {}
    let syms_before = symbols.len();
    walk_children(
        node,
        source,
        file_path,
        lang,
        symbols,
        edges,
        imports,
        exports,
        dedup,
        next_id,
        current_sym_id,
    );

    for sym in &symbols[syms_before..] {
        if !exports.iter().any(|(n, _, _)| n == &sym.name) {
            exports.push((sym.name.clone(), None, None));
        }
    }
}

// ------------------------------------------------------------------
//  Class body handling – extracts heritage edges
// ------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn walk_class_body(
    node: Node,
    source: &[u8],
    file_path: &str,
    lang: &str,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
    imports: &mut Vec<(String, String)>,
    exports: &mut Vec<(String, Option<u64>, Option<String>)>,
    dedup: &mut HashSet<(String, String)>,
    next_id: &mut u64,
    current_sym_id: Option<u64>,
) {
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            match child.kind() {
                "class_heritage" => {
                    walk_children(
                        child,
                        source,
                        file_path,
                        lang,
                        symbols,
                        edges,
                        imports,
                        exports,
                        dedup,
                        next_id,
                        current_sym_id,
                    );
                }
                _ => {
                    walk_node(
                        child,
                        source,
                        file_path,
                        lang,
                        symbols,
                        edges,
                        imports,
                        exports,
                        dedup,
                        next_id,
                        current_sym_id,
                    );
                }
            }
        }
    }
}

// ======================================================================
//  Tests
// ======================================================================

#[cfg(test)]
#[path = "parser_tests.rs"]
mod parser_tests;
