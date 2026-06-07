//! IPC handlers for tunnel-related methods.
//!
//! Methods: list_tunnels, tunnel_add, tunnel_remove, tunnel_start, tunnel_stop,
//!          tunnel_toggle, tunnel_set_node, tunnel_set_autostart,
//!          tunnel_set_jump_candidates, tunnel_set_post_connect, tunnel_set_tags,
//!          tunnel_set_url_path, tunnel_rename, tunnels_batch,
//!          tunnel_events, discover_nodes, port_suggest.
//!
//! Parity: `Auto2FADaemon.handle_request` in daemon.py.
//!
//! # Live-SSH methods
//! `tunnel_start`, `tunnel_stop`, `tunnel_toggle`, `tunnel_set_node`,
//! `discover_nodes` require a live SSH master / forward process.  They
//! compile and return proper JSON shapes; their TODO(integration) stubs
//! mark where the real calls will go.

use std::sync::{Arc, Mutex};

use a2fa_core::config::save_tunnels;
use a2fa_core::engine::State;
use a2fa_core::error::{Error, Result};
use a2fa_core::model::{Tunnel, TunnelStatus};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Snapshot helper (mirrors `_tunnel_snapshot` in daemon.py)
// ---------------------------------------------------------------------------

pub fn tunnel_snapshot(t: &Tunnel) -> Value {
    json!({
        "name": t.name,
        "local_port": t.local_port,
        "remote_port": t.remote_port,
        "jump_candidates": t.jump_candidates,
        "last_node": t.last_node,
        "last_user": t.last_user,
        "auto_start": t.auto_start,
        "post_connect_cmd": t.post_connect_cmd,
        "tags": t.tags,
        "url_path": t.url_path,
        "active_jump": t.active_jump,
        "status": t.status,
        "last_msg": t.last_msg,
        "last_alive_at": t.last_alive_at,
        "total_uptime_sec": t.total_uptime_sec,
        "connect_count": t.connect_count,
        "fail_count": t.fail_count,
    })
}

// ---------------------------------------------------------------------------
// list_tunnels
// ---------------------------------------------------------------------------

pub fn list_tunnels(state: &Arc<Mutex<State>>) -> Result<Value> {
    let guard = state.lock().unwrap();
    let snaps: Vec<Value> = guard.tunnels.iter().map(tunnel_snapshot).collect();
    Ok(json!(snaps))
}

// ---------------------------------------------------------------------------
// tunnel_add
// ---------------------------------------------------------------------------

/// Port range 1024..=65535, mirrors TunnelManager.add validation in tunnels.py.
fn is_valid_port(p: u16) -> bool {
    p >= 1024
}

/// Check whether a local port is currently bound on 127.0.0.1.
fn port_in_use(port: u16) -> bool {
    use std::net::TcpListener;
    TcpListener::bind(("127.0.0.1", port)).is_err()
}

pub fn tunnel_add(state: &Arc<Mutex<State>>, params: &Value) -> Result<Value> {
    let name = params["name"]
        .as_str()
        .ok_or_else(|| Error::BadParams("name required".into()))?
        .to_owned();

    let local_port = params["local_port"]
        .as_u64()
        .ok_or_else(|| Error::BadParams("local_port required".into()))? as u16;

    if !is_valid_port(local_port) {
        return Err(Error::BadParams(format!(
            "local_port {local_port} out of range (1024..65535)"
        )));
    }

    let remote_port = params
        .get("remote_port")
        .and_then(|v| v.as_u64())
        .map(|p| p as u16)
        .unwrap_or(local_port);

    let mut guard = state.lock().unwrap();

    // Duplicate check (by name)
    if guard.tunnels.iter().any(|t| t.name == name) {
        return Err(Error::Duplicate(format!("tunnel '{name}' already exists")));
    }

    // Port in use check (by local_port among existing tunnels)
    if guard.tunnels.iter().any(|t| t.local_port == local_port) {
        return Err(Error::PortInUse(local_port));
    }

    // Actual bind check
    if port_in_use(local_port) {
        return Err(Error::PortInUse(local_port));
    }

    let tunnel = Tunnel {
        name: name.clone(),
        local_port,
        remote_port,
        jump_candidates: None,
        last_node: None,
        last_user: None,
        auto_start: false,
        post_connect_cmd: None,
        tags: vec![],
        url_path: None,
        wants_alive: false,
        status: TunnelStatus::Idle,
        active_jump: None,
        last_msg: "Added".into(),
        last_alive_at: 0.0,
        total_uptime_sec: 0.0,
        connect_count: 0,
        fail_count: 0,
    };

    let snap = tunnel_snapshot(&tunnel);
    guard.tunnels.push(tunnel);

    // Persist — best effort; don't fail the add if the write fails.
    let path = guard.tunnels_path.clone();
    let tunnels = guard.tunnels.clone();
    drop(guard);
    let _ = save_tunnels(&path, &tunnels);

    Ok(snap)
}

