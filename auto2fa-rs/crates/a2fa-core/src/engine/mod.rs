//! Engine — the single authoritative state store for the auto2fa daemon.
//!
//! # Architecture / Lock Rule
//!
//! State lives behind **one** `Mutex<State>`. The rule is:
//!
//! > The mutex MUST be held **only** for fast critical sections (field reads and
//! > writes that complete in microseconds). It MUST **never** be held across
//! > any blocking I/O — ssh spawns, port probes, file writes, or `sleep` calls.
//!
//! The tick loop and IPC handlers follow this pattern:
//! 1. Lock → snapshot the fields they need → **unlock**.
//! 2. Do blocking ssh/tunnel/file work **off-lock** on worker threads.
//! 3. Lock → write results back → **unlock**.
//!
//! This keeps the IPC server responsive while long-running operations (master
//! login, forward setup) are in flight.
//!
//! # Subscriber model
//!
//! Change events are pushed to subscribers via bounded
//! `std::sync::mpsc::SyncSender<String>` channels.  Each subscriber holds the
//! `SyncSender` end; the engine holds a `Vec` of clones.  `emit` uses
//! `try_send` so it NEVER blocks (it runs under the `State` lock): a subscriber
//! whose channel is full (a stuck/non-reading client) is dropped, as are dead
//! (disconnected) senders.  The total number of subscribers is hard-capped by
//! [`MAX_SUBSCRIBERS`].  The `String` payload is a JSON-encoded IPC event (same
//! wire format as `daemon.py`'s `_emit`).

pub mod change_key;
pub mod schedule;
pub mod tick;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{SyncSender, TrySendError};

/// Hard cap on the total number of concurrent event subscribers.
///
/// Guards against client-driven resource exhaustion: each subscriber holds a
/// channel and a forwarder thread, so an attacker (or buggy client) opening
/// many connections must not be able to register unbounded subscribers.
pub const MAX_SUBSCRIBERS: usize = 64;

use crate::config;
use crate::model::{Host, Tunnel};

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// All runtime state owned by the engine.
///
/// Instantiate once and wrap in `Arc<Mutex<State>>` for sharing across threads.
pub struct State {
    /// Snapshot of every managed SSH jump-host.
    pub hosts: Vec<Host>,

    /// Snapshot of every managed tunnel.
    pub tunnels: Vec<Tunnel>,

    /// Active event subscribers. Each entry is a bounded `mpsc::SyncSender`
    /// whose consumer (typically a connected IPC client) receives JSON event
    /// strings. Dead and backed-up (full-channel) senders are pruned inside
    /// [`State::emit`]; the count is capped at [`MAX_SUBSCRIBERS`].
    pub subscribers: Vec<SyncSender<String>>,

    /// Path to `tunnels.json` — needed for save operations.
    pub tunnels_path: PathBuf,

    // ---- Change-detection bookmarks ----------------------------------------
    //
    // These store the last emitted stable-field key per host/tunnel name so the
    // tick loop can compare the current key and only emit when something
    // actually changed.  See `change_key.rs`.

    /// Last emitted tunnel change-key, keyed by tunnel name.
    pub(crate) last_tunnel_keys: HashMap<String, String>,

    /// Last emitted host change-key, keyed by host name.
    pub(crate) last_host_keys: HashMap<String, String>,
}

impl State {
    /// Create a new `State`, loading tunnels from `tunnels_path` and
    /// host metadata from `passwords_path`.
    ///
    /// Hosts are constructed from the passwords.json metadata plus
    /// default runtime values; they will be kept up to date by the engine
    /// tick as the real SSH manager threads report status.
    pub fn new(tunnels_path: PathBuf, passwords_path: &std::path::Path) -> Self {
        let tunnels = config::load_tunnels(&tunnels_path);
        let meta = config::load_meta(passwords_path);

        // Build default Host snapshots from passwords.json metadata.
        let hosts: Vec<Host> = meta
            .iter()
            .map(|(name, m)| Host {
                host: name.clone(),
                status: "Idle".to_owned(),
                active: m.auto_connect,
                is_master_ready: false,
                pool_index: 0,
                pool_alive: 0,
                is_mounted: false,
                last_msg: "Starting".to_owned(),
            })
            .collect();

        Self {
            hosts,
            tunnels,
            subscribers: Vec::new(),
            tunnels_path,
            last_tunnel_keys: HashMap::new(),
            last_host_keys: HashMap::new(),
        }
    }

    /// Convenience constructor for tests and integration helpers: starts with
    /// no hosts and an explicit tunnel list.  Available in all configurations
    /// so downstream crates (e.g. `a2fa-daemon` integration tests) can use it.
    pub fn with_tunnels(tunnels: Vec<Tunnel>) -> Self {
        Self {
            hosts: Vec::new(),
            tunnels,
            subscribers: Vec::new(),
            tunnels_path: PathBuf::from("/dev/null"),
            last_tunnel_keys: HashMap::new(),
            last_host_keys: HashMap::new(),
        }
    }

