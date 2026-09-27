//! `visp-hooks` 契约层：冻结 hook 事件模型、公共信封、env 变量名与 schema 版本（设计 §6.2/§6.3）。
//!
//! 本 crate **仅含契约**：零执行逻辑、零 IO、零配置。执行器/匹配/配置在后续阶段落地。

pub mod env;
pub mod event;

pub use env::*;
pub use event::*;
