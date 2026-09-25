//! 跨平台文件监听模块 `visp-fs`（设计 §4）。
//!
//! 依赖方向约束：本 crate **不得**依赖 `visp-core` / `visp-config` /
//! `visp-daemon` / `visp-codegraph` / `visp-agent`；对外契约只含「路径 + 事件类型」。
//!
//! Wave 1 只交付**纯逻辑核心**（无 IO、无 notify 接线）。

pub mod degrade;
pub mod normalize;
pub mod target;
