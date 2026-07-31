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

/// Persist the tunnel list to disk WITHOUT holding the State lock across the
/// fsync.
///
/// `save_tunnels` fsyncs, whose latency is unbounded under disk pressure / a
/// wedged FS (and the daemon manages sshfs mounts on the same machine). Holding
/// `Mutex<State>` across that fsync freezes the 0.5 s tick loop, the 3 s
/// heartbeat, the tunnel-maintenance loop, AND every IPC handler — a whole-
/// daemon wedge until the fsync returns. Snapshot path+tunnels under a brief
/// (poison-tolerant) lock, drop it, then save. Best-effort, matching the
/// already-correct off-lock sites (tunnel_add / tunnel_remove).
/// ALL tunnel persistence must go through here. The save lock serializes
/// writers AND the snapshot is taken INSIDE it, so rename order == snapshot
/// order: a stale snapshot can never land after a newer one. (Before this,
/// each site snapshotted then saved independently — per-call unique temp
/// files made every write internally consistent, but a maintenance save
/// holding a pre-add snapshot could rename over a later `tunnel_add` save,
/// leaving the new tunnel off disk until the next save: a crash in that
/// window lost it.)
///
/// Lock order: SAVE → State (brief clone) → drop State → fsync. Never call
/// this while already holding the State lock.
pub(crate) fn persist_tunnels(state: &Arc<Mutex<State>>) {
    static SAVE_LOCK: Mutex<()> = Mutex::new(());
    let _g = SAVE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (path, tunnels) = {
        let g = crate::lock_state(state);
        (g.tunnels_path.clone(), g.tunnels.clone())
    };
    let _ = save_tunnels(&path, &tunnels);
}

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
        "direct_host": t.direct_host,
        "auto_start": t.auto_start,
        "post_connect_cmd": t.post_connect_cmd,
        "tags": t.tags,
        "url_path": t.url_path,
        "active_jump": t.active_jump,
        "status": t.status,
        "last_msg": t.last_msg,
        "last_alive_at": t.last_alive_at,
        // LIVE uptime: total_uptime_sec only accumulates when a run ENDS, so an
        // Alive tunnel must add its current run here — otherwise a 6-hour first
        // run reports 0 the whole time. last_alive_at is stamped on each
        // idle→alive transition. (Python's _tunnel_snapshot used _live_uptime.)
        "total_uptime_sec": if t.status == TunnelStatus::Alive && t.last_alive_at > 0.0 {
            t.total_uptime_sec
                + (a2fa_core::tunnels::uptime::now_unix() - t.last_alive_at).max(0.0)
        } else {
            t.total_uptime_sec
        },
        "connect_count": t.connect_count,
        "fail_count": t.fail_count,
    })
}

// ---------------------------------------------------------------------------
// list_tunnels
// ---------------------------------------------------------------------------

