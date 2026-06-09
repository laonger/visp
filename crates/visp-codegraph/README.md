# visp-codegraph — 代码图谱引擎

基于 tree-sitter 的代码解析引擎 + SQLite 索引，支持 TS/TSX/Rust/Python/C/C++/Go 语言的符号提取与查询。

## 关键文件

- `parser.rs` — tree-sitter 解析器
- `graph.rs` — 图数据结构
- `store.rs` — SQLite 持久化
- `index.rs` — 全量/增量索引
- `query.rs` — 符号查询

## 依赖

无内部 crate 依赖。

## 测试

```bash
cargo test -p visp-codegraph
```
