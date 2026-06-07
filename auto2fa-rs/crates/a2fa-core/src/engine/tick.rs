//! Tick loop — polls state, emits change events, and drives maintenance work.
//!
//! # Design
//!
//! Mirrors `Auto2FADaemon._state_poll_loop` in `daemon.py`. One pass:
//!
//! 1. **Snapshot current stable-keys** under lock.
//! 2. **Compute new stable-keys** from the current model state (also under
//!    the same brief lock — no I/O).
//! 3. **Emit events** for any key that changed.
//! 4. **Update bookmarks** (`last_host_keys` / `last_tunnel_keys`).
//!
//! Actual SSH / tunnel maintenance (heartbeat probes, forward restarts) is
//! **structurally present** as a TODO stub. When wired, the blocking calls
//! will happen **off-lock** on a Rayon thread pool or `std::thread::spawn`,
//! and results will be written back under a brief re-lock. See the DEFERRED
//! note below.
//!
//! # DEFERRED
//!
//! - SSH heartbeat probes and master rebuild calls.
//! - Tunnel forward health checks and auto-restart.
//! - Integration with `crate::ssh::master` and `crate::tunnels::forward`.
//!
//! # `poll_loop`
//!
//! `poll_loop` sleeps `TICK_INTERVAL` (0.5 s) between calls to `run_tick` and
//! exits when `stop` is set to `true`. Use an `Arc<AtomicBool>` shared with
//! the daemon main thread to request a clean shutdown.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use log::{debug, warn};

use crate::engine::change_key::{host_change_key, tunnel_change_key};
use crate::engine::schedule::TICK_INTERVAL;
use crate::engine::State;
use crate::proto::{encode_event, Event};

// ---------------------------------------------------------------------------
// run_tick
// ---------------------------------------------------------------------------

/// Run one poll tick:
/// 1. Read current state, compute change-keys.
/// 2. Emit events for changed stable-keys.
/// 3. Update last_*_keys bookmarks.
///
/// All work is done **under the same brief lock acquisition** (no I/O inside).
/// Future SSH maintenance will be launched off-lock from here.
pub fn run_tick(state: &Mutex<State>) {
    let mut guard = match state.lock() {
        Ok(g) => g,
        Err(e) => {
            warn!("run_tick: state lock poisoned: {e}");
            return;
        }
    };

    // ---- Host change detection -----------------------------------------

    // Collect new keys and changed host names first to avoid borrow conflicts.
    let mut changed_hosts: Vec<(String, String)> = Vec::new(); // (name, new_key)
    for host in &guard.hosts {
        let new_key = host_change_key(host);
        let prev_key = guard.last_host_keys.get(&host.host);
        if prev_key.map(|k| k != &new_key).unwrap_or(true) {
            changed_hosts.push((host.host.clone(), new_key));
        }
    }

    // Emit + update bookmarks for hosts.
    for (host_name, new_key) in changed_hosts {
        // Build the event payload (snapshot of the matching host).
        if let Some(host) = guard.hosts.iter().find(|h| h.host == host_name) {
            // encode_event returns JSON + '\n' — the wire protocol requires the newline.
            let line = encode_event(
                Event::HostStatusChanged.as_str(),
                serde_json::to_value(host).unwrap_or(serde_json::Value::Null),
            );
            guard.emit(line);
        }
        guard.last_host_keys.insert(host_name, new_key);
    }

    // ---- Tunnel change detection ----------------------------------------

    let mut changed_tunnels: Vec<(String, String)> = Vec::new(); // (name, new_key)

    for tunnel in &guard.tunnels {
        let new_key = tunnel_change_key(tunnel);
        let prev_key = guard.last_tunnel_keys.get(&tunnel.name);
        if prev_key.map(|k| k != &new_key).unwrap_or(true) {
            changed_tunnels.push((tunnel.name.clone(), new_key));
        }
    }

    for (tname, new_key) in changed_tunnels {
        if let Some(tunnel) = guard.tunnels.iter().find(|t| t.name == tname) {
            // encode_event returns JSON + '\n' — the wire protocol requires the newline.
            let line = encode_event(
                Event::TunnelStatusChanged.as_str(),
                serde_json::to_value(tunnel).unwrap_or(serde_json::Value::Null),
            );
            guard.emit(line);
        }
        guard.last_tunnel_keys.insert(tname, new_key);
    }

    // ---- Cleanup bookmarks for removed tunnels -------------------------

    let current_names: std::collections::HashSet<&str> =
        guard.tunnels.iter().map(|t| t.name.as_str()).collect();

    let removed: Vec<String> = guard
        .last_tunnel_keys
        .keys()
        .filter(|n| !current_names.contains(n.as_str()))
        .cloned()
        .collect();

    for name in removed {
        guard.last_tunnel_keys.remove(&name);
        // encode_event returns JSON + '\n' — the wire protocol requires the newline.
        let line = encode_event(
            Event::TunnelStatusChanged.as_str(),
            serde_json::json!({ "name": name, "status": "removed" }),
        );
        guard.emit(line);
    }

    // ---- DEFERRED: SSH / tunnel maintenance ----------------------------
    //
    // TODO(integration): Off-lock maintenance pattern:
    //
    //   drop(guard);  // <-- MUST drop lock before any blocking I/O
    //
    //   // 1. Heartbeat probe for each active host.
    //   //    crate::ssh::master::PoolState::active_master_ready() (fast, local socket check)
    //   //    If dead → spawn thread → start_master / try_rotate.
    //
    //   // 2. Tunnel forward health check.
    //   //    crate::tunnels::probe::probe_port_ready(local_port, timeout)
    //   //    If dead and wants_alive → spawn thread → start_forward.
    //
    //   // re-lock here to write results back.
    //
    // The guard is intentionally NOT dropped before function return in this
    // stub so the compiler is happy.  When the above is wired, add `drop(guard)`
    // before the spawned threads.
}