pub fn list_tunnels(state: &Arc<Mutex<State>>) -> Result<Value> {
    let guard = crate::lock_state(state);
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

    // try_from (not `as u16`): `70000 as u16` silently truncates to 4464 and
    // would create a tunnel on a different port than requested.
    let local_port = params["local_port"]
        .as_u64()
        .and_then(|p| u16::try_from(p).ok())
        .ok_or_else(|| Error::BadParams("local_port required (1024..65535)".into()))?;

    if !is_valid_port(local_port) {
        return Err(Error::BadParams(format!(
            "local_port {local_port} out of range (1024..65535)"
        )));
    }

    let remote_port = match params.get("remote_port").and_then(|v| v.as_u64()) {
        Some(p) => u16::try_from(p)
            .map_err(|_| Error::BadParams(format!("remote_port {p} out of range (max 65535)")))?,
        None => local_port,
    };

    // Optional direct-mode target: forward straight to this registered host's
    // own localhost (no jump / no node). Reject a leading '-' (ssh arg injection).
    let direct_host: Option<String> = match params.get("direct_host") {
        None | Some(Value::Null) => None,
        Some(v) => {
            let s = v.as_str().unwrap_or("").trim().to_owned();
            if s.is_empty() {
                None
            } else if s.starts_with('-') {
                return Err(Error::BadParams(format!(
                    "invalid direct_host '{s}': must not start with '-'"
                )));
            } else {
                Some(s)
            }
        }
    };

    let mut guard = crate::lock_state(state);

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
        direct_host,
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
    drop(guard);

    // Persist — best effort; don't fail the add if the write fails.
    persist_tunnels(state);

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

    let mut guard = crate::lock_state(state);
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
    drop(guard);
    persist_tunnels(state);

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
    post_connect_running: Option<Arc<Mutex<HashSet<String>>>>,
) -> Result<Value> {
    let name = params["name"]
        .as_str()
        .ok_or_else(|| Error::BadParams("name required".into()))?
        .to_owned();

    let resolved: Option<(a2fa_core::tunnels::forward::ForwardSpec, u16, u16, Option<String>)> = {
        let mut guard = crate::lock_state(state);
        let t = guard
            .tunnels
            .iter()
            .find(|t| t.name == name)
            .ok_or_else(|| Error::NotFound(name.clone()))?;

        // Idempotent + in-flight latch.
        if matches!(t.status, TunnelStatus::Alive | TunnelStatus::Starting) {
            return Ok(Value::Null);
        }

        let direct_host = t.direct_host.clone();
        let local_port = t.local_port;
        let remote_port = t.remote_port;
        let post_cmd = t.post_connect_cmd.clone();

        match direct_host {
            // ---- Direct mode: forward to <host>'s own localhost ----
            Some(host) => {
                let ready = guard
                    .hosts
                    .iter()
                    .any(|h| h.host == host && h.is_master_ready);
                let t = guard.tunnels.iter_mut().find(|t| t.name == name).unwrap();
                if !ready {
                    // Park until the host's master is up; maintenance recovers it.
                    t.status = TunnelStatus::Idle;
                    t.last_msg = format!("waiting for host {host}");
                    t.active_jump = Some(host.clone());
                    t.wants_alive = true;
                    return Ok(Value::Null);
                }
                t.status = TunnelStatus::Starting;
                t.active_jump = Some(host.clone());
                t.last_msg = format!("starting direct to {host}");
                t.wants_alive = true;
                Some((
                    a2fa_core::tunnels::forward::ForwardSpec::Direct { host },
                    local_port,
                    remote_port,
                    post_cmd,
                ))
            }
            // ---- Compute mode: SLURM two-hop (unchanged) ----
            None => {
                let jump = guard
                    .hosts
                    .iter()
                    .find(|h| h.is_master_ready && {
                        let t = guard.tunnels.iter().find(|t| t.name == name).unwrap();
                        match &t.jump_candidates {
                            Some(cands) => cands.contains(&h.host),
                            None => true,
                        }
                    })
                    .map(|h| h.host.clone());

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
                t.status = TunnelStatus::Starting;
                t.active_jump = Some(jump.clone());
                t.last_msg = format!("starting via {jump}");
                t.wants_alive = true;
                Some((
                    a2fa_core::tunnels::forward::ForwardSpec::Compute { jump, user, node },
                    local_port,
                    remote_port,
                    post_cmd,
                ))
            }
        }
    };

    let (spec, local_port, remote_port, post_connect_cmd) = match resolved {
        Some(v) => v,
        None => return Ok(Value::Null),
    };

    // Spawn the blocking worker off-lock. Use the SHARED post-connect dedup set
    // (threaded in from DaemonCtx) so the IPC path and the maintenance loop
    // dedup against the SAME set — a fresh set here would make dedup a no-op for
    // the IPC path (concurrent duplicate hooks possible). Fall back to a fresh
    // set only for legacy callers that don't supply one (e.g. unit tests).
    let post_connect_running: Arc<Mutex<HashSet<String>>> =
        post_connect_running.unwrap_or_else(|| Arc::new(Mutex::new(HashSet::new())));

    match runtime {
        Some(rt) => spawn_tunnel_start_with_runtime(
            name,
            spec,
            local_port,
            remote_port,
            post_connect_cmd,
            Arc::clone(state),
            post_connect_running,
            rt,
        ),
        None => spawn_tunnel_start(
            name,
            spec,
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
        let mut guard = crate::lock_state(state);
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
            let mut guard = crate::lock_state(state);
            if let Some(t) = guard.tunnels.iter_mut().find(|t| t.name == name) {
                t.total_uptime_sec += delta;
            }
        }
    }

    // Persist the change.
    persist_tunnels(state);

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
    post_connect_running: Option<Arc<Mutex<HashSet<String>>>>,
) -> Result<Value> {
    let name = params["name"]
        .as_str()
        .ok_or_else(|| Error::BadParams("name required".into()))?;

    let should_stop = {
        let guard = crate::lock_state(state);
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
        tunnel_start(state, params, runtime, post_connect_running)
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
    post_connect_running: Option<Arc<Mutex<HashSet<String>>>>,
) -> Result<Value> {
    let name = params["name"]
        .as_str()
        .ok_or_else(|| Error::BadParams("name required".into()))?
        .to_owned();
    // Normalize the raw SLURM nodelist (e.g. "gpunode[01-03]") to the first
    // concrete hostname ("gpunode01").  Plain hostnames pass through unchanged.
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

    // `start` (default true): whether to (re)start the tunnel after recording
    // the node. Import passes `false` so restoring a backup persists each
    // tunnel's node WITHOUT firing N immediate SSH starts at possibly-dead
    // SLURM nodes (auto_start tunnels still come up on the next daemon boot).
    let do_start = params.get("start").and_then(|v| v.as_bool()).unwrap_or(true);

    let (old_node, old_status) = {
        let mut guard = crate::lock_state(state);
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

    // Persist the new node assignment (off-lock — no fsync under State lock).
    persist_tunnels(state);

    // Fresh node = fresh staleness budget. The miss counter accumulated
    // against the OLD node (legitimately gone from squeue) — carrying it over
    // meant the FIRST miss against the new node could cross the stale
    // threshold instantly (observed live: bastion01 re-noded, alive 4 s, then
    // "squeue miss #3" → killed).
    if let Some(rt) = &runtime {
        rt.with_rt_mut(&name, |r| r.consecutive_squeue_misses = 0);
    }

    if !do_start {
        // Node recorded + persisted above; caller asked us NOT to start.
        return Ok(Value::Null);
    }

    let params_with_name = json!({"name": name});

    match old_status {
        TunnelStatus::Idle | TunnelStatus::Failed | TunnelStatus::Stale | TunnelStatus::PortBusy => {
            // Was idle / stuck — just start.
            // Mirrors Python: status ∈ {idle, stale, failed, port_busy} → start.
            tunnel_start(state, &params_with_name, runtime, post_connect_running)?;
        }
        TunnelStatus::Alive | TunnelStatus::Starting => {
            // Was alive — only restart if the node actually changed.
            if old_node.as_deref() != Some(&node) {
                tunnel_stop(state, &params_with_name, runtime.clone())?;
                tunnel_start(state, &params_with_name, runtime, post_connect_running)?;
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
        let mut guard = crate::lock_state(state);
        let t = guard
            .tunnels
            .iter_mut()
            .find(|t| t.name == name)
            .ok_or_else(|| Error::NotFound(name.to_owned()))?;
        t.auto_start = value;
        tunnel_snapshot(t)
    };

    persist_tunnels(state);
    Ok(snap)
}

// ---------------------------------------------------------------------------
// tunnel_set_ports
// ---------------------------------------------------------------------------

/// Change a tunnel's local and/or remote port after creation.
///
/// Ports used to be fixed at creation: hitting a local-port clash meant deleting
/// the tunnel and building it again, which silently discarded its tags,
/// post-connect hook, URL path and jump pinning. Everything else about a tunnel
/// is editable; the ports were the one exception.
///
/// Params: `name`, plus at least one of `local_port` / `remote_port`.
/// A live tunnel is STOPPED here — the running `ssh -L` is bound to the old
/// port, so leaving it up would mean the UI advertises a port the child isn't
/// serving. `wants_alive` is preserved, so the maintenance loop brings it back
/// on the new port without the caller orchestrating a restart.
pub fn tunnel_set_ports(
    state: &Arc<Mutex<State>>,
    params: &Value,
    runtime: Option<Arc<TunnelRuntime>>,
) -> Result<Value> {
    let name = params["name"]
        .as_str()
        .ok_or_else(|| Error::BadParams("name required".into()))?
        .to_owned();

    // Parse before touching anything: a rejected value must change nothing.
    let new_local = parse_port_param(params, "local_port")?;
    let new_remote = parse_port_param(params, "remote_port")?;
    if new_local.is_none() && new_remote.is_none() {
        return Err(Error::BadParams(
            "nothing to change — pass 'local_port' and/or 'remote_port'".into(),
        ));
    }

    // Confirm the tunnel exists BEFORE the port checks. The bind check below
    // momentarily binds the port, which is a real side effect: doing it for a
    // request that is going to fail anyway needlessly disturbs whatever else is
    // probing that port. (Caught by a flaky test — this transient bind raced
    // another test's own bind check on the same port.)
    {
        let guard = crate::lock_state(state);
        if !guard.tunnels.iter().any(|t| t.name == name) {
            return Err(Error::NotFound(name.clone()));
        }
    }

    // Reject a local port already claimed by ANOTHER tunnel. Two tunnels on one
    // local port is a guaranteed ExitOnForwardFailure death for whichever starts
    // second, and the failure surfaces far from this edit.
    if let Some(port) = new_local {
        let taken_by = {
            let guard = crate::lock_state(state);
            guard
                .tunnels
                .iter()
                .find(|t| t.name != name && t.local_port == port)
                .map(|t| t.name.clone())
        };
        if let Some(other) = taken_by {
            return Err(Error::BadParams(format!(
                "local port {port} is already used by tunnel '{other}'"
            )));
        }
        // …and by anything ELSE on this Mac. `tunnel_add` has always done this
        // bind check; leaving it out here accepted a port held by an unrelated
        // process (another dev server, a stale forward) and let it fail later at
        // ExitOnForwardFailure, far from the edit that caused it. Skipped when
        // the tunnel already holds the port itself — its own live child binds
        // it, so the check would reject a no-op edit.
        let already_ours = {
            let guard = crate::lock_state(state);
            guard.tunnels.iter().any(|t| t.name == name && t.local_port == port)
        };
        if !already_ours && port_in_use(port) {
            return Err(Error::PortInUse(port));
        }
    }

    // Was it running? Stop it before the ports change so the child that is bound
    // to the OLD local port is never left masquerading as the new one.
    let was_alive = {
        let guard = crate::lock_state(state);
        guard
            .tunnels
            .iter()
            .find(|t| t.name == name)
            .map(|t| {
                matches!(
                    t.status,
                    TunnelStatus::Alive | TunnelStatus::Starting | TunnelStatus::Stale
                )
            })
            .ok_or_else(|| Error::NotFound(name.clone()))?
    };
    if was_alive {
        if let Some(rt) = &runtime {
            rt.kill_child(&name); // off the State lock, like tunnel_remove
        }
    }

    let snap = {
        let mut guard = crate::lock_state(state);
        let t = guard
            .tunnels
            .iter_mut()
            .find(|t| t.name == name)
            .ok_or_else(|| Error::NotFound(name.clone()))?;
        if let Some(p) = new_local {
            t.local_port = p;
        }
        if let Some(p) = new_remote {
            t.remote_port = p;
        }
        if was_alive {
            // Keep wants_alive as-is: the maintenance loop restarts it on the
            // new port. Only the observable status resets.
            t.status = TunnelStatus::Idle;
            t.last_msg = "Ports changed — restarting".into();
        }
        tunnel_snapshot(t)
    };

    persist_tunnels(state);
    Ok(snap)
}

/// Read an optional u16 port param. Present-but-invalid is an error (never a
/// silent clamp): `0` is not a bindable port and >65535 is not a port at all.
fn parse_port_param(params: &Value, key: &str) -> Result<Option<u16>> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => {
            let n = v
                .as_u64()
                .ok_or_else(|| Error::BadParams(format!("{key} must be a number")))?;
            if !(1..=65535).contains(&n) {
                return Err(Error::BadParams(format!(
                    "{key} {n} out of range (1-65535)"
                )));
            }
            Ok(Some(n as u16))
        }
    }
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
        let mut guard = crate::lock_state(state);
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

    persist_tunnels(state);
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
        let mut guard = crate::lock_state(state);
        let t = guard
            .tunnels
            .iter_mut()
            .find(|t| t.name == name)
            .ok_or_else(|| Error::NotFound(name.to_owned()))?;
        t.post_connect_cmd = cmd;
        tunnel_snapshot(t)
    };

    persist_tunnels(state);
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
        let mut guard = crate::lock_state(state);
        let t = guard
            .tunnels
            .iter_mut()
            .find(|t| t.name == name)
            .ok_or_else(|| Error::NotFound(name.to_owned()))?;
        t.tags = tags;
        tunnel_snapshot(t)
    };

    persist_tunnels(state);
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
        let mut guard = crate::lock_state(state);
        let t = guard
            .tunnels
            .iter_mut()
            .find(|t| t.name == name)
            .ok_or_else(|| Error::NotFound(name.to_owned()))?;
        t.url_path = path;
        tunnel_snapshot(t)
    };

    persist_tunnels(state);
    Ok(snap)
}

