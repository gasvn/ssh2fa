//! Unix-socket RPC client.
//!
//! Mirrors the Python `_rpc` helper in `cli.py`:
//! - Connects to `AUTO2FA_SOCK` or `~/.ssh2fa/ssh2fa.sock`.
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
    PathBuf::from(home).join(".ssh2fa").join("ssh2fa.sock")
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

    // Read newline-terminated frames until OUR response arrives. A subscribed
    // connection (`raw subscribe_events`) can have broadcast `{"event": …}`
    // frames interleaved ahead of the ack — treating the first line as the
    // response made the CLI print an event as if it were the result. Skip
    // event frames and frames whose id doesn't match ours (bounded, so a
    // chatty daemon can't loop us forever).
    let mut reader = BufReader::new(stream);
    let resp: Value = {
        const MAX_SKIPPED_FRAMES: usize = 256;
        let mut skipped = 0usize;
        loop {
            let mut buf = String::new();
            reader.read_line(&mut buf).map_err(|e| {
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut
                {
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

            let frame: Value = serde_json::from_str(buf.trim_end())
                .with_context(|| format!("daemon sent malformed response: {buf}"))?;

            // Event frame → not our response; keep reading.
            if frame.get("event").is_some() {
                skipped += 1;
                if skipped > MAX_SKIPPED_FRAMES {
                    bail!("daemon flooded {MAX_SKIPPED_FRAMES} event frames without answering");
                }
                continue;
            }
            // A handler PANIC makes the daemon reply with an EMPTY id error
            // frame (it couldn't recover the request id) — that is terminal
            // for our request: accept it so the user sees "internal error"
            // immediately instead of a 30s "did not respond" timeout.
            let frame_id = frame.get("id").and_then(Value::as_str);
            if frame.get("error").is_some() && matches!(frame_id, None | Some("")) {
                break frame;
            }
            // Response frame for a different id (shouldn't happen on a fresh
            // connection, but never mis-attribute) → keep reading.
            if frame_id != Some(id.as_str()) {
                skipped += 1;
                if skipped > MAX_SKIPPED_FRAMES {
                    bail!("daemon answered with mismatched response ids");
                }
                continue;
            }
            break frame;
        }
    };

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
    use std::sync::Mutex;

    /// All tests below mutate the process-global AUTO2FA_SOCK env var; cargo
    /// runs tests on parallel threads, so without this they interleave and
    /// fail intermittently (one test's remove_var landing between another's
    /// set_var and its assert).
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn socket_path_default() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("AUTO2FA_SOCK");
        let p = socket_path();
        assert!(p.to_string_lossy().contains(".ssh2fa/ssh2fa.sock"));
    }

    #[test]
    fn socket_path_override() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("AUTO2FA_SOCK", "/tmp/test.sock");
        let p = socket_path();
        assert_eq!(p, PathBuf::from("/tmp/test.sock"));
        std::env::remove_var("AUTO2FA_SOCK");
    }

    #[test]
    fn rpc_missing_socket_returns_err() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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

    /// A handler-panic error frame carries an EMPTY id — it must be terminal
    /// (the user sees "internal error" immediately), not skipped into a 30s
    /// "did not respond" timeout.
    #[test]
    fn rpc_accepts_empty_id_error_frame_as_terminal() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("mock2.sock");
        let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();

        let server = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let mut req_line = String::new();
            {
                let mut r = BufReader::new(conn.try_clone().unwrap());
                r.read_line(&mut req_line).unwrap();
            }
            // Panic-path response: empty id + error.
            conn.write_all(
                b"{\"id\":\"\",\"error\":{\"code\":\"internal\",\"message\":\"handler panicked\"}}\n",
            )
            .unwrap();
        });

        std::env::set_var("AUTO2FA_SOCK", &sock);
        let started = std::time::Instant::now();
        let r = rpc("anything", serde_json::json!({}));
        std::env::remove_var("AUTO2FA_SOCK");
        server.join().unwrap();

        let msg = r.unwrap_err().to_string();
        assert!(msg.contains("handler panicked"), "got: {msg}");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "must fail fast, not wait out the 30s timeout"
        );
    }

    /// REGRESSION: an interleaved `{"event":…}` frame arriving ahead of the
    /// response (subscribed connection) must be SKIPPED, not parsed as the
    /// response. The mock daemon writes an event frame, a mismatched-id
    /// response, then the real response.
    #[test]
    fn rpc_skips_event_frames_and_mismatched_ids() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("mock.sock");
        let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();

        let server = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            // Read the request line to learn the client's id.
            let mut req_line = String::new();
            {
                let mut r = BufReader::new(conn.try_clone().unwrap());
                r.read_line(&mut req_line).unwrap();
            }
            let req: Value = serde_json::from_str(req_line.trim_end()).unwrap();
            let id = req["id"].as_str().unwrap().to_owned();
            // 1. An event frame (broadcast racing the ack).
            conn.write_all(b"{\"event\":\"tunnel_status_changed\",\"data\":{}}\n")
                .unwrap();
            // 2. A response with a WRONG id.
            conn.write_all(b"{\"id\":\"deadbeef\",\"result\":{\"wrong\":true}}\n")
                .unwrap();
            // 3. The real response.
            let resp = format!("{{\"id\":\"{id}\",\"result\":{{\"right\":true}}}}\n");
            conn.write_all(resp.as_bytes()).unwrap();
        });

        std::env::set_var("AUTO2FA_SOCK", &sock);
        let r = rpc("subscribe_events", serde_json::json!({}));
        std::env::remove_var("AUTO2FA_SOCK");
        server.join().unwrap();

        let v = r.expect("rpc must succeed with the real response");
        assert_eq!(v["right"], true, "must return the matching-id result, got: {v}");
    }
}
