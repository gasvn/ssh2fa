//! IPC handlers for host-related methods.
//!
//! Methods: ping, list_hosts, host_toggle, host_mount_toggle,
//!          host_rotate, host_add, host_test_credentials.
//!
//! Parity: `Auto2FADaemon.handle_request` in daemon.py.
//!
//! # Live-SSH methods
//! `host_toggle`, `host_mount_toggle`, `host_rotate`, `host_add`, and
//! `host_test_credentials` all require a live SSH master or Keychain write.
//! They compile and return a proper error shape; they are marked with a
//! TODO(integration) note for the wiring step.

use std::sync::{Arc, Mutex};

use a2fa_core::engine::State;
use a2fa_core::error::{Error, Result};
use a2fa_core::model::Host;
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Snapshot helpers (mirror `_host_snapshot` in daemon.py)
// ---------------------------------------------------------------------------

/// Build a JSON snapshot of a single `Host`, matching daemon.py's
/// `_host_snapshot` return dict exactly.
pub fn host_snapshot(h: &Host) -> Value {
    json!({
        "host": h.host,
        "status": h.status,
        "active": h.active,
        "is_master_ready": h.is_master_ready,
        "pool_index": h.pool_index,
        "pool_alive": h.pool_alive,
        "is_mounted": h.is_mounted,
        "last_msg": h.last_msg,
    })
}

// ---------------------------------------------------------------------------
// ping
// ---------------------------------------------------------------------------

pub fn ping(state: &Arc<Mutex<State>>) -> Result<Value> {
    let _guard = state.lock().unwrap(); // take lock just to prove it's accessible
    Ok(json!({ "ok": true, "pid": std::process::id() }))
}

// ---------------------------------------------------------------------------
// list_hosts
// ---------------------------------------------------------------------------

pub fn list_hosts(state: &Arc<Mutex<State>>) -> Result<Value> {
    let guard = state.lock().unwrap();
    let snaps: Vec<Value> = guard.hosts.iter().map(host_snapshot).collect();
    Ok(json!(snaps))
}

// ---------------------------------------------------------------------------
// host_toggle
// ---------------------------------------------------------------------------

/// Toggle a host's active/inactive state.
///
/// TODO(integration): call the ssh::master layer to actually start/stop the
/// SSH master connection. For now we flip `active` in state.
pub fn host_toggle(state: &Arc<Mutex<State>>, params: &Value) -> Result<Value> {
    let host_name = params["host"]
        .as_str()
        .ok_or_else(|| Error::BadParams("host required".into()))?;

    let mut guard = state.lock().unwrap();
    let host = guard
        .hosts
        .iter_mut()
        .find(|h| h.host == host_name)
        .ok_or_else(|| Error::NotFound(format!("host {host_name}")))?;

    host.active = !host.active;
    host.last_msg = if host.active {
        "Activated".into()
    } else {
        "Deactivated".into()
    };
    Ok(Value::Null)
}

// ---------------------------------------------------------------------------
// host_mount_toggle
// ---------------------------------------------------------------------------

/// Toggle SSHFS mount for a host.
///
/// TODO(integration): call sshfs mount/unmount.
pub fn host_mount_toggle(state: &Arc<Mutex<State>>, params: &Value) -> Result<Value> {
    let host_name = params["host"]
        .as_str()
        .ok_or_else(|| Error::BadParams("host required".into()))?;

    let mut guard = state.lock().unwrap();
    let host = guard
        .hosts
        .iter_mut()
        .find(|h| h.host == host_name)
        .ok_or_else(|| Error::NotFound(format!("host {host_name}")))?;

    host.is_mounted = !host.is_mounted;
    host.last_msg = if host.is_mounted {
        "Mount requested".into()
    } else {
        "Unmount requested".into()
    };
    Ok(Value::Null)
}

// ---------------------------------------------------------------------------
// host_rotate
// ---------------------------------------------------------------------------

/// Manually rotate the active connection-pool slot for a host.
///
/// TODO(integration): call ssh::master rotate / update_symlink.
pub fn host_rotate(state: &Arc<Mutex<State>>, params: &Value) -> Result<Value> {
    let host_name = params["host"]
        .as_str()
        .ok_or_else(|| Error::BadParams("host required".into()))?;

    let mut guard = state.lock().unwrap();
    let host = guard
        .hosts
        .iter_mut()
        .find(|h| h.host == host_name && h.active)
        .ok_or_else(|| Error::NotFound("host not active".into()))?;

    host.pool_index = (host.pool_index + 1) % 2;
    host.last_msg = format!("Manual Rotate -> {}", host.pool_index);
    Ok(Value::Null)
}

// ---------------------------------------------------------------------------
// host_add
// ---------------------------------------------------------------------------

