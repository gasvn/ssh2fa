//! Stable-field change-key computation for state-change event detection.
//!
//! # Design
//!
//! The poll loop compares snapshots every 0.5 s. To avoid firing spurious
//! `TUNNEL_STATUS_CHANGED` events on every tick (because `total_uptime_sec`
//! is recomputed live), we compute a *change key* that **excludes** volatile
//! fields:
//!
//! - `Tunnel`: excludes `total_uptime_sec`, `wants_alive`, `url_path`,
//!   `connect_count`, `fail_count` — only includes the `_TUNNEL_STABLE_FIELDS`
//!   tuple from `daemon.py`.
//! - `Host`: excludes `last_msg` (free-form countdown text changes every tick
//!   during cooldown) — only includes the `_HOST_STABLE_FIELDS` tuple.
//!
//! Returns a `String` (JSON of the stable subset) so `PartialEq` / `Eq` work
//! naturally without needing a heterogeneous tuple.

use crate::model::{Host, Tunnel};

// ---------------------------------------------------------------------------
// Tunnel stable-field key
// ---------------------------------------------------------------------------

/// `_TUNNEL_STABLE_FIELDS` from daemon.py (in order):
/// name, local_port, remote_port, jump_candidates, last_node, last_user,
/// auto_start, post_connect_cmd, tags, active_jump, status, last_msg,
/// last_alive_at
///
/// Explicitly **excludes**: `total_uptime_sec` (live-computed), `wants_alive`,
/// `url_path` (volatile / UI-only), `connect_count`, `fail_count`.
pub fn tunnel_change_key(t: &Tunnel) -> String {
    serde_json::json!({
        "name":             t.name,
        "local_port":       t.local_port,
        "remote_port":      t.remote_port,
        "jump_candidates":  t.jump_candidates,
        "last_node":        t.last_node,
        "last_user":        t.last_user,
        "auto_start":       t.auto_start,
        "post_connect_cmd": t.post_connect_cmd,
        "tags":             t.tags,
        "active_jump":      t.active_jump,
        "status":           t.status,
        "last_msg":         t.last_msg,
        "last_alive_at":    t.last_alive_at,
    })
    .to_string()
}

// ---------------------------------------------------------------------------
// Host stable-field key
// ---------------------------------------------------------------------------

/// `_HOST_STABLE_FIELDS` from daemon.py (in order):
/// host, status, active, is_master_ready, pool_index, pool_alive, is_mounted
///
/// Explicitly **excludes**: `last_msg` (noisy cooldown countdown text).
pub fn host_change_key(h: &Host) -> String {
    serde_json::json!({
        "host":           h.host,
        "status":         h.status,
        "active":         h.active,
        "is_master_ready": h.is_master_ready,
        "pool_index":     h.pool_index,
        "pool_alive":     h.pool_alive,
        "is_mounted":     h.is_mounted,
    })
    .to_string()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::TunnelStatus;

    // ---- helpers ----

    fn sample_tunnel() -> Tunnel {
        Tunnel {
            name: "nb".into(),
            local_port: 8888,
            remote_port: 8888,
            jump_candidates: Some(vec!["k6".into()]),
            last_node: Some("gpunode01".into()),
            last_user: Some("user1".into()),
            direct_host: None,
            auto_start: true,
            post_connect_cmd: None,
            tags: vec!["ml".into()],
            url_path: None,
            wants_alive: true,
            status: TunnelStatus::Alive,
            active_jump: Some("k6".into()),
            last_msg: "Connected".into(),
            last_alive_at: 1_700_000_000.0,
            total_uptime_sec: 42.0,
            connect_count: 3,
            fail_count: 1,
        }
    }

    fn sample_host() -> Host {
        Host {
            host: "k6".into(),
            status: "[green]Pool Active (0)[/green]".into(),
            active: true,
            is_master_ready: true,
            pool_index: 0,
            pool_alive: 2,
            is_mounted: false,
            last_msg: "Ready".into(),
        }
    }

    // ---- regression guards ----

    /// total_uptime_sec changes every tick while a tunnel is alive;
    /// that must NOT fire an event.
    #[test]
    fn tunnel_change_key_ignores_uptime() {
        let a = sample_tunnel();
        let mut b = a.clone();
        b.total_uptime_sec += 5.0; // ONLY uptime changed
        assert_eq!(tunnel_change_key(&a), tunnel_change_key(&b)); // => NO event

        let mut c = a.clone();
        c.status = TunnelStatus::Alive;
        let mut d = a.clone();
        d.status = TunnelStatus::Starting;
        assert_ne!(tunnel_change_key(&c), tunnel_change_key(&d)); // => event
    }

    /// last_msg changes every tick during host cool-down countdown;
    /// that must NOT fire an event.
    #[test]
    fn host_change_key_ignores_last_msg() {
        let a = sample_host();
        let mut b = a.clone();
        b.last_msg = "cool-down 297s".into(); // noisy countdown
        assert_eq!(host_change_key(&a), host_change_key(&b));
    }

    /// A real host-stable-field change (is_master_ready) MUST fire an event.
    #[test]
    fn host_change_key_detects_real_change() {
        let a = sample_host();
        let mut b = a.clone();
        b.is_master_ready = false;
        assert_ne!(host_change_key(&a), host_change_key(&b));
    }

    /// connect_count / fail_count changes must NOT fire an event
    /// (they are excluded from the stable set).
    #[test]
    fn tunnel_change_key_ignores_connect_and_fail_counts() {
        let a = sample_tunnel();
        let mut b = a.clone();
        b.connect_count += 1;
        b.fail_count += 2;
        assert_eq!(tunnel_change_key(&a), tunnel_change_key(&b));
    }

    /// wants_alive changing must NOT fire a change event.
    #[test]
    fn tunnel_change_key_ignores_wants_alive() {
        let a = sample_tunnel();
        let mut b = a.clone();
        b.wants_alive = !b.wants_alive;
        assert_eq!(tunnel_change_key(&a), tunnel_change_key(&b));
    }

    /// A real tunnel-stable-field change (last_node) MUST produce a different key.
    #[test]
    fn tunnel_change_key_detects_node_change() {
        let a = sample_tunnel();
        let mut b = a.clone();
        b.last_node = Some("gpunode02".into());
        assert_ne!(tunnel_change_key(&a), tunnel_change_key(&b));
    }

    /// url_path is excluded from the stable set.
    #[test]
    fn tunnel_change_key_ignores_url_path() {
        let a = sample_tunnel();
        let mut b = a.clone();
        b.url_path = Some("?token=abc".into());
        assert_eq!(tunnel_change_key(&a), tunnel_change_key(&b));
    }
}
