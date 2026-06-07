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
//! `tunnel_start`, `tunnel_stop`, `tunnel_toggle`, `tunnel_set_node`, and
//! `discover_nodes` interact with the ssh core.  Start/stop operations are
//! dispatched to `crate::workers::spawn_tunnel_start`; stop happens inline
//! (kill + wait is fast).  `discover_nodes` calls
//! `a2fa_core::tunnels::discover_nodes_via_control` which reuses the existing
//! ControlMaster socket so no new 2FA is triggered.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use a2fa_core::config::save_tunnels;
use a2fa_core::engine::State;
use a2fa_core::error::{Error, Result};
use a2fa_core::model::{Tunnel, TunnelStatus};
use a2fa_core::ssh::control::active_symlink_path;
use a2fa_core::tunnels::{discover_nodes_via_control, expand_first_node};
use serde_json::{json, Value};

use crate::tunnel_runtime::TunnelRuntime;
use crate::workers::{spawn_tunnel_start, spawn_tunnel_start_with_runtime};

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

    // Duplicate check (by name).
    if guard.tunnels.iter().any(|t| t.name == name) {
        return Err(Error::Duplicate(format!("tunnel '{name}' already exists")));
    }

    // Port in use check (by local_port among existing tunnels).
    if guard.tunnels.iter().any(|t| t.local_port == local_port) {
        return Err(Error::PortInUse(local_port));
    }

    // Actual bind check.
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

pub fn tunnel_remove(
    state: &Arc<Mutex<State>>,
    params: &Value,
    runtime: Option<Arc<TunnelRuntime>>,
) -> Result<Value> {
    let name = params["name"]
        .as_str()
        .ok_or_else(|| Error::BadParams("name required".into()))?;

    // Kill the ssh -L child process (SIGKILL) before removing the entry.
    // Do this BEFORE acquiring the State lock so we never hold the lock across
    // the kill syscall (which is fast but is still a syscall).
    if let Some(rt) = &runtime {
        rt.kill_child(name);
        rt.with_rt_mut(name, |r| {
            r.last_recovery_attempt_ts = 0.0;
        });
    }

    let mut guard = state.lock().unwrap();
    let pos = guard
        .tunnels
        .iter()
        .position(|t| t.name == name)
        .ok_or_else(|| Error::NotFound(name.to_owned()))?;

    // Clear wants_alive so the maintenance loop doesn't attempt to restart
    // the tunnel between the kill above and the remove below.
    guard.tunnels[pos].status = TunnelStatus::Idle;
    guard.tunnels[pos].wants_alive = false;
    guard.tunnels.remove(pos);

    let path = guard.tunnels_path.clone();
    let tunnels = guard.tunnels.clone();
    drop(guard);
    let _ = save_tunnels(&path, &tunnels);

    // Clean up runtime state (counters + child entry) for this tunnel.
    if let Some(rt) = &runtime {
        rt.remove(name);
    }

    Ok(Value::Null)
}

// ---------------------------------------------------------------------------
// tunnel_start
// ---------------------------------------------------------------------------