// ---------------------------------------------------------------------------
// tunnel_remove
// ---------------------------------------------------------------------------

pub fn tunnel_remove(state: &Arc<Mutex<State>>, params: &Value) -> Result<Value> {
    let name = params["name"]
        .as_str()
        .ok_or_else(|| Error::BadParams("name required".into()))?;

    let mut guard = state.lock().unwrap();
    let pos = guard
        .tunnels
        .iter()
        .position(|t| t.name == name)
        .ok_or_else(|| Error::NotFound(name.to_owned()))?;

    // TODO(integration): stop_forward if status == Alive before removing.
    guard.tunnels.remove(pos);

    let path = guard.tunnels_path.clone();
    let tunnels = guard.tunnels.clone();
    drop(guard);
    let _ = save_tunnels(&path, &tunnels);

    Ok(Value::Null)
}

// ---------------------------------------------------------------------------
// tunnel_start / tunnel_stop / tunnel_toggle
// ---------------------------------------------------------------------------

/// Start a tunnel — idempotent.
///
/// TODO(integration): call crate::tunnels::forward::start_forward(...).
pub fn tunnel_start(state: &Arc<Mutex<State>>, params: &Value) -> Result<Value> {
    let name = params["name"]
        .as_str()
        .ok_or_else(|| Error::BadParams("name required".into()))?;

    let mut guard = state.lock().unwrap();
    let t = guard
        .tunnels
        .iter_mut()
        .find(|t| t.name == name)
        .ok_or_else(|| Error::NotFound(name.to_owned()))?;

    if t.status == TunnelStatus::Alive {
        return Ok(Value::Null); // idempotent
    }

    t.wants_alive = true;
    t.last_msg = "Start requested (stub)".into();
    // TODO(integration): spawn start_forward off-lock.
    Ok(Value::Null)
}

/// Stop a tunnel — idempotent.
///
/// TODO(integration): call crate::tunnels::forward::stop_forward(...).
pub fn tunnel_stop(state: &Arc<Mutex<State>>, params: &Value) -> Result<Value> {
    let name = params["name"]
        .as_str()
        .ok_or_else(|| Error::BadParams("name required".into()))?;

    let mut guard = state.lock().unwrap();
    let t = guard
        .tunnels
        .iter_mut()
        .find(|t| t.name == name)
        .ok_or_else(|| Error::NotFound(name.to_owned()))?;

    if t.status != TunnelStatus::Alive {
        return Ok(Value::Null); // idempotent
    }

    t.wants_alive = false;
    t.status = TunnelStatus::Idle;
    t.last_msg = "Stopped".into();
    t.active_jump = None;
    // TODO(integration): kill the ssh -L process off-lock.
    Ok(Value::Null)
}

