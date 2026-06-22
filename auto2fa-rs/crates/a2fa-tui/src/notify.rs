//! Best-effort native desktop notifications and clipboard, mirroring the
//! Python TUI's `_system_notify` / `_fallback_clipboard`.
//!
//! Both fire on a background thread with a short timeout and swallow every
//! error — they must never block the UI thread or panic.

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;

/// Show a native macOS notification via `osascript`.
///
/// Runs on a detached thread, 2 s timeout, swallows all errors. No-op on
/// non-macOS platforms.
pub fn system_notify(title: &str, msg: &str) {
    if !cfg!(target_os = "macos") {
        return;
    }
    let title = title.to_string();
    let msg = msg.to_string();
    std::thread::spawn(move || {
        // Escape double-quotes so the AppleScript string literal stays valid.
        let safe_title = title.replace('"', "\\\"");
        let safe_msg = msg.replace('"', "\\\"");
        let script =
            format!("display notification \"{safe_msg}\" with title \"{safe_title}\"");
        let mut child = match Command::new("osascript")
            .arg("-e")
            .arg(script)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(_) => return,
        };
        // Crude timeout: poll for completion, kill after ~2 s.
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {
                    if std::time::Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(_) => break,
            }
        }
    });
}

/// Copy `text` to the system clipboard, best-effort, on a background thread.
///
/// Tries `pbcopy` (macOS) first, then `xclip` / `wl-copy` (Linux). Swallows
/// all errors and never blocks the UI thread.
pub fn copy_to_clipboard(text: &str) {
    let text = text.to_string();
    std::thread::spawn(move || {
        let candidates: &[&[&str]] = &[
            &["pbcopy"],
            &["xclip", "-selection", "clipboard"],
            &["wl-copy"],
        ];
        for cmd in candidates {
            let mut child = match Command::new(cmd[0])
                .args(&cmd[1..])
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
            match child.wait() {
                Ok(status) if status.success() => return,
                _ => continue,
            }
        }
    });
}