// ---------------------------------------------------------------------------
// tunnel_rename
// ---------------------------------------------------------------------------

pub fn tunnel_rename(
    state: &Arc<Mutex<State>>,
    params: &Value,
    runtime: Option<Arc<TunnelRuntime>>,
) -> Result<Value> {
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
        let mut guard = crate::lock_state(state);

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

        t.name = new.clone();
        tunnel_snapshot(t)
    };

    // Re-key the runtime registry (live child + counters + events) so an
    // Alive tunnel keeps running seamlessly under its new name. The old code
    // instead marked it Idle/wants_alive=false but NEVER killed the child —
    // leaking an untracked ssh forward under the old name. (Python migrated
    // its running-set entry on rename, tunnels.py.)
    if let Some(rt) = &runtime {
        rt.rename_entry(old, &new);
    }

    persist_tunnels(state);
    Ok(snap)
}

// ---------------------------------------------------------------------------
// tunnels_batch
// ---------------------------------------------------------------------------

/// Maximum number of tunnel starts to kick off concurrently in one batch.
///
/// A `tunnels_batch{action:"start"}` request naming N idle tunnels would, with
/// no cap, fan out N concurrent `ssh -L` children + start-worker threads at
/// once. Each `tunnel_start` call only SPAWNS the worker (it returns after
/// flipping the tunnel to `Starting` and dispatching off-lock), so the bound
/// here limits how many starts we INITIATE per pass; the per-tunnel `Starting`
/// latch already prevents duplicate starts of the same tunnel. We process the
/// requested names in chunks of this size, joining each chunk's spawn-and-flip
/// work before moving on, so a giant request can't initiate an unbounded burst.
const BATCH_START_CONCURRENCY: usize = 4;