/// Toggle a tunnel between started and stopped.
pub fn tunnel_toggle(state: &Arc<Mutex<State>>, params: &Value) -> Result<Value> {
    let name = params["name"]
        .as_str()
        .ok_or_else(|| Error::BadParams("name required".into()))?;

    let is_alive = {
        let guard = state.lock().unwrap();
        guard
            .tunnels
            .iter()
            .find(|t| t.name == name)
            .ok_or_else(|| Error::NotFound(name.to_owned()))?
            .status
            == TunnelStatus::Alive
    };

    if is_alive {
        tunnel_stop(state, params)
    } else {
        tunnel_start(state, params)
    }
}

// ---------------------------------------------------------------------------
// tunnel_set_node
// ---------------------------------------------------------------------------

/// Set the target node for a tunnel.
///
/// TODO(integration): expand SLURM ranges via core helper, then restart if alive.
pub fn tunnel_set_node(state: &Arc<Mutex<State>>, params: &Value) -> Result<Value> {
    let name = params["name"]
        .as_str()
        .ok_or_else(|| Error::BadParams("name required".into()))?;
    let node = params["node"]
        .as_str()
        .ok_or_else(|| Error::BadParams("node required".into()))?;
    let user = params
        .get("user")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned();

    let mut guard = state.lock().unwrap();
    let t = guard
        .tunnels
        .iter_mut()
        .find(|t| t.name == name)
        .ok_or_else(|| Error::NotFound(name.to_owned()))?;

    t.last_node = Some(node.to_owned());
    if !user.is_empty() {
        t.last_user = Some(user);
    }
    t.last_msg = format!("Node set to {node}");
    Ok(Value::Null)
}

// ---------------------------------------------------------------------------
// tunnel_set_autostart
// ---------------------------------------------------------------------------

pub fn tunnel_set_autostart(state: &Arc<Mutex<State>>, params: &Value) -> Result<Value> {
    let name = params["name"]
        .as_str()
        .ok_or_else(|| Error::BadParams("name required".into()))?;
    let value = params
        .get("value")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let snap = {
        let mut guard = state.lock().unwrap();
        let t = guard
            .tunnels
            .iter_mut()
            .find(|t| t.name == name)
            .ok_or_else(|| Error::NotFound(name.to_owned()))?;
        t.auto_start = value;
        tunnel_snapshot(t)
    };

    // Persist
    let guard = state.lock().unwrap();
    let _ = save_tunnels(&guard.tunnels_path, &guard.tunnels);
    Ok(snap)
}

// ---------------------------------------------------------------------------
// tunnel_set_jump_candidates
// ---------------------------------------------------------------------------

