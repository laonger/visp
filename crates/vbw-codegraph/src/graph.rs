#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SymbolKind {
    Function,
    Method,
    Class,
    Interface,
    TypeAlias,
    Variable,
    Enum,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Symbol {
    pub id: u64,
    pub name: String,
    pub kind: SymbolKind,
    pub file_path: String,
    pub line: u32,
    pub column: u32,
    pub signature: Option<String>,
    pub docstring: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EdgeKind {
    Call,
    Reference,
    Implementation,
    Inheritance,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Edge {
    pub source_id: u64,
    pub target_id: Option<u64>,
    pub target_name: Option<String>,
    pub kind: EdgeKind,
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Symbol tests ---

    #[test]
    fn test_symbol_creation() {
        let sym = Symbol {
            id: 1,
            name: "foo".into(),
            kind: SymbolKind::Function,
            file_path: "src/lib.rs".into(),
            line: 10,
            column: 4,
            signature: Some("fn foo(x: i32) -> i32".into()),
            docstring: None,
        };
        assert_eq!(sym.id, 1);
        assert_eq!(sym.name, "foo");
        assert_eq!(sym.kind, SymbolKind::Function);
        assert_eq!(sym.file_path, "src/lib.rs");
        assert_eq!(sym.line, 10);
        assert_eq!(sym.column, 4);
        assert_eq!(sym.signature, Some("fn foo(x: i32) -> i32".into()));
        assert_eq!(sym.docstring, None);
    }

    #[test]
    fn test_symbol_kind_variants() {
        let variants = vec![
            SymbolKind::Function,
            SymbolKind::Method,
            SymbolKind::Class,
            SymbolKind::Interface,
            SymbolKind::TypeAlias,
            SymbolKind::Variable,
            SymbolKind::Enum,
        ];
        assert_eq!(variants.len(), 7);
        for (i, a) in variants.iter().enumerate() {
            for (j, b) in variants.iter().enumerate() {
                if i != j {
                    assert_ne!(a, b);
                }
            }
        }
    }

    // --- Edge tests ---

    #[test]
    fn test_edge_resolved() {
        let edge = Edge {
            source_id: 1,
            target_id: Some(5),
            target_name: None,
            kind: EdgeKind::Call,
        };
        assert_eq!(edge.source_id, 1);
        assert_eq!(edge.target_id, Some(5));
        assert_eq!(edge.target_name, None);
        assert_eq!(edge.kind, EdgeKind::Call);
    }

    #[test]
    fn test_edge_unresolved() {
        let edge = Edge {
            source_id: 3,
            target_id: None,
            target_name: Some("foo".into()),
            kind: EdgeKind::Reference,
        };
        assert_eq!(edge.source_id, 3);
        assert_eq!(edge.target_id, None);
        assert_eq!(edge.target_name, Some("foo".into()));
        assert_eq!(edge.kind, EdgeKind::Reference);
    }

    #[test]
    fn test_edge_kind_variants() {
        let variants = vec![
            EdgeKind::Call,
            EdgeKind::Reference,
            EdgeKind::Implementation,
            EdgeKind::Inheritance,
        ];
        assert_eq!(variants.len(), 4);
        for (i, a) in variants.iter().enumerate() {
            for (j, b) in variants.iter().enumerate() {
                if i != j {
                    assert_ne!(a, b);
                }
            }
        }
    }
}
