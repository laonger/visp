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

#[derive(Debug, Clone, PartialEq)]
pub struct FileInfo {
    pub path: String,
    pub language: String,
    pub symbol_count: u32,
    pub last_indexed_at: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Symbol tests ---

    #[test]
    fn test_symbol_kind_variants() {
        let variants = [
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

    #[test]
    fn test_edge_kind_variants() {
        let variants = [
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
