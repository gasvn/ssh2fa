//! Unix-socket RPC client for the TUI.
//!
//! Mirrors the CLI client (`a2fa-cli/src/client.rs`) with an additional
//! `subscribe` function that opens a *second* persistent connection, sends
//! `subscribe_events`, and forwards each newline-delimited event JSON to the
//! UI via an `mpsc` channel.

use std::io::{BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;

// ---------------------------------------------------------------------------
// Socket path
// ---------------------------------------------------------------------------

/// Return the socket path, honoring `AUTO2FA_SOCK` if set.
pub fn socket_path() -> PathBuf {
    if let Ok(v) = std::env::var("AUTO2FA_SOCK") {
        return PathBuf::from(v);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".auto2fa").join("auto2fa.sock")
}

// ---------------------------------------------------------------------------
// One-shot RPC
// ---------------------------------------------------------------------------

/// Connect, send one request, return the `result` value.
pub fn rpc(method: &str, params: Value) -> Result<Value> {
    let path = socket_path();

    if !path.exists() {
        bail!(
            "daemon socket not found at {} — is the daemon running?",
            path.display()
        );
    }

    let stream = UnixStream::connect(&path).with_context(|| {
        format!(
            "connect failed — is the daemon running? ({})",
            path.display()
        )
    })?;

    let timeout = Duration::from_secs(30);
    stream.set_read_timeout(Some(timeout)).context("set_read_timeout")?;
    stream.set_write_timeout(Some(timeout)).context("set_write_timeout")?;

    // Unique-enough request id.
    let id = {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
    };
    let req = serde_json::json!({ "id": id, "method": method, "params": params });
    let mut line = req.to_string();
    line.push('\n');

    let mut writer = stream.try_clone().context("clone stream")?;
    writer.write_all(line.as_bytes()).context("send request")?;
    let _ = writer.shutdown(Shutdown::Write);

    let mut reader = BufReader::new(stream);
    let mut buf = String::new();
    reader.read_line(&mut buf).map_err(|e| {
        if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut {
            anyhow!("daemon did not respond within 30 s")
        } else {
            anyhow!("lost connection to daemon: {}", e)
        }
    })?;

    if buf.trim().is_empty() {
        bail!("daemon closed connection without responding");
    }

    let resp: Value = serde_json::from_str(buf.trim_end())
        .with_context(|| format!("daemon sent malformed response: {buf}"))?;

    if let Some(err) = resp.get("error") {
        let msg = err
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown daemon error");
        bail!("daemon error: {}", msg);
    }

    Ok(resp.get("result").cloned().unwrap_or(Value::Null))
}

// ---------------------------------------------------------------------------
// Event subscription
// ---------------------------------------------------------------------------

/// Open a persistent connection, send `subscribe_events`, and forward each
/// event line as a `serde_json::Value` to `tx`.
///
/// Intended to run on a background thread.  Returns when the connection is
/// closed (daemon stopped) or when the channel is hung up.
///
/// Caller is responsible for spawning the thread and providing the `tx` end of
/// an `mpsc` channel (or `crossbeam_channel`, etc.).
pub fn subscribe(tx: Sender<Value>) -> Result<()> {
    let path = socket_path();

    if !path.exists() {
        bail!(
            "daemon socket not found at {} — is the daemon running?",
            path.display()
        );
    }

    let stream = UnixStream::connect(&path).with_context(|| {
        format!("subscribe connect failed ({})", path.display())
    })?;

    // No read timeout on the subscriber connection — events are push-based.
    stream.set_write_timeout(Some(Duration::from_secs(5))).ok();

    let req = serde_json::json!({
        "id": "sub",
        "method": "subscribe_events",
        "params": {}
    });
    let mut line = req.to_string();
    line.push('\n');

    let mut writer = stream.try_clone().context("clone subscribe stream")?;
    writer.write_all(line.as_bytes()).context("send subscribe request")?;
    // Do NOT shutdown the write half — the daemon keeps streaming events.

    let reader = BufReader::new(stream);
    for raw in reader.lines() {
        match raw {
            Err(_) => break, // connection closed
            Ok(text) => {
                if text.trim().is_empty() {
                    continue;
                }
                if let Ok(v) = serde_json::from_str::<Value>(&text) {
                    if tx.send(v).is_err() {
                        // Receiver dropped; the TUI is shutting down.
                        break;
                    }
                }
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests (no daemon needed)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_path_default() {
        std::env::remove_var("AUTO2FA_SOCK");
        let p = socket_path();
        assert!(
            p.to_string_lossy().contains(".auto2fa/auto2fa.sock"),
            "unexpected path: {p:?}"
        );
    }

    #[test]
    fn socket_path_override() {
        std::env::set_var("AUTO2FA_SOCK", "/tmp/tui_test.sock");
        let p = socket_path();
        assert_eq!(p, PathBuf::from("/tmp/tui_test.sock"));
        std::env::remove_var("AUTO2FA_SOCK");
    }

    #[test]
    fn rpc_missing_socket_returns_err() {
        std::env::set_var("AUTO2FA_SOCK", "/tmp/a2fa_tui_test_nonexistent.sock");
        let r = rpc("ping", serde_json::json!({}));
        std::env::remove_var("AUTO2FA_SOCK");
        assert!(r.is_err());
        let msg = r.unwrap_err().to_string();
        assert!(
            msg.contains("daemon") || msg.contains("socket"),
            "unexpected error: {msg}"
        );
    }
}
