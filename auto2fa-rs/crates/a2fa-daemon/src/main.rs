//! a2fa-daemon entry point.
//!
//! Initialises logging then delegates to `server::run()`.

use simplelog::{Config, LevelFilter, WriteLogger};
use std::fs::OpenOptions;

fn main() {
    // Rotate the log before the logger opens the file (mirrors daemon.py).
    a2fa_daemon::log_rotation::rotate_daemon_log();

    // Log to /tmp/auto2fa_daemon.log (append mode, matching daemon.py).
    let log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open("/tmp/auto2fa_daemon.log")
        .expect("cannot open /tmp/auto2fa_daemon.log");

    WriteLogger::init(LevelFilter::Info, Config::default(), log_file)
        .expect("logger init failed");

    if let Err(e) = a2fa_daemon::server::run() {
        log::error!("daemon exited with error: {e:#}");
        eprintln!("daemon error: {e:#}");
        std::process::exit(1);
    }
}
