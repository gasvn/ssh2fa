//! Unix-socket RPC client.
//!
//! Mirrors the Python `_rpc` helper in `cli.py`:
//! - Connects to `AUTO2FA_SOCK` or `~/.auto2fa/auto2fa.sock`.
//! - Sets a 30-second read/write timeout.
//! - Sends `{"id","method","params"}\n`, reads one newline-terminated line.
//! - Returns the `result` value on success, or a friendly `anyhow::Error`
//!   containing the daemon's error message.

use std::io::{BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
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

/// Connect to the daemon socket, send one RPC request, return the `result`
/// value.  On daemon error the daemon's `error.message` is wrapped into an
/// `Err`.
///
/// Friendly errors for:
/// - socket not found (daemon not running)
/// - connection refused
/// - 30-second timeout
/// - broken pipe / lost connection
/// - malformed JSON response
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
    stream
        .set_read_timeout(Some(timeout))
        .context("set_read_timeout")?;
    stream
        .set_write_timeout(Some(timeout))
        .context("set_write_timeout")?;

    // Build and send the request.
    let id = format!("{:x}", {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
    });
    let req = serde_json::json!({ "id": id, "method": method, "params": params });
    let mut line = req.to_string();
    line.push('\n');

    let mut writer = stream.try_clone().context("clone stream for write")?;
    writer
        .write_all(line.as_bytes())
        .context("lost connection to daemon while sending request")?;
    // Signal that we are done writing so the daemon can detect EOF on its
    // read half without us having to keep the write end open.
    let _ = writer.shutdown(Shutdown::Write);

    // Read one newline-terminated response line.
    let mut reader = BufReader::new(stream);
    let mut buf = String::new();
    reader.read_line(&mut buf).map_err(|e| {
        if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut {
            anyhow!(
                "daemon did not respond within 30 s — it may be wedged; \
                 try again or restart the app"
            )
        } else {
            anyhow!("lost connection to daemon: {}", e)
        }
    })?;

    if buf.trim().is_empty() {
        bail!("daemon closed connection without responding");
    }

    // Parse response.
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
// Tests (no daemon needed)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_path_default() {
        std::env::remove_var("AUTO2FA_SOCK");
        let p = socket_path();
        assert!(p.to_string_lossy().contains(".auto2fa/auto2fa.sock"));
    }

    #[test]
    fn socket_path_override() {
        std::env::set_var("AUTO2FA_SOCK", "/tmp/test.sock");
        let p = socket_path();
        assert_eq!(p, PathBuf::from("/tmp/test.sock"));
        std::env::remove_var("AUTO2FA_SOCK");
    }

    #[test]
    fn rpc_missing_socket_returns_err() {
        std::env::set_var("AUTO2FA_SOCK", "/tmp/a2fa_cli_test_nonexistent.sock");
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