/// Start a tunnel — idempotent.
///
/// Extracts jump/node/port info from State (under the lock), then dispatches
/// to `spawn_tunnel_start` which runs the blocking ssh off-lock.
/// Mirrors `TunnelManager.start` in tunnels.py.
pub fn tunnel_start(
    state: &Arc<Mutex<State>>,
    params: &Value,
    runtime: Option<Arc<TunnelRuntime>>,
) -> Result<Value> {
    let name = params["name"]
        .as_str()
        .ok_or_else(|| Error::BadParams("name required".into()))?
        .to_owned();

    // Snapshot everything we need under the lock.
    let (jump, user, node, local_port, remote_port, post_connect_cmd) = {
        let mut guard = state.lock().unwrap();
        let t = guard
            .tunnels
            .iter_mut()
            .find(|t| t.name == name)
            .ok_or_else(|| Error::NotFound(name.clone()))?;

        if t.status == TunnelStatus::Alive {
            return Ok(Value::Null); // idempotent
        }

        // Pick the first ready jump host.
        let jump = guard
            .hosts
            .iter()
            .find(|h| h.is_master_ready && {
                // If the tunnel has explicit candidates, check that.
                let t = guard.tunnels.iter().find(|t| t.name == name).unwrap();
                match &t.jump_candidates {
                    Some(cands) => cands.contains(&h.host),
                    None => true,
                }
            })
            .map(|h| h.host.clone());

        // Re-borrow tunnel mutably after the host lookup.
        let t = guard.tunnels.iter_mut().find(|t| t.name == name).unwrap();

        let node = match t.last_node.clone() {
            Some(n) => n,
            None => {
                t.status = TunnelStatus::Idle;
                t.last_msg = "no node — press Enter to pick".into();
                return Ok(Value::Null);
            }
        };

        let jump = match jump {
            Some(j) => j,
            None => {
                t.status = TunnelStatus::Idle;
                t.last_msg = "waiting for jump host".into();
                return Ok(Value::Null);
            }
        };

        let user = t
            .last_user
            .clone()
            .unwrap_or_else(|| std::env::var("USER").unwrap_or_default());

        if user.is_empty() {
            t.status = TunnelStatus::Failed;
            t.last_msg = "no user (set last_user in tunnels.json)".into();
            return Ok(Value::Null);
        }

        let local_port = t.local_port;
        let remote_port = t.remote_port;
        let post_cmd = t.post_connect_cmd.clone();

        t.status = TunnelStatus::Starting;
        t.active_jump = Some(jump.clone());
        t.last_msg = format!("starting via {jump}");
        t.wants_alive = true;

        (jump, user, node, local_port, remote_port, post_cmd)
    };

    // Spawn the blocking worker off-lock.
    let post_connect_running: Arc<Mutex<HashSet<String>>> =
        Arc::new(Mutex::new(HashSet::new()));

    match runtime {
        Some(rt) => spawn_tunnel_start_with_runtime(
            name,
            jump,
            user,
            node,
            local_port,
            remote_port,
            post_connect_cmd,
            Arc::clone(state),
            post_connect_running,
            rt,
        ),
        None => spawn_tunnel_start(
            name,
            jump,
            user,
            node,
            local_port,
            remote_port,
            post_connect_cmd,
            Arc::clone(state),
            post_connect_running,
        ),
    }

    Ok(Value::Null)
}

// ---------------------------------------------------------------------------
// tunnel_stop
// ---------------------------------------------------------------------------

/// Stop a tunnel — idempotent.
///
/// Mirrors `TunnelManager.stop` (user_initiated=True) in tunnels.py.
/// Clears `wants_alive`, marks the tunnel Idle, persists the change, and
/// SIGKILLs the `ssh -L` child process via the runtime registry.
pub fn tunnel_stop(
    state: &Arc<Mutex<State>>,
    params: &Value,
    runtime: Option<Arc<TunnelRuntime>>,
) -> Result<Value> {
    let name = params["name"]
        .as_str()
        .ok_or_else(|| Error::BadParams("name required".into()))?;

    // Clear wants_alive and mark Idle under the State lock FIRST, so the
    // maintenance loop sees the user's intent immediately.
    {
        let mut guard = state.lock().unwrap();
        let t = guard
            .tunnels
            .iter_mut()
            .find(|t| t.name == name)
            .ok_or_else(|| Error::NotFound(name.to_owned()))?;

        if t.status == TunnelStatus::Idle {
            return Ok(Value::Null); // idempotent
        }

        t.wants_alive = false;
        t.status = TunnelStatus::Idle;
        t.last_msg = "Stopped".into();
        t.active_jump = None;
    }

    // Kill the child process AFTER releasing the State lock.
    // SIGKILL + wait is fast, but we still don't want to hold the lock for it.
    if let Some(rt) = &runtime {
        rt.kill_child(name);
        // Accumulate uptime: fold alive_since into total_uptime_sec.
        let alive_since = rt.with_rt_mut(name, |r| {
            let s = r.alive_since;
            r.alive_since = None;
            s
        });
        if let Some(since) = alive_since {
            let delta = (a2fa_core::tunnels::uptime::now_unix() - since).max(0.0);
            let mut guard = state.lock().unwrap();
            if let Some(t) = guard.tunnels.iter_mut().find(|t| t.name == name) {
                t.total_uptime_sec += delta;
            }
        }
    }

    // Persist the change.
    let guard = state.lock().unwrap();
    let path = guard.tunnels_path.clone();
    let tunnels = guard.tunnels.clone();
    drop(guard);
    let _ = save_tunnels(&path, &tunnels);

    Ok(Value::Null)
}

