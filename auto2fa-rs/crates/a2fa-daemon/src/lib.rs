//! a2fa-daemon library — exposes all modules for integration testing.
//!
//! The actual `main` entry point lives in `main.rs`; this crate root lets
//! integration tests in `tests/` import `dispatch`, `server`, etc. without
//! going through the binary.

pub mod dispatch;
pub mod handlers;
pub mod log_rotation;
pub mod managers;
pub mod server;
pub mod singleton;
pub mod subscribers;
pub mod tunnel_maintenance;
pub mod tunnel_runtime;
pub mod workers;

pub use a2fa_core::engine::State;

use std::sync::{Mutex, MutexGuard};

/// Poison-tolerant lock of the engine `State`.
///
/// The core daemon loops (heartbeat, tunnel-maintenance) run for the process's
/// whole lifetime. If any worker panics while holding `Mutex<State>`, the lock
/// is poisoned and a plain `.lock().unwrap()` would propagate that panic into
/// the loop thread, silently killing it. Since the panicking worker has already
/// finished mutating `State`, the data is consistent enough to keep going; the
/// safe choice is to recover the guard and carry on rather than wedge the loop.
///
/// On `Ok` returns the guard; on `Err(poisoned)` recovers via `into_inner()`
/// and warns once so the poisoning is visible in the logs.
///
/// Mirrors the recovery pattern already used in
/// `a2fa_core::engine::tick::run_tick` and `HostManagers::teardown_all`.
pub fn lock_state<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    match m.lock() {
        Ok(g) => g,
        Err(poisoned) => {
            log::warn!("lock_state: State mutex was poisoned — recovering and continuing");
            poisoned.into_inner()
        }
    }
}