pub fn tunnel_set_jump_candidates(state: &Arc<Mutex<State>>, params: &Value) -> Result<Value> {
    let name = params["name"]
        .as_str()
        .ok_or_else(|| Error::BadParams("name required".into()))?;

    let cands: Option<Vec<String>> = match params.get("candidates") {
        None | Some(Value::Null) => None,
        Some(Value::Array(arr)) => {
            Some(arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        }
        Some(_) => {
            return Err(Error::BadParams("candidates must be list or null".into()))
        }
    };

    let snap = {
        let mut guard = state.lock().unwrap();
        // Filter to known hosts
        let known_hosts: Vec<String> = guard.hosts.iter().map(|h| h.host.clone()).collect();
        let filtered = cands.map(|cs| {
            cs.into_iter().filter(|c| known_hosts.contains(c)).collect::<Vec<_>>()
        });

        let t = guard
            .tunnels
            .iter_mut()
            .find(|t| t.name == name)
            .ok_or_else(|| Error::NotFound(name.to_owned()))?;
        t.jump_candidates = filtered;
        // TODO(integration): if was_alive, restart tunnel with new candidates.
        tunnel_snapshot(t)
    };

    let guard = state.lock().unwrap();
    let _ = save_tunnels(&guard.tunnels_path, &guard.tunnels);
    Ok(snap)
}

// ---------------------------------------------------------------------------
// tunnel_set_post_connect
// ---------------------------------------------------------------------------

pub fn tunnel_set_post_connect(state: &Arc<Mutex<State>>, params: &Value) -> Result<Value> {
    let name = params["name"]
        .as_str()
        .ok_or_else(|| Error::BadParams("name required".into()))?;

    let cmd: Option<String> = match params.get("cmd") {
        None | Some(Value::Null) => None,
        Some(v) => {
            let s = v.as_str().unwrap_or("").trim().to_owned();
            if s.is_empty() { None } else { Some(s) }
        }
    };

    let snap = {
        let mut guard = state.lock().unwrap();
        let t = guard
            .tunnels
            .iter_mut()
            .find(|t| t.name == name)
            .ok_or_else(|| Error::NotFound(name.to_owned()))?;
        t.post_connect_cmd = cmd;
        tunnel_snapshot(t)
    };

    let guard = state.lock().unwrap();
    let _ = save_tunnels(&guard.tunnels_path, &guard.tunnels);
    Ok(snap)
}

// ---------------------------------------------------------------------------
// tunnel_set_tags
// ---------------------------------------------------------------------------

pub fn tunnel_set_tags(state: &Arc<Mutex<State>>, params: &Value) -> Result<Value> {
    let name = params["name"]
        .as_str()
        .ok_or_else(|| Error::BadParams("name required".into()))?;

    let tags: Vec<String> = match params.get("tags") {
        None | Some(Value::Null) => vec![],
        Some(Value::Array(arr)) => {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::trim))
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect()
        }
        Some(_) => return Err(Error::BadParams("tags must be a list of strings".into())),
    };

    let snap = {
        let mut guard = state.lock().unwrap();
        let t = guard
            .tunnels
            .iter_mut()
            .find(|t| t.name == name)
            .ok_or_else(|| Error::NotFound(name.to_owned()))?;
        t.tags = tags;
        tunnel_snapshot(t)
    };

    let guard = state.lock().unwrap();
    let _ = save_tunnels(&guard.tunnels_path, &guard.tunnels);
    Ok(snap)
}

// ---------------------------------------------------------------------------
// tunnel_set_url_path
// ---------------------------------------------------------------------------

pub fn tunnel_set_url_path(state: &Arc<Mutex<State>>, params: &Value) -> Result<Value> {
    let name = params["name"]
        .as_str()
        .ok_or_else(|| Error::BadParams("name required".into()))?;

    let path: Option<String> = match params.get("path") {
        None | Some(Value::Null) => None,
        Some(v) => {
            let s = v.as_str().unwrap_or("").trim().to_owned();
            if s.is_empty() { None } else { Some(s) }
        }
    };

    let snap = {
        let mut guard = state.lock().unwrap();
        let t = guard
            .tunnels
            .iter_mut()
            .find(|t| t.name == name)
            .ok_or_else(|| Error::NotFound(name.to_owned()))?;
        t.url_path = path;
        tunnel_snapshot(t)
    };

    let guard = state.lock().unwrap();
    let _ = save_tunnels(&guard.tunnels_path, &guard.tunnels);
    Ok(snap)
}

// ---------------------------------------------------------------------------
// tunnel_rename
// ---------------------------------------------------------------------------

pub fn tunnel_rename(state: &Arc<Mutex<State>>, params: &Value) -> Result<Value> {
    let old = params["old"]
        .as_str()
        .ok_or_else(|| Error::BadParams("old name required".into()))?;
    let new = params["new"]
        .as_str()
        .ok_or_else(|| Error::BadParams("new name required".into()))?
        .trim()
        .to_owned();

    if new.is_empty() {
        return Err(Error::BadParams("new name required".into()));
    }

    let snap = {
        let mut guard = state.lock().unwrap();

        if old == new {
            let t = guard
                .tunnels
                .iter()
                .find(|t| t.name == old)
                .ok_or_else(|| Error::NotFound(old.to_owned()))?;
            return Ok(tunnel_snapshot(t));
        }

        if guard.tunnels.iter().any(|t| t.name == new) {
            return Err(Error::Duplicate(format!("tunnel '{new}' already exists")));
        }

        let t = guard
            .tunnels
            .iter_mut()
            .find(|t| t.name == old)
            .ok_or_else(|| Error::NotFound(old.to_owned()))?;

        // TODO(integration): stop if alive, rename, restart.
        t.name = new;
        tunnel_snapshot(t)
    };

    let guard = state.lock().unwrap();
    let _ = save_tunnels(&guard.tunnels_path, &guard.tunnels);
    Ok(snap)
}