// ---------------------------------------------------------------------------
// poll_loop
// ---------------------------------------------------------------------------

/// Run `run_tick` in a loop, sleeping `TICK_INTERVAL` (0.5 s) between passes.
///
/// Exits cleanly when `stop` is set to `true`.
///
/// ```rust,ignore
/// use std::sync::{Arc, atomic::{AtomicBool, Ordering}, Mutex};
/// use a2fa_core::engine::{State, tick::poll_loop};
///
/// let state = Arc::new(Mutex::new(State::with_tunnels(vec![])));
/// let stop  = Arc::new(AtomicBool::new(false));
/// let s2    = Arc::clone(&state);
/// let stop2 = Arc::clone(&stop);
/// std::thread::spawn(move || poll_loop(&s2, &stop2));
///
/// // ... later:
/// stop.store(true, Ordering::Relaxed);
/// ```
pub fn poll_loop(state: &Arc<Mutex<State>>, stop: &Arc<AtomicBool>) {
    while !stop.load(Ordering::Relaxed) {
        run_tick(state);
        thread::sleep(TICK_INTERVAL);
    }
    debug!("poll_loop: stop flag set — exiting");
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::State;
    use crate::model::{Host, Tunnel, TunnelStatus};
    use crate::proto::Event;
    use std::sync::{mpsc, Mutex};

    fn make_tunnel(name: &str, status: TunnelStatus) -> Tunnel {
        Tunnel {
            name: name.into(),
            local_port: 8888,
            remote_port: 8888,
            jump_candidates: None,
            last_node: None,
            last_user: None,
            auto_start: false,
            post_connect_cmd: None,
            tags: vec![],
            url_path: None,
            wants_alive: false,
            status,
            active_jump: None,
            last_msg: "Ready".into(),
            last_alive_at: 0.0,
            total_uptime_sec: 0.0,
            connect_count: 0,
            fail_count: 0,
        }
    }

    fn make_host(name: &str, ready: bool) -> Host {
        Host {
            host: name.into(),
            status: "Idle".into(),
            active: true,
            is_master_ready: ready,
            pool_index: 0,
            pool_alive: 0,
            is_mounted: false,
            last_msg: "OK".into(),
        }
    }

    /// First tick emits an event for each tunnel (no previous bookmark).
    /// The emitted event must use the lowercase wire name (tunnel_status_changed).
    #[test]
    fn first_tick_emits_tunnel_event() {
        let (tx, rx) = mpsc::channel();
        let mut inner = State::with_tunnels(vec![make_tunnel("nb", TunnelStatus::Idle)]);
        inner.subscribe(tx);
        let state = Mutex::new(inner);

        run_tick(&state);

        let msg = rx.try_recv().expect("expected event on first tick");
        assert!(msg.contains("nb"), "event should mention tunnel name");
        // Wire protocol requires lowercase event name.
        assert!(
            msg.contains(Event::TunnelStatusChanged.as_str()),
            "expected lowercase '{}' in: {msg}",
            Event::TunnelStatusChanged.as_str()
        );
        // Must NOT contain the old UPPERCASE name.
        assert!(
            !msg.contains("TUNNEL_STATUS_CHANGED"),
            "must not emit UPPERCASE event name, got: {msg}"
        );
    }

    /// Second tick with unchanged state emits NO event.
    #[test]
    fn second_tick_no_event_when_unchanged() {
        let (tx, rx) = mpsc::channel();
        let mut inner = State::with_tunnels(vec![make_tunnel("nb", TunnelStatus::Idle)]);
        inner.subscribe(tx);
        let state = Mutex::new(inner);

        run_tick(&state); // first tick — populates bookmarks
        let _ = rx.try_recv(); // discard first event

        run_tick(&state); // second tick — same state, no event
        assert!(rx.try_recv().is_err(), "no event expected on second tick");
    }

    /// Changing total_uptime_sec must NOT emit an event.
    #[test]
    fn uptime_change_does_not_emit_event() {
        let (tx, rx) = mpsc::channel();
        let mut inner = State::with_tunnels(vec![make_tunnel("nb", TunnelStatus::Alive)]);
        inner.subscribe(tx);
        let state = Mutex::new(inner);

        run_tick(&state); // first tick
        let _ = rx.try_recv();

        // Mutate only total_uptime_sec
        state.lock().unwrap().tunnels[0].total_uptime_sec += 5.0;

        run_tick(&state); // second tick
        assert!(rx.try_recv().is_err(), "uptime change must not emit event");
    }

    /// A real status change (Idle → Alive) MUST emit an event.
    #[test]
    fn status_change_emits_event() {
        let (tx, rx) = mpsc::channel();
        let mut inner = State::with_tunnels(vec![make_tunnel("nb", TunnelStatus::Idle)]);
        inner.subscribe(tx);
        let state = Mutex::new(inner);

        run_tick(&state);
        let _ = rx.try_recv();

        state.lock().unwrap().tunnels[0].status = TunnelStatus::Alive;
        run_tick(&state);

        let msg = rx.try_recv().expect("expected event after status change");
        assert!(msg.contains("alive") || msg.contains("Alive") || msg.contains("nb"));
    }

    /// Removing a tunnel emits a "removed" event.
    #[test]
    fn removed_tunnel_emits_removed_event() {
        let (tx, rx) = mpsc::channel();
        let mut inner = State::with_tunnels(vec![make_tunnel("nb", TunnelStatus::Idle)]);
        inner.subscribe(tx);
        let state = Mutex::new(inner);

        run_tick(&state);
        let _ = rx.try_recv();

        // Remove the tunnel
        state.lock().unwrap().tunnels.clear();
        run_tick(&state);

        let msg = rx.try_recv().expect("expected removed event");
        assert!(msg.contains("removed"), "expected 'removed' in event: {msg}");
    }

    /// Host changes are also detected; event name must be lowercase on the wire.
    #[test]
    fn host_status_change_emits_event() {
        let (tx, rx) = mpsc::channel();
        let mut inner = State::with_tunnels(vec![]);
        inner.hosts.push(make_host("k6", false));
        inner.subscribe(tx);
        let state = Mutex::new(inner);

        run_tick(&state);
        let _ = rx.try_recv(); // first-tick event

        state.lock().unwrap().hosts[0].is_master_ready = true;
        run_tick(&state);

        let msg = rx.try_recv().expect("expected host event");
        assert!(
            msg.contains(Event::HostStatusChanged.as_str()),
            "expected lowercase '{}' in: {msg}",
            Event::HostStatusChanged.as_str()
        );
        assert!(
            !msg.contains("HOST_STATUS_CHANGED"),
            "must not emit UPPERCASE event name, got: {msg}"
        );
    }

    /// Emitted JSON must have the shape: {"event":"tunnel_status_changed","data":{...}}\n
    #[test]
    fn emitted_event_json_shape_is_correct() {
        let (tx, rx) = mpsc::channel();
        let mut inner = State::with_tunnels(vec![make_tunnel("myconn", TunnelStatus::Idle)]);
        inner.subscribe(tx);
        let state = Mutex::new(inner);

        run_tick(&state);

        let msg = rx.try_recv().expect("expected event");
        // Wire protocol: must end with newline for framing.
        assert!(msg.ends_with('\n'), "event line must be newline-terminated: {msg:?}");
        let v: serde_json::Value =
            serde_json::from_str(msg.trim_end()).expect("event must be valid JSON");
        assert_eq!(
            v["event"].as_str().unwrap(),
            "tunnel_status_changed",
            "event field must be lowercase: {msg}"
        );
        assert!(v["data"].is_object(), "data field must be an object: {msg}");
    }

    /// last_msg change on a host must NOT emit an event.
    #[test]
    fn host_last_msg_change_does_not_emit_event() {
        let (tx, rx) = mpsc::channel();
        let mut inner = State::with_tunnels(vec![]);
        inner.hosts.push(make_host("k6", true));
        inner.subscribe(tx);
        let state = Mutex::new(inner);

        run_tick(&state);
        let _ = rx.try_recv();

        state.lock().unwrap().hosts[0].last_msg = "cool-down 297s".into();
        run_tick(&state);

        assert!(
            rx.try_recv().is_err(),
            "last_msg change on host must not emit event"
        );
    }

    /// poll_loop exits when stop flag is set.
    #[test]
    fn poll_loop_exits_on_stop() {
        let state = Arc::new(Mutex::new(State::with_tunnels(vec![])));
        let stop = Arc::new(AtomicBool::new(false));

        let s2 = Arc::clone(&state);
        let stop2 = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            stop2.store(true, Ordering::Relaxed); // stop immediately
            poll_loop(&s2, &stop2);
        });

        handle.join().expect("poll_loop thread should exit");
    }
}
