//! visp-daemon library crate.
//!
//! Library target that makes the crate's public API available to
//! integration tests and potential downstream users.  Re-exports the
//! observability and config modules.
//!
//! The daemon binary (`main.rs`) is a separate target.

pub mod bus;
pub mod config;
pub mod hook_map;
pub mod observability;
pub mod reload;
pub mod watch;
