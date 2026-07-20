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
#[path = "graph_tests.rs"]
mod graph_tests;
