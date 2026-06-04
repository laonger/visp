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

#[cfg(test)]
mod tests {
    use super::*;

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
}
