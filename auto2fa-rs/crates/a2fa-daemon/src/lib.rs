//! a2fa-daemon library — exposes all modules for integration testing.
//!
//! The actual `main` entry point lives in `main.rs`; this crate root lets
//! integration tests in `tests/` import `dispatch`, `server`, etc. without
//! going through the binary.

pub mod dispatch;
pub mod handlers;
pub mod managers;
pub mod server;
pub mod singleton;
pub mod subscribers;
pub mod tunnel_maintenance;
pub mod tunnel_runtime;
pub mod workers;

pub use a2fa_core::engine::State;
