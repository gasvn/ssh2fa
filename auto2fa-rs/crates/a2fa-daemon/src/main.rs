//! a2fa-daemon entry point.
//!
//! Initialises logging then delegates to `server::run()`.

use simplelog::{Config, LevelFilter, WriteLogger};
use std::fs::OpenOptions;

fn main() {
    // Rotate the log before the logger opens the file (mirrors daemon.py).
    a2fa_daemon::log_rotation::rotate_daemon_log();

    // Log to /tmp/ssh2fa_daemon.log (append mode, matching daemon.py).
    // Degrade, never crash: if the log file can't be opened or the logger fails
    // to init, fall back to stderr (captured by launchd) and CONTINUE. Panicking
    // here would exit before any work → launchd KeepAlive respawns → a fast
    // crashloop the user can't escape, all to avoid... not having a log file.
    match OpenOptions::new()
        .create(true)
        .append(true)
        .open("/tmp/ssh2fa_daemon.log")
    {
        Ok(log_file) => {
            if WriteLogger::init(LevelFilter::Info, Config::default(), log_file).is_err() {
                let _ = WriteLogger::init(LevelFilter::Info, Config::default(), std::io::stderr());
            }
        }
        Err(e) => {
            eprintln!("[daemon] cannot open /tmp/ssh2fa_daemon.log ({e}); logging to stderr");
            let _ = WriteLogger::init(LevelFilter::Info, Config::default(), std::io::stderr());
        }
    }

    // Keep rotating while running — the boot check alone let a weeks-long
    // daemon grow the log without bound (safe: the logger fd is O_APPEND).
    a2fa_daemon::log_rotation::spawn_periodic_rotation();

    // launchd hands user agents a soft RLIMIT_NOFILE of only 256, which a
    // long-lived SSH/pty/tunnel daemon can exhaust — after which every spawn
    // fails with "Too many open files" and the daemon retry-storms. Raise it
    // before doing any real work. Best-effort; logs the outcome.
    a2fa_core::sys::raise_fd_limit();

    if let Err(e) = a2fa_daemon::server::run() {
        log::error!("daemon exited with error: {e:#}");
        eprintln!("daemon error: {e:#}");
        std::process::exit(1);
    }
}