/// Validate host-name regex (mirrors `_valid_host_name` in daemon.py).
fn valid_host_name(host: &str) -> bool {
    if host.contains("..") {
        return false;
    }
    let mut chars = host.chars();
    let first = match chars.next() {
        Some(c) => c,
        None => return false,
    };
    if !first.is_alphanumeric() && first != '_' {
        return false;
    }
    chars.all(|c| c.is_alphanumeric() || c == '.' || c == '-' || c == '_')
}

/// Add a host to the config and start its manager.
///
/// TODO(integration): write Keychain entry, write passwords.json, and start
/// the real SSH manager thread.
pub fn host_add(state: &Arc<Mutex<State>>, params: &Value) -> Result<Value> {
    let host_name = params["host"]
        .as_str()
        .ok_or_else(|| Error::BadParams("host required".into()))?;

    if !valid_host_name(host_name) {
        return Err(Error::BadParams(
            "invalid host name (letters, digits, '.', '-', '_' only; no '/' or '..')".into(),
        ));
    }

    let auto_connect = params.get("auto_connect")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let mut guard = state.lock().unwrap();
    if guard.hosts.iter().any(|h| h.host == host_name) {
        return Err(Error::Duplicate(format!("host {host_name} already exists")));
    }

    let new_host = a2fa_core::model::Host {
        host: host_name.to_owned(),
        status: "Idle".into(),
        active: auto_connect,
        is_master_ready: false,
        pool_index: 0,
        pool_alive: 0,
        is_mounted: false,
        last_msg: "Added".into(),
    };
    let snap = host_snapshot(&new_host);
    guard.hosts.push(new_host);
    Ok(snap)
}

// ---------------------------------------------------------------------------
// host_test_credentials
// ---------------------------------------------------------------------------

/// Dry-run credential test.
///
/// TODO(integration): spawn a one-shot `ssh` via pexpect-equivalent to verify
/// password + OTP without writing anything to disk.
pub fn host_test_credentials(_state: &Arc<Mutex<State>>, _params: &Value) -> Result<Value> {
    // Stub: the test cannot proceed without a live SSH target.
    Ok(json!({
        "ok": false,
        "reason": "host_test_credentials requires live SSH — not yet wired in Rust daemon"
    }))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use a2fa_core::engine::State;
    use std::sync::{Arc, Mutex};

    fn make_state_with_host(name: &str, active: bool) -> Arc<Mutex<State>> {
        let mut state = State::with_tunnels(vec![]);
        state.hosts.push(a2fa_core::model::Host {
            host: name.into(),
            status: "Idle".into(),
            active,
            is_master_ready: false,
            pool_index: 0,
            pool_alive: 0,
            is_mounted: false,
            last_msg: "OK".into(),
        });
        Arc::new(Mutex::new(state))
    }

    #[test]
    fn ping_returns_ok_pid() {
        let state = Arc::new(Mutex::new(State::with_tunnels(vec![])));
        let v = ping(&state).unwrap();
        assert_eq!(v["ok"], true);
        assert!(v["pid"].as_u64().unwrap() > 0);
    }

    #[test]
    fn list_hosts_empty() {
        let state = Arc::new(Mutex::new(State::with_tunnels(vec![])));
        let v = list_hosts(&state).unwrap();
        assert!(v.as_array().unwrap().is_empty());
    }

    #[test]
    fn list_hosts_one() {
        let state = make_state_with_host("k6", true);
        let v = list_hosts(&state).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["host"], "k6");
    }

    #[test]
    fn host_toggle_flips_active() {
        let state = make_state_with_host("k6", false);
        host_toggle(&state, &json!({"host": "k6"})).unwrap();
        assert!(state.lock().unwrap().hosts[0].active);
    }

    #[test]
    fn host_toggle_not_found() {
        let state = Arc::new(Mutex::new(State::with_tunnels(vec![])));
        let err = host_toggle(&state, &json!({"host": "ghost"})).unwrap_err();
        assert!(matches!(err, Error::NotFound(_)));
    }

    #[test]
    fn valid_host_name_accepts_safe_names() {
        assert!(valid_host_name("k6"));
        assert!(valid_host_name("holy_gpu01"));
        assert!(valid_host_name("node-1.cluster"));
    }

    #[test]
    fn valid_host_name_rejects_unsafe() {
        assert!(!valid_host_name(""));
        assert!(!valid_host_name("a/b"));
        assert!(!valid_host_name("a..b"));
        assert!(!valid_host_name("-bad"));
        assert!(!valid_host_name(".bad"));
    }

    #[test]
    fn host_add_duplicate_returns_error() {
        let state = make_state_with_host("k6", false);
        let err = host_add(
            &state,
            &json!({"host": "k6", "password": "x", "otpauth_url": "otpauth://totp/x?secret=ABC"}),
        )
        .unwrap_err();
        assert!(matches!(err, Error::Duplicate(_)));
    }
}
