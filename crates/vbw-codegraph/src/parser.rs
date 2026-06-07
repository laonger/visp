use std::collections::HashSet;
use std::error::Error;

use tree_sitter::{Node, Parser as TsParser};

use crate::graph::{Edge, EdgeKind, Symbol, SymbolKind};

pub struct Parser {
    parser: TsParser,
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
        let mut parser = TsParser::new();
        let lang = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
        parser.set_language(&lang)?;
        Ok(Self { parser })
    }

    pub fn parse_file(
        &mut self,
        file_path: &str,
        content: &str,
    ) -> Result<ParseResult, Box<dyn Error>> {
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
        let mut dedup: HashSet<String> = HashSet::new();
        let mut next_id: u64 = 0;

        walk_children(
            root,
            source,
            file_path,
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

// ------------------------------------------------------------------
//  Free helper functions (no Self:: needed, avoids too_many_arguments on Parser impl)
// ------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn walk_children(
    node: Node,
    source: &[u8],
    file_path: &str,
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
    imports: &mut Vec<(String, String)>,
    exports: &mut Vec<(String, Option<u64>, Option<String>)>,
    dedup: &mut HashSet<String>,
    next_id: &mut u64,
    current_sym_id: Option<u64>,
) {
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            walk_node(
                child,
                source,
                file_path,
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
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
    imports: &mut Vec<(String, String)>,
    exports: &mut Vec<(String, Option<u64>, Option<String>)>,
    dedup: &mut HashSet<String>,
    next_id: &mut u64,
    current_sym_id: Option<u64>,
) {
    match node.kind() {
        // --- Symbol declarations ---
        "function_declaration" => {
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
                node, source, file_path, symbols, edges, imports, exports, dedup, next_id, sym_id,
            );
        }

        "class_declaration" => {
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
            walk_class_body(
                node, source, file_path, symbols, edges, imports, exports, dedup, next_id, sym_id,
            );
        }

        "method_definition" => {
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
                node, source, file_path, symbols, edges, imports, exports, dedup, next_id, sym_id,
            );
        }

        "interface_declaration" => {
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
                symbols,
                edges,
                imports,
                exports,
                dedup,
                next_id,
                current_sym_id,
            );
        }

        "type_alias_declaration" => {
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
                symbols,
                edges,
                imports,
                exports,
                dedup,
                next_id,
                current_sym_id,
            );
        }

        "enum_declaration" => {
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
        "lexical_declaration" | "variable_declaration" => {
            handle_variable_declaration(
                node,
                source,
                file_path,
                symbols,
                edges,
                imports,
                exports,
                dedup,
                next_id,
                current_sym_id,
            );
        }

        // Arrow functions assigned to variables are handled above.
        // A bare arrow_function node gets walked for inner edges only.
        "arrow_function" => {
            walk_children(
                node,
                source,
                file_path,
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
        "call_expression" => {
            if let Some(func) = node.child_by_field_name("function")
                && let Ok(name) = func.utf8_text(source)
            {
                add_edge(current_sym_id, EdgeKind::Call, name, edges);
            }
            walk_children(
                node,
                source,
                file_path,
                symbols,
                edges,
                imports,
                exports,
                dedup,
                next_id,
                current_sym_id,
            );
        }

        "new_expression" => {
            if let Some(ctor) = node.child_by_field_name("constructor")
                && let Ok(name) = ctor.utf8_text(source)
            {
                add_edge(current_sym_id, EdgeKind::Call, name, edges);
            }
            walk_children(
                node,
                source,
                file_path,
                symbols,
                edges,
                imports,
                exports,
                dedup,
                next_id,
                current_sym_id,
            );
        }

        "extends_clause" => {
            for i in 0..node.child_count() {
                if let Some(child) = node.child(i as u32)
                    && child.is_named()
                    && let Ok(name) = child.utf8_text(source)
                {
                    add_edge(current_sym_id, EdgeKind::Inheritance, name, edges);
                }
            }
        }

        "implements_clause" => {
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
        "import_statement" => {
            handle_import(node, source, imports);
            walk_children(
                node,
                source,
                file_path,
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
        "export_statement" => {
            handle_export(
                node,
                source,
                file_path,
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
            walk_children(
                node,
                source,
                file_path,
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
    dedup: &mut HashSet<String>,
    next_id: &mut u64,
) -> u64 {
    if dedup.contains(name) {
        symbols
            .iter()
            .find(|s| s.name == name)
            .map(|s| s.id)
            .unwrap_or(*next_id)
    } else {
        dedup.insert(name.to_string());
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
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
    imports: &mut Vec<(String, String)>,
    exports: &mut Vec<(String, Option<u64>, Option<String>)>,
    dedup: &mut HashSet<String>,
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
                } else if !dedup.contains(name) {
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
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
    imports: &mut Vec<(String, String)>,
    exports: &mut Vec<(String, Option<u64>, Option<String>)>,
    dedup: &mut HashSet<String>,
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
    symbols: &mut Vec<Symbol>,
    edges: &mut Vec<Edge>,
    imports: &mut Vec<(String, String)>,
    exports: &mut Vec<(String, Option<u64>, Option<String>)>,
    dedup: &mut HashSet<String>,
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
mod tests {
    use super::*;

    fn setup() -> Parser {
        Parser::new().expect("Failed to create parser")
    }

    #[test]
    fn test_function_declaration() {
        let mut p = setup();
        let res = p.parse_file("test.ts", "function foo() {}\n").unwrap();
        assert_eq!(res.symbols.len(), 1);
        assert_eq!(res.symbols[0].name, "foo");
        assert_eq!(res.symbols[0].kind, SymbolKind::Function);
    }

    #[test]
    fn test_class_declaration() {
        let mut p = setup();
        let res = p.parse_file("test.ts", "class Bar {}\n").unwrap();
        assert_eq!(res.symbols.len(), 1);
        assert_eq!(res.symbols[0].name, "Bar");
        assert_eq!(res.symbols[0].kind, SymbolKind::Class);
    }

    #[test]
    fn test_variable_declaration() {
        let mut p = setup();
        let res = p.parse_file("test.ts", "const x = 1;\n").unwrap();
        assert_eq!(res.symbols.len(), 1);
        assert_eq!(res.symbols[0].name, "x");
        assert_eq!(res.symbols[0].kind, SymbolKind::Variable);
    }

    #[test]
    fn test_arrow_function() {
        let mut p = setup();
        let res = p.parse_file("test.ts", "const fn = () => {};\n").unwrap();
        assert_eq!(res.symbols.len(), 1);
        assert_eq!(res.symbols[0].name, "fn");
        assert_eq!(res.symbols[0].kind, SymbolKind::Function);
    }

    #[test]
    fn test_method() {
        let mut p = setup();
        let res = p.parse_file("test.ts", "class A { m() {} }\n").unwrap();
        let methods: Vec<_> = res
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Method)
            .collect();
        assert_eq!(methods.len(), 1);
        assert_eq!(methods[0].name, "m");
        assert_eq!(methods[0].kind, SymbolKind::Method);
    }

    #[test]
    fn test_interface() {
        let mut p = setup();
        let res = p.parse_file("test.ts", "interface I {}\n").unwrap();
        assert_eq!(res.symbols.len(), 1);
        assert_eq!(res.symbols[0].name, "I");
        assert_eq!(res.symbols[0].kind, SymbolKind::Interface);
    }

    #[test]
    fn test_type_alias() {
        let mut p = setup();
        let res = p.parse_file("test.ts", "type T = string;\n").unwrap();
        assert_eq!(res.symbols.len(), 1);
        assert_eq!(res.symbols[0].name, "T");
        assert_eq!(res.symbols[0].kind, SymbolKind::TypeAlias);
    }

    #[test]
    fn test_enum() {
        let mut p = setup();
        let res = p.parse_file("test.ts", "enum E { A, B }\n").unwrap();
        assert_eq!(res.symbols.len(), 1);
        assert_eq!(res.symbols[0].name, "E");
        assert_eq!(res.symbols[0].kind, SymbolKind::Enum);
    }

    #[test]
    fn test_call_edge() {
        let mut p = setup();
        let res = p
            .parse_file("test.ts", "function caller() { foo(); }\n")
            .unwrap();
        let calls: Vec<_> = res
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Call)
            .collect();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].target_name, Some("foo".into()));
        assert_eq!(calls[0].kind, EdgeKind::Call);
    }

    #[test]
    fn test_import_statement() {
        let mut p = setup();
        let res = p
            .parse_file("test.ts", "import { bar } from './other';\n")
            .unwrap();
        assert_eq!(res.imports.len(), 1);
        assert_eq!(res.imports[0], ("bar".to_string(), "./other".to_string()));
    }

    #[test]
    fn test_export_statement() {
        let mut p = setup();
        let res = p
            .parse_file("test.ts", "export function baz() {}\n")
            .unwrap();
        assert!(!res.exports.is_empty(), "expected at least one export");
        let found = res
            .exports
            .iter()
            .any(|(n, _, re)| n == "baz" && re.is_none());
        assert!(found, "export 'baz' not found in {:?}", res.exports);
    }

    #[test]
    fn test_re_export() {
        let mut p = setup();
        let res = p
            .parse_file("test.ts", "export { qux } from './other';\n")
            .unwrap();
        assert!(!res.exports.is_empty(), "expected at least one export");
        let found = res
            .exports
            .iter()
            .any(|(n, _, re)| n == "qux" && re.as_deref() == Some("./other"));
        assert!(
            found,
            "re-export 'qux from ./other' not found in {:?}",
            res.exports
        );
    }

    #[test]
    fn test_inheritance() {
        let mut p = setup();
        let res = p.parse_file("test.ts", "class D extends E {}\n").unwrap();
        let inh: Vec<_> = res
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Inheritance)
            .collect();
        assert_eq!(inh.len(), 1);
        assert_eq!(inh[0].target_name, Some("E".into()));
        assert_eq!(inh[0].kind, EdgeKind::Inheritance);
    }

    #[test]
    fn test_syntax_error() {
        let mut p = setup();
        let res = p.parse_file("test.ts", "function foo( {}\n"); // missing )
        assert!(res.is_err(), "expected Err for syntax error");
    }

    #[test]
    fn test_empty_file() {
        let mut p = setup();
        let res = p.parse_file("test.ts", "").unwrap();
        assert_eq!(res.symbols.len(), 0);
        assert_eq!(res.edges.len(), 0);
        assert_eq!(res.imports.len(), 0);
        assert_eq!(res.exports.len(), 0);
    }
}