// ---------------------------------------------------------------------------
// tunnel_toggle
// ---------------------------------------------------------------------------

/// Toggle a tunnel between started and stopped.
///
/// Mirrors the Python original: stop when status ∈ {Alive, Starting};
/// start otherwise. Stopping a "Starting" tunnel is useful when the user
/// wants to abort a connection attempt that is still in progress.
pub fn tunnel_toggle(
    state: &Arc<Mutex<State>>,
    params: &Value,
    runtime: Option<Arc<TunnelRuntime>>,
) -> Result<Value> {
    let name = params["name"]
        .as_str()
        .ok_or_else(|| Error::BadParams("name required".into()))?;

    let should_stop = {
        let guard = state.lock().unwrap();
        let status = &guard
            .tunnels
            .iter()
            .find(|t| t.name == name)
            .ok_or_else(|| Error::NotFound(name.to_owned()))?
            .status;
        matches!(status, TunnelStatus::Alive | TunnelStatus::Starting)
    };

    if should_stop {
        tunnel_stop(state, params, runtime)
    } else {
        tunnel_start(state, params, runtime)
    }
}

// ---------------------------------------------------------------------------
// tunnel_set_node
// ---------------------------------------------------------------------------

/// Set the target node for a tunnel, persist, then start it.
///
/// Mirrors `TunnelManager.set_node` in tunnels.py:
/// - Sets last_node / last_user.
/// - If was Idle/Failed/Stale → start.
/// - If was Alive/Starting AND the node changed → stop then start
///   (so the forward re-targets the new node).
pub fn tunnel_set_node(
    state: &Arc<Mutex<State>>,
    params: &Value,
    runtime: Option<Arc<TunnelRuntime>>,
) -> Result<Value> {
    let name = params["name"]
        .as_str()
        .ok_or_else(|| Error::BadParams("name required".into()))?
        .to_owned();
    // Normalize the raw SLURM nodelist (e.g. "holygpu[01-03]") to the first
    // concrete hostname ("holygpu01").  Plain hostnames pass through unchanged.
    // Mirrors daemon.py line 378: `node, _is_range = expand_first_node(node)`.
    let node = {
        let raw = params["node"]
            .as_str()
            .ok_or_else(|| Error::BadParams("node required".into()))?;
        let (expanded, _is_range) = expand_first_node(raw);
        expanded
    };
    let user = params
        .get("user")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned();

    let (old_node, old_status) = {
        let mut guard = state.lock().unwrap();
        let t = guard
            .tunnels
            .iter_mut()
            .find(|t| t.name == name)
            .ok_or_else(|| Error::NotFound(name.clone()))?;

        let prev_node = t.last_node.clone();
        let prev_status = t.status;

        t.last_node = Some(node.clone());
        if !user.is_empty() {
            t.last_user = Some(user);
        }
        t.last_msg = format!("Node set to {node}");

        (prev_node, prev_status)
    };

    // Persist the new node assignment.
    {
        let guard = state.lock().unwrap();
        let _ = save_tunnels(&guard.tunnels_path, &guard.tunnels);
    }

    let params_with_name = json!({"name": name});

    match old_status {
        TunnelStatus::Idle | TunnelStatus::Failed | TunnelStatus::Stale | TunnelStatus::PortBusy => {
            // Was idle / stuck — just start.
            // Mirrors Python: status ∈ {idle, stale, failed, port_busy} → start.
            tunnel_start(state, &params_with_name, runtime)?;
        }
        TunnelStatus::Alive | TunnelStatus::Starting => {
            // Was alive — only restart if the node actually changed.
            if old_node.as_deref() != Some(&node) {
                tunnel_stop(state, &params_with_name, runtime.clone())?;
                tunnel_start(state, &params_with_name, runtime)?;
            }
        }
    }

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
        // Filter to known hosts (drop unknown names).
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

        // If the tunnel is alive, mark it stopped before renaming so the
        // tick loop doesn't try to restart the old name.
        if t.status == TunnelStatus::Alive {
            t.status = TunnelStatus::Idle;
            t.wants_alive = false;
        }

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
            tunnel_start(state, &pv, None)
        } else {
            tunnel_stop(state, &pv, None)
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

/// Discover SLURM nodes via an existing SSH master ControlPath.
///
/// Mirrors `NodeDiscovery.discover(mgr)` in daemon.py.
///
/// Uses `discover_nodes_via_control` so the ssh call multiplexes over the
/// already-authenticated master socket — NO new 2FA prompt is triggered.
/// The ControlPath is obtained from `ssh::control::active_symlink_path(host)`.
///
/// Returns `[{jobid, partition, name, state, time, node}, …]`.
pub fn discover_nodes(state: &Arc<Mutex<State>>, params: &Value) -> Result<Value> {
    let host_name = params["host"]
        .as_str()
        .ok_or_else(|| Error::BadParams("host required".into()))?
        .to_owned();

    // Verify the host exists and its master is ready.
    {
        let guard = state.lock().unwrap();
        let host = guard
            .hosts
            .iter()
            .find(|h| h.host == host_name)
            .ok_or_else(|| Error::NotFound(host_name.clone()))?;

        if !host.is_master_ready {
            return Err(Error::Discovery(format!("{host_name} master not ready")));
        }
    }

    // Get the active ControlPath for the host.
    let cp = active_symlink_path(&host_name);

    // Run squeue via the master socket (blocking, but fast — local pipe).
    let jobs = discover_nodes_via_control(&host_name, &cp)?;

    let result: Vec<Value> = jobs
        .iter()
        .map(|j| {
            json!({
                "jobid": j.jobid,
                "partition": j.partition,
                "name": j.name,
                "state": j.state,
                "time": j.time,
                "node": j.node,
            })
        })
        .collect();

    Ok(json!(result))
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
    use a2fa_core::model::Host;
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

    fn make_alive_tunnel(name: &str, port: u16) -> Arc<Mutex<State>> {
        let t = Tunnel {
            name: name.into(),
            local_port: port,
            remote_port: port,
            jump_candidates: None,
            last_node: Some("holygpu01".into()),
            last_user: Some("jdoe".into()),
            auto_start: false,
            post_connect_cmd: None,
            tags: vec![],
            url_path: None,
            wants_alive: true,
            status: TunnelStatus::Alive,
            active_jump: Some("k6".into()),
            last_msg: "Connected".into(),
            last_alive_at: 0.0,
            total_uptime_sec: 0.0,
            connect_count: 1,
            fail_count: 0,
        };
        Arc::new(Mutex::new(State::with_tunnels(vec![t])))
    }

    fn make_tunnel_with_status(name: &str, port: u16, status: TunnelStatus) -> Arc<Mutex<State>> {
        let t = Tunnel {
            name: name.into(),
            local_port: port,
            remote_port: port,
            jump_candidates: None,
            last_node: Some("holygpu01".into()),
            last_user: Some("jdoe".into()),
            auto_start: false,
            post_connect_cmd: None,
            tags: vec![],
            url_path: None,
            wants_alive: true,
            status,
            active_jump: Some("k6".into()),
            last_msg: "In progress".into(),
            last_alive_at: 0.0,
            total_uptime_sec: 0.0,
            connect_count: 0,
            fail_count: 0,
        };
        Arc::new(Mutex::new(State::with_tunnels(vec![t])))
    }

    // ---- list_tunnels --------------------------------------------------

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

    // ---- tunnel_add ----------------------------------------------------

    #[test]
    fn tunnel_add_invalid_port_returns_bad_params() {
        let state = make_state();
        let err = tunnel_add(&state, &json!({"name": "t", "local_port": 80})).unwrap_err();
        assert!(matches!(err, Error::BadParams(_)));
    }

    #[test]
    fn tunnel_add_duplicate_name_returns_duplicate() {
        let state = make_state_with_tunnel("nb", 9100);
        let err = tunnel_add(&state, &json!({"name": "nb", "local_port": 9200})).unwrap_err();
        assert!(matches!(err, Error::Duplicate(_)));
    }

    // ---- tunnel_stop ---------------------------------------------------

    #[test]
    fn tunnel_stop_marks_idle_and_clears_wants_alive() {
        let state = make_alive_tunnel("nb", 9300);
        tunnel_stop(&state, &json!({"name": "nb"}), None).unwrap();
        let guard = state.lock().unwrap();
        let t = &guard.tunnels[0];
        assert_eq!(t.status, TunnelStatus::Idle);
        assert!(!t.wants_alive);
    }

    #[test]
    fn tunnel_stop_idempotent() {
        let state = make_state_with_tunnel("nb", 9301);
        // Already idle — should be a no-op, no error.
        tunnel_stop(&state, &json!({"name": "nb"}), None).unwrap();
        assert_eq!(state.lock().unwrap().tunnels[0].status, TunnelStatus::Idle);
    }

    #[test]
    fn tunnel_stop_unknown_name_returns_not_found() {
        let state = make_state();
        let err = tunnel_stop(&state, &json!({"name": "ghost"}), None).unwrap_err();
        assert!(matches!(err, Error::NotFound(_)));
    }

    // ---- tunnel_start (state-only; no real ssh) -------------------------

    #[test]
    fn tunnel_start_unknown_name_returns_not_found() {
        let state = make_state();
        let err = tunnel_start(&state, &json!({"name": "ghost"}), None).unwrap_err();
        assert!(matches!(err, Error::NotFound(_)));
    }

    #[test]
    fn tunnel_start_no_node_sets_idle_last_msg() {
        // Tunnel with no last_node → start should set last_msg and return Ok.
        let state = make_state_with_tunnel("nb", 9302);
        // No ready host → no jump; no node → picks the "no node" path.
        tunnel_start(&state, &json!({"name": "nb"}), None).unwrap();
        let msg = state.lock().unwrap().tunnels[0].last_msg.clone();
        assert!(msg.contains("no node") || msg.contains("waiting") || msg.contains("jump"));
    }

    // ---- tunnel_toggle -------------------------------------------------

    #[test]
    fn tunnel_toggle_alive_stops() {
        let state = make_alive_tunnel("nb", 9400);
        tunnel_toggle(&state, &json!({"name": "nb"}), None).unwrap();
        assert_eq!(state.lock().unwrap().tunnels[0].status, TunnelStatus::Idle);
    }

    /// Toggle on a Starting tunnel must stop it (FIX 3 — parity with Python).
    #[test]
    fn tunnel_toggle_starting_stops() {
        let state = make_tunnel_with_status("nb", 9401, TunnelStatus::Starting);
        tunnel_toggle(&state, &json!({"name": "nb"}), None).unwrap();
        assert_eq!(
            state.lock().unwrap().tunnels[0].status,
            TunnelStatus::Idle,
            "toggle on Starting tunnel must stop it"
        );
    }

    // ---- tunnel_set_node -----------------------------------------------

    #[test]
    fn tunnel_set_node_updates_last_node() {
        let state = make_state_with_tunnel("nb", 9500);
        tunnel_set_node(
            &state,
            &json!({"name": "nb", "node": "holygpu01", "user": "jdoe"}),
            None,
        )
        .unwrap();
        let guard = state.lock().unwrap();
        assert_eq!(guard.tunnels[0].last_node.as_deref(), Some("holygpu01"));
        assert_eq!(guard.tunnels[0].last_user.as_deref(), Some("jdoe"));
    }

    /// SLURM range strings must be normalised to the first concrete node before
    /// being stored (mirrors daemon.py line 378).
    #[test]
    fn tunnel_set_node_expands_slurm_range() {
        let state = make_state_with_tunnel("nb", 9501);
        tunnel_set_node(
            &state,
            &json!({"name": "nb", "node": "holygpu[01-03]", "user": "jdoe"}),
            None,
        )
        .unwrap();
        let guard = state.lock().unwrap();
        assert_eq!(
            guard.tunnels[0].last_node.as_deref(),
            Some("holygpu01"),
            "SLURM range must be expanded to first node before storage"
        );
    }

    #[test]
    fn tunnel_set_node_unknown_returns_not_found() {
        let state = make_state();
        let err = tunnel_set_node(
            &state,
            &json!({"name": "ghost", "node": "holygpu01"}),
            None,
        )
        .unwrap_err();
        assert!(matches!(err, Error::NotFound(_)));
    }

    /// set_node on a Stale tunnel must attempt a start (FIX 4 — parity with Python).
    #[test]
    fn tunnel_set_node_stale_attempts_start() {
        let state = make_tunnel_with_status("nb", 9502, TunnelStatus::Stale);
        tunnel_set_node(
            &state,
            &json!({"name": "nb", "node": "holygpu01", "user": "jdoe"}),
            None,
        )
        .unwrap();
        let guard = state.lock().unwrap();
        // After set_node on a stale tunnel, the tunnel should no longer be Stale;
        // it will be Idle (no ready jump host in test state) or Starting.
        assert_ne!(
            guard.tunnels[0].status,
            TunnelStatus::Stale,
            "stale tunnel must not stay Stale after set_node"
        );
    }

    /// set_node on a PortBusy tunnel must attempt a start (FIX 4 — parity with Python).
    #[test]
    fn tunnel_set_node_port_busy_attempts_start() {
        let state = make_tunnel_with_status("nb", 9503, TunnelStatus::PortBusy);
        tunnel_set_node(
            &state,
            &json!({"name": "nb", "node": "holygpu01", "user": "jdoe"}),
            None,
        )
        .unwrap();
        let guard = state.lock().unwrap();
        // After set_node on a PortBusy tunnel, it must not remain PortBusy.
        assert_ne!(
            guard.tunnels[0].status,
            TunnelStatus::PortBusy,
            "port_busy tunnel must not stay PortBusy after set_node"
        );
    }

    // ---- tunnel_rename -------------------------------------------------

    #[test]
    fn tunnel_rename_ok() {
        let state = make_state_with_tunnel("nb", 9600);
        let v = tunnel_rename(&state, &json!({"old": "nb", "new": "nb2"})).unwrap();
        assert_eq!(v["name"], "nb2");
        assert_eq!(state.lock().unwrap().tunnels[0].name, "nb2");
    }

    #[test]
    fn tunnel_rename_duplicate_returns_error() {
        let mut inner = State::with_tunnels(vec![]);
        for (name, port) in [("nb", 9700u16), ("nb2", 9701u16)] {
            inner.tunnels.push(Tunnel {
                name: name.into(),
                local_port: port,
                remote_port: port,
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

    // ---- discover_nodes ------------------------------------------------

    #[test]
    fn discover_nodes_missing_host_returns_not_found() {
        let state = make_state();
        let err = discover_nodes(&state, &json!({"host": "ghost"})).unwrap_err();
        assert!(matches!(err, Error::NotFound(_)));
    }

    #[test]
    fn discover_nodes_master_not_ready_returns_discovery_error() {
        let mut inner = State::with_tunnels(vec![]);
        inner.hosts.push(Host {
            host: "k6".into(),
            status: "Idle".into(),
            active: false,
            is_master_ready: false, // not ready
            pool_index: 0,
            pool_alive: 0,
            is_mounted: false,
            last_msg: "".into(),
        });
        let state = Arc::new(Mutex::new(inner));
        let err = discover_nodes(&state, &json!({"host": "k6"})).unwrap_err();
        assert!(matches!(err, Error::Discovery(_)));
    }

    // ---- port_suggest --------------------------------------------------

    #[test]
    fn port_suggest_returns_free_port() {
        let state = make_state();
        let v = port_suggest(&state, &json!({})).unwrap();
        let port = v["port"].as_u64().unwrap();
        assert!(port >= 1024);
    }

    // ---- tunnel_set_tags -----------------------------------------------

    #[test]
    fn tunnel_set_tags_and_retrieve() {
        let state = make_state_with_tunnel("nb", 9800);
        let v = tunnel_set_tags(
            &state,
            &json!({"name": "nb", "tags": ["ml", "gpu"]}),
        )
        .unwrap();
        assert_eq!(v["tags"], json!(["ml", "gpu"]));
    }

    // ---- tunnels_batch -------------------------------------------------

    #[test]
    fn tunnels_batch_bad_action() {
        let state = make_state();
        let err = tunnels_batch(&state, &json!({"action": "fly", "names": []})).unwrap_err();
        assert!(matches!(err, Error::BadParams(_)));
    }

    #[test]
    fn tunnels_batch_stop_unknown_reports_error_per_item() {
        let state = make_state();
        let v = tunnels_batch(
            &state,
            &json!({"action": "stop", "names": ["ghost"]}),
        )
        .unwrap();
        let results = v["results"].as_array().unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["ok"], false);
    }
}
