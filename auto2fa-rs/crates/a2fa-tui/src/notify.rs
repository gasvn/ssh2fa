//! Best-effort native desktop notifications and clipboard, mirroring the
//! Python TUI's `_system_notify` / `_fallback_clipboard`.
//!
//! Both fire on a background thread with a short timeout and swallow every
//! error — they must never block the UI thread or panic. Which tool to run is
//! `a2fa_core::platform`'s business (`osascript`/`pbcopy` on macOS,
//! `notify-send` + `wl-copy`/`xclip`/`xsel` on Linux); a headless box where
//! none of them exist is a supported outcome, not an error.

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;

/// Run `child` to completion, killing it after `timeout`. Returns whether it
/// exited successfully.
fn wait_bounded(child: &mut std::process::Child, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return false;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => return false,
        }
    }
}

/// Show a native desktop notification.
///
/// Runs on a detached thread with a 2 s timeout and swallows all errors — on a
/// headless server the notifier simply isn't installed, which is fine.
pub fn system_notify(title: &str, msg: &str) {
    let Some((cmd, args)) = a2fa_core::platform::notify_command(title, msg) else {
        return;
    };
    std::thread::spawn(move || {
        let mut child = match Command::new(cmd)
            .args(&args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(_) => return, // notifier not installed
        };
        wait_bounded(&mut child, Duration::from_secs(2));
    });
}

/// Copy `text` to the system clipboard, best-effort, on a background thread.
///
/// Tries this platform's tools in order and stops at the first that succeeds.
/// Every one of them may be missing (a headless box, or Wayland tools on an X11
/// session); the copy is then silently skipped rather than failing the UI.
pub fn copy_to_clipboard(text: &str) {
    let text = text.to_string();
    std::thread::spawn(move || {
        for (cmd, args) in a2fa_core::platform::clipboard_commands() {
            let mut child = match Command::new(cmd)
                .args(*args)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                Ok(c) => c,
                Err(_) => continue, // tool not installed; try the next
            };
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
                // Drop stdin so the child sees EOF.
            }
            // Bounded: wl-copy on a session with no compositor can hang, and a
            // stuck clipboard helper must not leak a thread per copy.
            if wait_bounded(&mut child, Duration::from_secs(3)) {
                return;
            }
        }
    });
}
