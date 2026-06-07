//! Library surface of `a2fa-tui` — exposes the pure `app` module so that
//! integration tests can import and test the reducer without the terminal.

pub mod app;
pub mod client;
pub mod views;