    /// Push a JSON event string to all live subscribers, pruning dead and
    /// backed-up ones.
    ///
    /// This MUST stay non-blocking: it runs while the `State` mutex is held, so
    /// it uses `try_send` and never a blocking `send`. A subscriber whose
    /// channel is full (a connected-but-not-reading client) is dropped rather
    /// than allowed to stall the caller or grow the heap without bound — the
    /// forwarder thread shuts the socket down once its sender is gone.
    pub fn emit(&mut self, event_json: String) {
        self.subscribers.retain(|tx| match tx.try_send(event_json.clone()) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                log::warn!("dropping subscriber: event channel full (slow/stuck client)");
                false
            }
            Err(TrySendError::Disconnected(_)) => false,
        });
    }

    /// Add a subscriber channel.
    ///
    /// Enforces the [`MAX_SUBSCRIBERS`] cap: if the cap is reached the new
    /// sender is refused (logged) instead of pushed. Returns `true` if the
    /// subscriber was registered.
    ///
    /// We deliberately do NOT probe existing senders for liveness here: the
    /// only non-blocking probe (`try_send`) would inject a bogus payload into
    /// healthy subscribers' streams. Dead/backed-up senders are pruned by
    /// [`State::emit`] on the next event instead.
    pub fn subscribe(&mut self, tx: SyncSender<String>) -> bool {
        if self.subscribers.len() >= MAX_SUBSCRIBERS {
            log::warn!(
                "refusing new subscriber: MAX_SUBSCRIBERS ({}) reached",
                MAX_SUBSCRIBERS
            );
            return false;
        }
        self.subscribers.push(tx);
        true
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::TunnelStatus;
    use std::sync::mpsc;

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

    #[test]
    fn state_with_tunnels_roundtrip() {
        let t = make_tunnel("nb", TunnelStatus::Idle);
        let state = State::with_tunnels(vec![t]);
        assert_eq!(state.tunnels.len(), 1);
        assert_eq!(state.tunnels[0].name, "nb");
    }

    #[test]
    fn emit_reaches_subscriber() {
        let mut state = State::with_tunnels(vec![]);
        let (tx, rx) = mpsc::sync_channel(16);
        assert!(state.subscribe(tx));
        state.emit(r#"{"event":"test"}"#.to_string());
        let msg = rx.try_recv().expect("should have received a message");
        assert!(msg.contains("test"));
    }

    #[test]
    fn emit_prunes_dead_subscriber() {
        let mut state = State::with_tunnels(vec![]);
        let (tx, rx) = mpsc::sync_channel(16);
        assert!(state.subscribe(tx));
        drop(rx); // disconnect the receiver
        state.emit("event".to_string()); // should not panic, just prune
        assert_eq!(state.subscribers.len(), 0);
    }

    #[test]
    fn emit_delivers_to_multiple_subscribers() {
        let mut state = State::with_tunnels(vec![]);
        let (tx1, rx1) = mpsc::sync_channel::<String>(16);
        let (tx2, rx2) = mpsc::sync_channel::<String>(16);
        assert!(state.subscribe(tx1));
        assert!(state.subscribe(tx2));
        state.emit("hello".to_string());
        assert_eq!(rx1.try_recv().unwrap(), "hello");
        assert_eq!(rx2.try_recv().unwrap(), "hello");
    }

    #[test]
    fn emit_drops_full_subscriber_and_does_not_block() {
        // A bounded channel of capacity 1 with a receiver that never reads:
        // after the buffer fills, the next emit must DROP the subscriber rather
        // than block the caller (emit runs under the State lock).
        let mut state = State::with_tunnels(vec![]);
        let (tx, _rx) = mpsc::sync_channel::<String>(1);
        assert!(state.subscribe(tx));

        // First emit fills the single slot (buffered, not yet read).
        state.emit("e1".to_string());
        assert_eq!(state.subscribers.len(), 1);

        // Second emit finds the channel full -> drops the subscriber.
        // If emit blocked instead of try_send'ing, this test would hang.
        state.emit("e2".to_string());
        assert_eq!(state.subscribers.len(), 0, "full subscriber should be dropped");
    }

    #[test]
    fn subscribe_enforces_max_subscribers_cap() {
        let mut state = State::with_tunnels(vec![]);
        // Keep receivers alive so senders stay registered.
        let mut keep = Vec::new();
        for _ in 0..MAX_SUBSCRIBERS {
            let (tx, rx) = mpsc::sync_channel::<String>(1);
            assert!(state.subscribe(tx), "should accept up to the cap");
            keep.push(rx);
        }
        assert_eq!(state.subscribers.len(), MAX_SUBSCRIBERS);

        // One past the cap must be refused.
        let (tx, rx) = mpsc::sync_channel::<String>(1);
        assert!(!state.subscribe(tx), "beyond cap should be refused");
        keep.push(rx);
        assert_eq!(state.subscribers.len(), MAX_SUBSCRIBERS);
    }
}
