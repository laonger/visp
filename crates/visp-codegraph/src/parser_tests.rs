#![cfg(test)]
use crate::graph::{EdgeKind, SymbolKind};
use crate::parser::*;

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

#[test]
fn test_rust_function() {
    let mut p = setup();
    let res = p
        .parse_file("test.rs", "fn hello() -> i32 { 42 }\n")
        .unwrap();
    assert_eq!(
        res.symbols.len(),
        1,
        "expected 1 symbol, got {:?}",
        res.symbols
    );
    assert_eq!(res.symbols[0].name, "hello");
    assert_eq!(res.symbols[0].kind, SymbolKind::Function);
}

#[test]
fn test_python_function() {
    let mut p = setup();
    let res = p
        .parse_file("test.py", "def hello():\n    return 42\n")
        .unwrap();
    assert_eq!(res.symbols.len(), 1);
    assert_eq!(res.symbols[0].kind, SymbolKind::Function);
}

#[test]
fn test_python_class() {
    let mut p = setup();
    let res = p.parse_file("test.py", "class Foo:\n    pass\n").unwrap();
    assert_eq!(res.symbols.len(), 1);
    assert_eq!(res.symbols[0].name, "Foo");
    assert_eq!(res.symbols[0].kind, SymbolKind::Class);
}

#[test]
fn test_python_method_in_class() {
    let mut p = setup();
    let res = p
        .parse_file("test.py", "class A:\n    def bar(self):\n        pass\n")
        .unwrap();
    let class: Vec<_> = res
        .symbols
        .iter()
        .filter(|s| s.kind == SymbolKind::Class)
        .collect();
    assert_eq!(class.len(), 1, "expected 1 class");
    assert_eq!(class[0].name, "A");
    let methods: Vec<_> = res
        .symbols
        .iter()
        .filter(|s| s.kind == SymbolKind::Method)
        .collect();
    assert_eq!(methods.len(), 1, "expected 1 method");
    assert_eq!(methods[0].name, "bar");
}

#[test]
fn test_python_method_outside_class() {
    let mut p = setup();
    let res = p
        .parse_file("test.py", "def outer():\n    def inner():\n        pass\n")
        .unwrap();
    let functions: Vec<_> = res
        .symbols
        .iter()
        .filter(|s| s.kind == SymbolKind::Function)
        .collect();
    assert_eq!(functions.len(), 2, "expected 2 functions");
    assert_eq!(functions[0].name, "outer");
    assert_eq!(
        functions[1].name, "inner",
        "nested def should be Function, not Method"
    );
}

#[test]
fn test_go_function() {
    let mut p = setup();
    let res = p
        .parse_file("test.go", "package p\nfunc foo() {}\n")
        .unwrap();
    assert_eq!(res.symbols.len(), 1);
    assert_eq!(res.symbols[0].name, "foo");
    assert_eq!(res.symbols[0].kind, SymbolKind::Function);
}

#[test]
fn test_go_method() {
    let mut p = setup();
    let res = p
        .parse_file("test.go", "package p\nfunc (r *R) Foo() {}\n")
        .unwrap();
    let methods: Vec<_> = res
        .symbols
        .iter()
        .filter(|s| s.kind == SymbolKind::Method)
        .collect();
    assert_eq!(methods.len(), 1, "expected 1 method, got {:?}", res.symbols);
    assert_eq!(methods[0].name, "Foo");
}

#[test]
fn test_go_const() {
    let mut p = setup();
    let res = p.parse_file("test.go", "package p\nconst X = 1\n").unwrap();
    let vars: Vec<_> = res
        .symbols
        .iter()
        .filter(|s| s.kind == SymbolKind::Variable)
        .collect();
    assert_eq!(vars.len(), 1, "expected 1 variable, got {:?}", res.symbols);
    assert_eq!(vars[0].name, "X");
}

#[test]
fn test_rust_const_item() {
    let mut p = setup();
    let res = p.parse_file("test.rs", "const FOO: i32 = 42;\n").unwrap();
    let vars: Vec<_> = res
        .symbols
        .iter()
        .filter(|s| s.kind == SymbolKind::Variable)
        .collect();
    assert_eq!(vars.len(), 1, "expected 1 variable, got {:?}", res.symbols);
    assert_eq!(vars[0].name, "FOO");
}

#[test]
fn test_rust_static_item() {
    let mut p = setup();
    let res = p.parse_file("test.rs", "static BAR: i32 = 42;\n").unwrap();
    let vars: Vec<_> = res
        .symbols
        .iter()
        .filter(|s| s.kind == SymbolKind::Variable)
        .collect();
    assert_eq!(vars.len(), 1, "expected 1 variable, got {:?}", res.symbols);
    assert_eq!(vars[0].name, "BAR");
}

#[test]
fn test_ts_interface_method_signature() {
    let mut p = setup();
    let res = p
        .parse_file("test.ts", "interface I { foo(): void }\n")
        .unwrap();
    let methods: Vec<_> = res
        .symbols
        .iter()
        .filter(|s| s.kind == SymbolKind::Method)
        .collect();
    assert_eq!(methods.len(), 1, "expected 1 method");
    assert_eq!(methods[0].name, "foo");
}

#[test]
fn test_ts_generator() {
    let mut p = setup();
    let res = p.parse_file("test.ts", "function* gen() {}\n").unwrap();
    assert_eq!(res.symbols.len(), 1, "expected 1 symbol");
    assert_eq!(res.symbols[0].name, "gen");
    assert_eq!(res.symbols[0].kind, SymbolKind::Function);
}

#[test]
fn test_c_function() {
    let mut p = setup();
    let res = p.parse_file("test.c", "void f() {}\n").unwrap();
    assert_eq!(res.symbols.len(), 1);
    assert_eq!(res.symbols[0].name, "f");
    assert_eq!(res.symbols[0].kind, SymbolKind::Function);
}

#[test]
fn test_c_struct() {
    let mut p = setup();
    let res = p.parse_file("test.c", "struct S { int x; };\n").unwrap();
    let classes: Vec<_> = res
        .symbols
        .iter()
        .filter(|s| s.kind == SymbolKind::Class)
        .collect();
    assert_eq!(classes.len(), 1);
    assert_eq!(classes[0].name, "S");
}

#[test]
fn test_cpp_class() {
    let mut p = setup();
    let res = p.parse_file("test.cpp", "class C {};\n").unwrap();
    let classes: Vec<_> = res
        .symbols
        .iter()
        .filter(|s| s.kind == SymbolKind::Class)
        .collect();
    assert_eq!(classes.len(), 1);
    assert_eq!(classes[0].name, "C");
}