// ---------------------------------------------------------------------------
// tunnels_batch
// ---------------------------------------------------------------------------

pub fn tunnels_batch(state: &Arc<Mutex<State>>, params: &Value) -> Result<Value> {
    let action = params
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if action != "start" && action != "stop" {
        return Err(Error::BadParams("action must be 'start' or 'stop'".into()));
    }

    let names: Vec<String> = match params.get("names") {
        None | Some(Value::Null) => vec![],
        Some(Value::Array(arr)) => {
            arr.iter().filter_map(|v| v.as_str().map(String::from)).collect()
        }
        Some(_) => return Err(Error::BadParams("names must be an array".into())),
    };

    let mut results: Vec<Value> = Vec::new();
    for name in &names {
        let pv = json!({"name": name});
        let outcome = if action == "start" {
            tunnel_start(state, &pv)
        } else {
            tunnel_stop(state, &pv)
        };
        match outcome {
            Ok(_) => results.push(json!({"name": name, "ok": true})),
            Err(e) => results.push(json!({"name": name, "ok": false, "error": e.to_string()})),
        }
    }

    Ok(json!({ "results": results }))
}

// ---------------------------------------------------------------------------
// tunnel_events
// ---------------------------------------------------------------------------

pub fn tunnel_events(state: &Arc<Mutex<State>>, params: &Value) -> Result<Value> {
    let name = params["name"]
        .as_str()
        .ok_or_else(|| Error::BadParams("name required".into()))?;

    let guard = state.lock().unwrap();
    guard
        .tunnels
        .iter()
        .find(|t| t.name == name)
        .ok_or_else(|| Error::NotFound(name.to_owned()))?;

    // The Rust Tunnel model doesn't yet carry a ring-buffer of events; return
    // an empty list (matches the shape from daemon.py).
    Ok(json!({ "events": [] }))
}

// ---------------------------------------------------------------------------
// discover_nodes
// ---------------------------------------------------------------------------

/// Discover SLURM nodes via an existing SSH master.
///
/// TODO(integration): call a2fa_core::tunnels::discover_nodes_via_master
/// with the active ControlPath from a2fa_core::ssh::control::active_symlink_path.
pub fn discover_nodes(state: &Arc<Mutex<State>>, params: &Value) -> Result<Value> {
    let host_name = params["host"]
        .as_str()
        .ok_or_else(|| Error::BadParams("host required".into()))?;

    let guard = state.lock().unwrap();
    let host = guard
        .hosts
        .iter()
        .find(|h| h.host == host_name)
        .ok_or_else(|| Error::NotFound(host_name.to_owned()))?;

    if !host.is_master_ready {
        return Err(Error::Discovery(format!("{host_name} master not ready")));
    }

    // TODO(integration): run NodeDiscovery::discover via the master ControlPath.
    Ok(json!([]))
}

// ---------------------------------------------------------------------------
// port_suggest
// ---------------------------------------------------------------------------

pub fn port_suggest(state: &Arc<Mutex<State>>, params: &Value) -> Result<Value> {
    let base = params
        .get("base")
        .and_then(|v| v.as_u64())
        .unwrap_or(8888) as u16;

    let taken: Vec<u16> = {
        let guard = state.lock().unwrap();
        guard.tunnels.iter().map(|t| t.local_port).collect()
    };

    let free = find_free_port(base, &taken);
    Ok(json!({ "port": free }))
}

