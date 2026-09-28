//! `visp-hooks` 契约层与执行器决策内核：冻结 hook 事件模型、公共信封、env 变量名与
//! schema 版本（设计 §6.2/§6.3），并提供规则匹配/排序/队列策略/cooldown 的纯决策内核
//! （设计 D4 / §9）。
//!
//! 契约层零执行逻辑、零 IO；决策内核不做进程 spawn/IO，副作用经 [`dispatcher::Handler`]
//! 由上层注入。配置（`HookRule`）与进程执行在后续阶段接线。

pub mod dispatcher;
pub mod env;
pub mod event;

pub use dispatcher::*;
pub use env::*;
pub use event::*;