pub fn tunnels_batch(
    state: &Arc<Mutex<State>>,
    params: &Value,
    runtime: Option<Arc<TunnelRuntime>>,
    post_connect_running: Option<Arc<Mutex<HashSet<String>>>>,
) -> Result<Value> {
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

    if action == "stop" {
        // Stop is cheap (flip flag + SIGKILL off-lock); no concurrency hazard.
        for name in &names {
            let pv = json!({"name": name});
            match tunnel_stop(state, &pv, runtime.clone()) {
                Ok(_) => results.push(json!({"name": name, "ok": true})),
                Err(e) => results.push(json!({"name": name, "ok": false, "error": e.to_string()})),
            }
        }
        return Ok(json!({ "results": results }));
    }

    // action == "start": cap how many starts we initiate at once. tunnel_start
    // returns quickly (spawns the ssh worker off-lock), so the cap needs a real
    // pause BETWEEN chunks — without it the chunked loop was equivalent to a
    // flat one and a 50-name batch fired 50 concurrent ssh children at once.
    // ~1 s of stagger per chunk lets the previous chunk's spawns get through
    // their initial connect before the next burst. (Runs on a connection
    // thread; the client's tunnels_batch timeout is 30 s, so even very large
    // batches stay within budget.)
    for (i, chunk) in names.chunks(BATCH_START_CONCURRENCY).enumerate() {
        if i > 0 {
            std::thread::sleep(std::time::Duration::from_millis(1000));
        }
        for name in chunk {
            let pv = json!({"name": name});
            let outcome = tunnel_start(state, &pv, runtime.clone(), post_connect_running.clone());
            match outcome {
                Ok(_) => results.push(json!({"name": name, "ok": true})),
                Err(e) => results.push(json!({"name": name, "ok": false, "error": e.to_string()})),
            }
        }
    }

    Ok(json!({ "results": results }))
}

// ---------------------------------------------------------------------------
// tunnel_events
// ---------------------------------------------------------------------------

