#![cfg(test)]
use crate::graph::*;

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