/// Find the lowest free port >= base that isn't in `taken` and isn't bound.
fn find_free_port(base: u16, taken: &[u16]) -> u16 {
    use std::net::TcpListener;

    let start = base.max(1024);
    for port in start..=65534 {
        if taken.contains(&port) {
            continue;
        }
        if TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return port;
        }
    }
    base
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use a2fa_core::engine::State;
    use std::sync::{Arc, Mutex};

    fn make_state() -> Arc<Mutex<State>> {
        Arc::new(Mutex::new(State::with_tunnels(vec![])))
    }

    fn make_state_with_tunnel(name: &str, port: u16) -> Arc<Mutex<State>> {
        let t = Tunnel {
            name: name.into(),
            local_port: port,
            remote_port: port,
            jump_candidates: None,
            last_node: None,
            last_user: None,
            auto_start: false,
            post_connect_cmd: None,
            tags: vec![],
            url_path: None,
            wants_alive: false,
            status: TunnelStatus::Idle,
            active_jump: None,
            last_msg: "Ready".into(),
            last_alive_at: 0.0,
            total_uptime_sec: 0.0,
            connect_count: 0,
            fail_count: 0,
        };
        Arc::new(Mutex::new(State::with_tunnels(vec![t])))
    }

    #[test]
    fn list_tunnels_empty() {
        let state = make_state();
        let v = list_tunnels(&state).unwrap();
        assert!(v.as_array().unwrap().is_empty());
    }

    #[test]
    fn list_tunnels_one() {
        let state = make_state_with_tunnel("nb", 9000);
        let v = list_tunnels(&state).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["name"], "nb");
        assert_eq!(arr[0]["local_port"], 9000);
    }

    #[test]
    fn tunnel_rename_ok() {
        let state = make_state_with_tunnel("nb", 9000);
        let v = tunnel_rename(&state, &json!({"old": "nb", "new": "nb2"})).unwrap();
        assert_eq!(v["name"], "nb2");
        assert_eq!(state.lock().unwrap().tunnels[0].name, "nb2");
    }

    #[test]
    fn tunnel_rename_duplicate_returns_error() {
        let mut inner = State::with_tunnels(vec![]);
        for name in ["nb", "nb2"] {
            inner.tunnels.push(Tunnel {
                name: name.into(),
                local_port: 9000 + inner.tunnels.len() as u16,
                remote_port: 9000,
                jump_candidates: None, last_node: None, last_user: None,
                auto_start: false, post_connect_cmd: None, tags: vec![],
                url_path: None, wants_alive: false, status: TunnelStatus::Idle,
                active_jump: None, last_msg: "Ready".into(), last_alive_at: 0.0,
                total_uptime_sec: 0.0, connect_count: 0, fail_count: 0,
            });
        }
        let state = Arc::new(Mutex::new(inner));
        let err = tunnel_rename(&state, &json!({"old": "nb", "new": "nb2"})).unwrap_err();
        assert!(matches!(err, Error::Duplicate(_)));
    }

    #[test]
    fn port_suggest_returns_free_port() {
        let state = make_state();
        let v = port_suggest(&state, &json!({})).unwrap();
        let port = v["port"].as_u64().unwrap();
        assert!(port >= 1024);
    }

    #[test]
    fn tunnel_set_tags_and_retrieve() {
        let state = make_state_with_tunnel("nb", 9001);
        let v = tunnel_set_tags(
            &state,
            &json!({"name": "nb", "tags": ["ml", "gpu"]}),
        )
        .unwrap();
        assert_eq!(v["tags"], json!(["ml", "gpu"]));
    }

    #[test]
    fn tunnels_batch_bad_action() {
        let state = make_state();
        let err = tunnels_batch(&state, &json!({"action": "fly", "names": []})).unwrap_err();
        assert!(matches!(err, Error::BadParams(_)));
    }
}