pub fn tunnel_events(
    state: &Arc<Mutex<State>>,
    params: &Value,
    runtime: Option<Arc<TunnelRuntime>>,
) -> Result<Value> {
    let name = params["name"]
        .as_str()
        .ok_or_else(|| Error::BadParams("name required".into()))?;

    // Validate tunnel exists.
    {
        let guard = crate::lock_state(state);
        guard
            .tunnels
            .iter()
            .find(|t| t.name == name)
            .ok_or_else(|| Error::NotFound(name.to_owned()))?;
    }

    let events: Vec<Value> = match runtime {
        Some(rt) => rt
            .events(name)
            .into_iter()
            .map(|e| json!({"ts": e.ts, "msg": e.msg}))
            .collect(),
        None => vec![],
    };

    Ok(json!({ "events": events }))
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
        let guard = crate::lock_state(state);
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

    // Optional explicit cluster account — the jump may log in as a different
    // user than the one whose jobs the caller wants (alice → bob while
    // the jobs belong to alice). Empty/absent → remote $USER.
    let user = params
        .get("user")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());

    // Run squeue via the master socket (blocking, but fast — local pipe).
    let jobs = discover_nodes_via_control(&host_name, &cp, user)?;

    let result: Vec<Value> = jobs
        .iter()
        .map(|j| {
            json!({
                "jobid": j.jobid,
                "partition": j.partition,
                "name": j.name,
                "state": j.state,
                "time": j.time,
                "time_left": j.time_left,
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
    // try_from (not `as u16`): an oversized base (e.g. a typo "99999") used to
    // truncate to a random-looking port; fall back to the default instead.
    let base = params
        .get("base")
        .and_then(|v| v.as_u64())
        .and_then(|p| u16::try_from(p).ok())
        .unwrap_or(8888);

    let taken: Vec<u16> = {
        let guard = crate::lock_state(state);
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
            direct_host: None,
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
            last_node: Some("gpunode01".into()),
            last_user: Some("jdoe".into()),
            direct_host: None,
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
            last_node: Some("gpunode01".into()),
            last_user: Some("jdoe".into()),
            direct_host: None,
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

    // ---- tunnel_set_ports ----------------------------------------------

    #[test]
    fn set_ports_changes_both_ports_and_persists_shape() {
        let state = make_tunnel_with_status("nb", 8888, TunnelStatus::Idle);
        let snap = tunnel_set_ports(
            &state,
            &json!({"name": "nb", "local_port": 9001, "remote_port": 9002}),
            None,
        )
        .unwrap();
        assert_eq!(snap["local_port"], 9001);
        assert_eq!(snap["remote_port"], 9002);
        let guard = crate::lock_state(&state);
        let t = guard.tunnels.iter().find(|t| t.name == "nb").unwrap();
        assert_eq!(t.local_port, 9001);
        assert_eq!(t.remote_port, 9002);
    }

    /// A partial edit must leave the other port alone.
    #[test]
    fn set_ports_local_only_leaves_remote_untouched() {
        let state = make_tunnel_with_status("nb", 8888, TunnelStatus::Idle);
        tunnel_set_ports(&state, &json!({"name": "nb", "local_port": 9100}), None).unwrap();
        let guard = crate::lock_state(&state);
        let t = guard.tunnels.iter().find(|t| t.name == "nb").unwrap();
        assert_eq!(t.local_port, 9100);
        assert_eq!(t.remote_port, 8888, "remote port must be untouched");
    }

    /// The whole point of the feature: editing ports must NOT cost the user the
    /// tunnel's other settings (which delete-and-recreate did).
    #[test]
    fn set_ports_preserves_tags_hook_and_jump_settings() {
        let state = make_tunnel_with_status("nb", 8888, TunnelStatus::Idle);
        {
            let mut guard = crate::lock_state(&state);
            let t = guard.tunnels.iter_mut().find(|t| t.name == "nb").unwrap();
            t.tags = vec!["gpu".into(), "jupyter".into()];
            t.post_connect_cmd = Some("open -a Safari".into());
            t.url_path = Some("/?token=abc".into());
            t.auto_start = true;
        }
        tunnel_set_ports(&state, &json!({"name": "nb", "local_port": 9200}), None).unwrap();
        let guard = crate::lock_state(&state);
        let t = guard.tunnels.iter().find(|t| t.name == "nb").unwrap();
        assert_eq!(t.tags, vec!["gpu".to_string(), "jupyter".to_string()]);
        assert_eq!(t.post_connect_cmd.as_deref(), Some("open -a Safari"));
        assert_eq!(t.url_path.as_deref(), Some("/?token=abc"));
        assert!(t.auto_start);
    }

    /// A live tunnel is stopped (its child is bound to the OLD port) but stays
    /// wanted, so the maintenance loop brings it back on the new port.
    #[test]
    fn set_ports_on_live_tunnel_resets_status_but_keeps_wants_alive() {
        let state = make_tunnel_with_status("nb", 8888, TunnelStatus::Alive);
        tunnel_set_ports(&state, &json!({"name": "nb", "local_port": 9300}), None).unwrap();
        let guard = crate::lock_state(&state);
        let t = guard.tunnels.iter().find(|t| t.name == "nb").unwrap();
        assert_eq!(t.status, TunnelStatus::Idle, "live tunnel must be stopped");
        assert!(t.wants_alive, "it must come back on the new port");
    }

    #[test]
    fn set_ports_requires_at_least_one_port() {
        let state = make_tunnel_with_status("nb", 8888, TunnelStatus::Idle);
        let err = tunnel_set_ports(&state, &json!({"name": "nb"}), None).unwrap_err();
        assert!(matches!(err, Error::BadParams(ref m) if m.contains("nothing to change")));
    }

    /// Out-of-range / non-numeric values must be rejected outright — never
    /// clamped into something that silently isn't what the user typed.
    #[test]
    fn set_ports_rejects_invalid_values_without_mutating() {
        let state = make_tunnel_with_status("nb", 8888, TunnelStatus::Idle);
        for bad in [json!(0), json!(65536), json!("8080")] {
            let err =
                tunnel_set_ports(&state, &json!({"name": "nb", "local_port": bad}), None)
                    .unwrap_err();
            assert!(matches!(err, Error::BadParams(_)), "must reject {bad}");
        }
        let guard = crate::lock_state(&state);
        assert_eq!(guard.tunnels[0].local_port, 8888, "nothing may change on error");
    }

    /// Two tunnels on one local port is a guaranteed ExitOnForwardFailure death
    /// for whichever starts second — reject at the edit, not at start time.
    #[test]
    fn set_ports_rejects_a_port_owned_by_another_tunnel() {
        let state = make_tunnel_with_status("nb", 8888, TunnelStatus::Idle);
        {
            let mut guard = crate::lock_state(&state);
            let mut other = guard.tunnels[0].clone();
            other.name = "other".into();
            other.local_port = 9999;
            guard.tunnels.push(other);
        }
        let err = tunnel_set_ports(&state, &json!({"name": "nb", "local_port": 9999}), None)
            .unwrap_err();
        assert!(matches!(err, Error::BadParams(ref m) if m.contains("already used")));
        // Keeping its OWN port is not a conflict with itself.
        tunnel_set_ports(&state, &json!({"name": "nb", "local_port": 8888}), None).unwrap();
    }

    /// `tunnel_add` has always refused a port held by any other process; the
    /// port EDITOR did not, so it accepted a port owned by an unrelated program
    /// and deferred the failure to ExitOnForwardFailure at start time.
    #[test]
    fn set_ports_rejects_a_port_held_by_another_process() {
        let state = make_tunnel_with_status("nb", 8888, TunnelStatus::Idle);
        // Hold a real port for the duration of the check.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let taken = listener.local_addr().unwrap().port();

        let err = tunnel_set_ports(&state, &json!({"name": "nb", "local_port": taken}), None)
            .unwrap_err();
        assert!(matches!(err, Error::PortInUse(p) if p == taken), "got {err:?}");
        assert_eq!(
            crate::lock_state(&state).tunnels[0].local_port, 8888,
            "a rejected edit must change nothing"
        );
        drop(listener);
    }

    /// Re-submitting the tunnel's OWN port must not be rejected — its own live
    /// child is what binds it.
    #[test]
    fn set_ports_allows_resubmitting_its_own_port() {
        let state = make_tunnel_with_status("nb", 8888, TunnelStatus::Idle);
        tunnel_set_ports(&state, &json!({"name": "nb", "local_port": 8888, "remote_port": 7000}), None)
            .unwrap();
        let guard = crate::lock_state(&state);
        assert_eq!(guard.tunnels[0].remote_port, 7000);
    }

    /// An unknown tunnel is rejected BEFORE the port checks — the bind check is
    /// a real side effect (it momentarily binds the port) and must not run for
    /// a request that cannot succeed.
    #[test]
    fn set_ports_unknown_tunnel_is_not_found_without_touching_the_port() {
        let state = make_tunnel_with_status("nb", 8888, TunnelStatus::Idle);
        // Hold the port: if the check ran before the existence check, this
        // would surface as PortInUse instead of NotFound.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let held = listener.local_addr().unwrap().port();
        let err = tunnel_set_ports(&state, &json!({"name": "ghost", "local_port": held}), None)
            .unwrap_err();
        assert!(matches!(err, Error::NotFound(_)), "got {err:?}");
        drop(listener);
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
        let guard = crate::lock_state(&state);
        let t = &guard.tunnels[0];
        assert_eq!(t.status, TunnelStatus::Idle);
        assert!(!t.wants_alive);
    }

    #[test]
    fn tunnel_stop_idempotent() {
        let state = make_state_with_tunnel("nb", 9301);
        // Already idle — should be a no-op, no error.
        tunnel_stop(&state, &json!({"name": "nb"}), None).unwrap();
        assert_eq!(crate::lock_state(&state).tunnels[0].status, TunnelStatus::Idle);
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
        let err = tunnel_start(&state, &json!({"name": "ghost"}), None, None).unwrap_err();
        assert!(matches!(err, Error::NotFound(_)));
    }

    #[test]
    fn tunnel_start_no_node_sets_idle_last_msg() {
        // Tunnel with no last_node → start should set last_msg and return Ok.
        let state = make_state_with_tunnel("nb", 9302);
        // No ready host → no jump; no node → picks the "no node" path.
        tunnel_start(&state, &json!({"name": "nb"}), None, None).unwrap();
        let msg = crate::lock_state(&state).tunnels[0].last_msg.clone();
        assert!(msg.contains("no node") || msg.contains("waiting") || msg.contains("jump"));
    }

    /// FIX (unbounded ssh -L spawn): calling `tunnel_start` on a tunnel that
    /// is already `Alive` must be an idempotent no-op — no spawn, status stays
    /// Alive, last_msg unchanged.
    #[test]
    fn tunnel_start_already_alive_is_noop() {
        let state = make_alive_tunnel("nb", 9310);
        let before = crate::lock_state(&state).tunnels[0].last_msg.clone();
        let v = tunnel_start(&state, &json!({"name": "nb"}), None, None).unwrap();
        assert_eq!(v, Value::Null);
        let guard = crate::lock_state(&state);
        let t = &guard.tunnels[0];
        assert_eq!(t.status, TunnelStatus::Alive, "status must stay Alive");
        assert_eq!(t.last_msg, before, "early-return must not touch last_msg");
    }

    /// FIX (unbounded ssh -L spawn): calling `tunnel_start` on a tunnel that is
    /// already `Starting` must take the same idempotent early-return — the
    /// `Starting` status is the in-flight latch, so a repeat IPC call during the
    /// ~10s probe window must NOT spawn another worker.
    ///
    /// We assert the early-return path is taken by proving the handler did NOT
    /// fall through to the node/jump-host resolution code: that code would, on
    /// this host-less test state, rewrite status away from Starting (to Idle /
    /// Failed) and overwrite last_msg. Since the guard early-returns first,
    /// status stays Starting and last_msg is untouched.
    #[test]
    fn tunnel_start_already_starting_is_noop() {
        let state = make_tunnel_with_status("nb", 9311, TunnelStatus::Starting);
        let before = crate::lock_state(&state).tunnels[0].last_msg.clone();
        let v = tunnel_start(&state, &json!({"name": "nb"}), None, None).unwrap();
        assert_eq!(v, Value::Null);
        let guard = crate::lock_state(&state);
        let t = &guard.tunnels[0];
        assert_eq!(
            t.status,
            TunnelStatus::Starting,
            "Starting must stay Starting (early-return latch, no spawn)"
        );
        assert_eq!(
            t.last_msg, before,
            "early-return must not touch last_msg (proves no fall-through)"
        );
    }

    // ---- tunnel_toggle -------------------------------------------------

    #[test]
    fn tunnel_toggle_alive_stops() {
        let state = make_alive_tunnel("nb", 9400);
        tunnel_toggle(&state, &json!({"name": "nb"}), None, None).unwrap();
        assert_eq!(crate::lock_state(&state).tunnels[0].status, TunnelStatus::Idle);
    }

    /// Toggle on a Starting tunnel must stop it (FIX 3 — parity with Python).
    #[test]
    fn tunnel_toggle_starting_stops() {
        let state = make_tunnel_with_status("nb", 9401, TunnelStatus::Starting);
        tunnel_toggle(&state, &json!({"name": "nb"}), None, None).unwrap();
        assert_eq!(
            crate::lock_state(&state).tunnels[0].status,
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
            &json!({"name": "nb", "node": "gpunode01", "user": "jdoe"}),
            None,
            None,
        )
        .unwrap();
        let guard = crate::lock_state(&state);
        assert_eq!(guard.tunnels[0].last_node.as_deref(), Some("gpunode01"));
        assert_eq!(guard.tunnels[0].last_user.as_deref(), Some("jdoe"));
    }

    /// start:false records the node but must NOT start the tunnel (import of a
    /// backup must not fire N immediate SSH starts). The idle tunnel stays
    /// idle and the node is persisted.
    #[test]
    fn tunnel_set_node_start_false_does_not_start() {
        let state = make_state_with_tunnel("nb", 9503);
        let v = tunnel_set_node(
            &state,
            &json!({"name": "nb", "node": "gpunode07", "user": "jdoe", "start": false}),
            None,
            None,
        )
        .unwrap();
        assert_eq!(v, Value::Null);
        let guard = crate::lock_state(&state);
        assert_eq!(guard.tunnels[0].last_node.as_deref(), Some("gpunode07"));
        // Idle stays Idle — no start was attempted (no ssh spawn in tests).
        assert_eq!(guard.tunnels[0].status, TunnelStatus::Idle);
    }

    /// REGRESSION (bastion01, observed live): the miss counter accumulated
    /// against the OLD node must reset on set_node — carrying it over meant
    /// the FIRST miss against the new node crossed the stale threshold
    /// (re-noded tunnel alive 4 s, then "squeue miss #3" → killed).
    #[test]
    fn tunnel_set_node_resets_squeue_miss_counter() {
        let state = make_state_with_tunnel("nb", 9502);
        // Tunnel was Alive on the old node so set_node with the SAME node
        // takes the no-restart branch (no ssh spawn in tests).
        {
            let mut guard = crate::lock_state(&state);
            guard.tunnels[0].status = TunnelStatus::Alive;
            guard.tunnels[0].last_node = Some("gpunode99".into());
        }
        let rt = TunnelRuntime::new();
        rt.with_rt_mut("nb", |r| r.consecutive_squeue_misses = 2);

        tunnel_set_node(
            &state,
            &json!({"name": "nb", "node": "gpunode99", "user": "jdoe"}),
            Some(Arc::clone(&rt)),
            None,
        )
        .unwrap();

        let misses = rt.with_rt_mut("nb", |r| r.consecutive_squeue_misses);
        assert_eq!(misses, 0, "set_node must reset the staleness budget");
    }

    /// SLURM range strings must be normalised to the first concrete node before
    /// being stored (mirrors daemon.py line 378).
    #[test]
    fn tunnel_set_node_expands_slurm_range() {
        let state = make_state_with_tunnel("nb", 9501);
        tunnel_set_node(
            &state,
            &json!({"name": "nb", "node": "gpunode[01-03]", "user": "jdoe"}),
            None,
            None,
        )
        .unwrap();
        let guard = crate::lock_state(&state);
        assert_eq!(
            guard.tunnels[0].last_node.as_deref(),
            Some("gpunode01"),
            "SLURM range must be expanded to first node before storage"
        );
    }

    #[test]
    fn tunnel_set_node_unknown_returns_not_found() {
        let state = make_state();
        let err = tunnel_set_node(
            &state,
            &json!({"name": "ghost", "node": "gpunode01"}),
            None,
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
            &json!({"name": "nb", "node": "gpunode01", "user": "jdoe"}),
            None,
            None,
        )
        .unwrap();
        let guard = crate::lock_state(&state);
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
            &json!({"name": "nb", "node": "gpunode01", "user": "jdoe"}),
            None,
            None,
        )
        .unwrap();
        let guard = crate::lock_state(&state);
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
        let v = tunnel_rename(&state, &json!({"old": "nb", "new": "nb2"}), None).unwrap();
        assert_eq!(v["name"], "nb2");
        assert_eq!(crate::lock_state(&state).tunnels[0].name, "nb2");
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
                direct_host: None,
                auto_start: false, post_connect_cmd: None, tags: vec![],
                url_path: None, wants_alive: false, status: TunnelStatus::Idle,
                active_jump: None, last_msg: "Ready".into(), last_alive_at: 0.0,
                total_uptime_sec: 0.0, connect_count: 0, fail_count: 0,
            });
        }
        let state = Arc::new(Mutex::new(inner));
        let err = tunnel_rename(&state, &json!({"old": "nb", "new": "nb2"}), None).unwrap_err();
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

    // ---- tunnel_events -------------------------------------------------

    #[test]
    fn tunnel_events_unknown_tunnel_returns_not_found() {
        let state = make_state();
        let err = tunnel_events(&state, &json!({"name": "ghost"}), None).unwrap_err();
        assert!(matches!(err, Error::NotFound(_)));
    }

    #[test]
    fn tunnel_events_no_runtime_returns_empty_events() {
        let state = make_state_with_tunnel("nb", 9900);
        let v = tunnel_events(&state, &json!({"name": "nb"}), None).unwrap();
        let evs = v["events"].as_array().unwrap();
        assert!(evs.is_empty());
    }

    #[test]
    fn tunnel_events_with_runtime_returns_recorded_events() {
        use crate::tunnel_runtime::TunnelRuntime;

        let state = make_state_with_tunnel("nb", 9901);
        let rt = TunnelRuntime::new();
        rt.record("nb", 1000.0, "connected");
        rt.record("nb", 1001.0, "alive");

        let v = tunnel_events(&state, &json!({"name": "nb"}), Some(Arc::clone(&rt))).unwrap();
        let evs = v["events"].as_array().unwrap();
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[0]["ts"], 1000.0);
        assert_eq!(evs[0]["msg"], "connected");
        assert_eq!(evs[1]["ts"], 1001.0);
        assert_eq!(evs[1]["msg"], "alive");
    }

    // ---- tunnels_batch -------------------------------------------------

    #[test]
    fn tunnels_batch_bad_action() {
        let state = make_state();
        let err = tunnels_batch(&state, &json!({"action": "fly", "names": []}), None, None).unwrap_err();
        assert!(matches!(err, Error::BadParams(_)));
    }

    #[test]
    fn tunnels_batch_stop_unknown_reports_error_per_item() {
        let state = make_state();
        let v = tunnels_batch(
            &state,
            &json!({"action": "stop", "names": ["ghost"]}),
            None,
            None,
        )
        .unwrap();
        let results = v["results"].as_array().unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["ok"], false);
    }

    /// FIX (unbounded breadth): a start batch naming many tunnels must process
    /// ALL of them (chunked under the concurrency cap) and return one result per
    /// name. On this host-less test state each start takes the "no jump/node"
    /// early path, so none actually spawn ssh — we just assert the breadth-cap
    /// loop covers every requested name.
    #[test]
    fn tunnels_batch_start_processes_all_names_under_cap() {
        let state = make_state_with_tunnel("nb", 9500);
        // 9 names (> BATCH_START_CONCURRENCY=4) → must still yield 9 results.
        let names: Vec<String> = (0..9).map(|i| format!("missing-{i}")).collect();
        // Include the real one too.
        let mut all = vec!["nb".to_string()];
        all.extend(names);
        let v = tunnels_batch(
            &state,
            &json!({"action": "start", "names": all}),
            None,
            None,
        )
        .unwrap();
        let results = v["results"].as_array().unwrap();
        assert_eq!(results.len(), 10, "every requested name must get a result");
    }

    // ---- direct mode ---------------------------------------------------

    #[test]
    fn tunnel_add_direct_host_stored_and_in_snapshot() {
        let state = make_state();
        let snap = tunnel_add(
            &state,
            &json!({"name": "web", "local_port": 9000, "direct_host": "loginhost"}),
        )
        .unwrap();
        assert_eq!(snap["direct_host"], "loginhost");
        let guard = crate::lock_state(&state);
        assert_eq!(guard.tunnels[0].direct_host.as_deref(), Some("loginhost"));
    }

    #[test]
    fn tunnel_add_without_direct_host_is_none() {
        let state = make_state();
        let snap = tunnel_add(&state, &json!({"name": "nb", "local_port": 8888})).unwrap();
        assert!(snap["direct_host"].is_null());
        assert_eq!(crate::lock_state(&state).tunnels[0].direct_host, None);
    }

    #[test]
    fn tunnel_add_direct_host_leading_dash_rejected() {
        let state = make_state();
        let err = tunnel_add(
            &state,
            &json!({"name": "x", "local_port": 9001, "direct_host": "-oProxyCommand=x"}),
        )
        .unwrap_err();
        assert!(matches!(err, Error::BadParams(_)));
    }

    /// A direct tunnel whose host is not registered/ready must NOT spawn — it
    /// parks Idle with a "waiting for host" message (maintenance recovers it).
    #[test]
    fn tunnel_start_direct_no_ready_host_waits() {
        let state = make_state();
        tunnel_add(
            &state,
            &json!({"name": "web", "local_port": 9002, "direct_host": "loginhost"}),
        )
        .unwrap();
        tunnel_start(&state, &json!({"name": "web"}), None, None).unwrap();
        let guard = crate::lock_state(&state);
        let t = &guard.tunnels[0];
        assert_eq!(t.status, TunnelStatus::Idle);
        assert!(t.last_msg.contains("waiting for host"), "got: {}", t.last_msg);
        assert_eq!(t.active_jump.as_deref(), Some("loginhost"));
        assert!(t.wants_alive, "wants_alive must be set so maintenance retries");
    }
}
