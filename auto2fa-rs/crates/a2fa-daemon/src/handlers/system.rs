//! IPC handlers for system-level methods.
//!
//! Methods: log_tail, wake_recover, reset_all, subscribe_events.
//!
//! Parity: `Auto2FADaemon.handle_request` in daemon.py.

use std::io::{Read, Seek, SeekFrom};
use std::sync::{Arc, Mutex};

use a2fa_core::engine::{recovery, State};
use a2fa_core::error::{Error, Result};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// log_tail
// ---------------------------------------------------------------------------

/// Return the last `n` lines of `/tmp/auto2fa_daemon.log`.
///
/// Uses a backwards block-read to stay cheap on large files
/// (mirrors `_tail_file` in daemon.py).
pub fn log_tail(_state: &Arc<Mutex<State>>, params: &Value) -> Result<Value> {
    let n = params
        .get("lines")
        .and_then(|v| v.as_u64())
        .unwrap_or(200) as usize;

    let path = "/tmp/auto2fa_daemon.log";
    let lines = tail_file(path, n)?;
    Ok(json!({ "lines": lines }))
}

fn tail_file(path: &str, n: usize) -> Result<Vec<String>> {
    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(Error::Io(e)),
    };

    let size = f.seek(SeekFrom::End(0)).map_err(Error::Io)?;
    if size == 0 {
        return Ok(vec![]);
    }

    let block_size: u64 = 4096;
    let mut lines: Vec<Vec<u8>> = Vec::new();
    let mut carry: Vec<u8> = Vec::new();
    let mut offset: u64 = size;

    while offset > 0 && lines.len() <= n {
        let read_size = block_size.min(offset);
        offset -= read_size;
        f.seek(SeekFrom::Start(offset)).map_err(Error::Io)?;
        let mut buf = vec![0u8; read_size as usize];
        f.read_exact(&mut buf).map_err(Error::Io)?;
        buf.extend_from_slice(&carry);
        let mut parts: Vec<Vec<u8>> = buf.split(|&b| b == b'\n').map(|s| s.to_vec()).collect();
        carry = parts.remove(0);
        parts.extend(lines);
        lines = parts;
    }
    if offset == 0 && !carry.is_empty() {
        let mut new = vec![carry];
        new.extend(lines);
        lines = new;
    }

    let decoded: Vec<String> = lines
        .into_iter()
        .filter(|l| !l.is_empty())
        .map(|l| String::from_utf8_lossy(&l).into_owned())
        .collect();

    let total = decoded.len();
    Ok(if total > n {
        decoded[total - n..].to_vec()
    } else {
        decoded
    })
}

// ---------------------------------------------------------------------------
// reset_all
// ---------------------------------------------------------------------------

pub fn reset_all(state: &Arc<Mutex<State>>, _params: &Value) -> Result<Value> {
    let result = recovery::reset_all(state);
    Ok(json!({
        "tunnels_stopped": result.tunnels_stopped,
        "masters_rebuilt": result.masters_rebuilt,
    }))
}

// ---------------------------------------------------------------------------
// wake_recover
// ---------------------------------------------------------------------------

pub fn wake_recover(state: &Arc<Mutex<State>>, _params: &Value) -> Result<Value> {
    let result = recovery::wake_recover(state);
    Ok(json!({ "tunnels_restarting": result.tunnels_restarting }))
}

// ---------------------------------------------------------------------------
// subscribe_events (called inline in the connection loop)
// ---------------------------------------------------------------------------

/// Returns the `subscribed: true` ack.  The actual subscriber wiring is done
/// in `server.rs` before this is called.
pub fn subscribe_events_ack() -> Value {
    json!({ "subscribed": true })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use a2fa_core::engine::State;
    use std::sync::{Arc, Mutex};

    #[test]
    fn reset_all_empty_state() {
        let state = Arc::new(Mutex::new(State::with_tunnels(vec![])));
        let v = reset_all(&state, &json!({})).unwrap();
        assert_eq!(v["tunnels_stopped"], 0);
    }

    #[test]
    fn wake_recover_empty_state() {
        let state = Arc::new(Mutex::new(State::with_tunnels(vec![])));
        let v = wake_recover(&state, &json!({})).unwrap();
        assert_eq!(v["tunnels_restarting"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn log_tail_missing_file_returns_empty() {
        let _state = Arc::new(Mutex::new(State::with_tunnels(vec![])));
        // Use a definitely nonexistent path (log_tail uses the real daemon log;
        // test it directly via tail_file instead).
        let v = tail_file("/nonexistent/path/xyz.log", 10).unwrap();
        assert!(v.is_empty());
    }
}
