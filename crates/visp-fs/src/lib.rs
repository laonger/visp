//! 跨平台文件监听模块 `visp-fs`（设计 §4）。
//!
//! 依赖方向约束：本 crate **不得**依赖 `visp-core` / `visp-config` /
//! `visp-daemon` / `visp-codegraph` / `visp-agent`；对外契约只含「路径 + 事件类型」。
//!
//! Wave 1 交付**纯逻辑核心**（无 IO、无 notify 接线）；Wave 2 增加
//! [`runtime`]（notify 接线 + 目录/文件双监听 + 动态补挂 + 适配层）。

pub mod degrade;
pub mod normalize;
pub mod runtime;
pub mod target;
