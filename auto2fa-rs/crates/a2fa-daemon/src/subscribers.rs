//! Subscriber / event fan-out helpers.
//!
//! Each connected client that sends `subscribe_events` gets an `mpsc::Receiver`
//! that delivers JSON event strings from the engine's `State::emit` path.
//! This module provides the wiring between the per-connection I/O thread and
//! the engine's subscriber list.

use std::io::Write;
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::sync::{mpsc, Arc, Mutex};

use a2fa_core::engine::State;

/// Bound on the per-subscriber event channel. A connected-but-not-reading
/// client backs up at most this many events before `State::emit` drops it,
/// rather than growing the heap without limit.
const SUBSCRIBER_CHANNEL_CAP: usize = 1024;

/// Spawn a thread that reads from `rx` and writes each event string to the
/// shared connection writer.  The thread exits cleanly when the channel closes
/// (sender dropped) or the stream write fails / times out (client disconnected
/// or wedged).
///
/// `writer` is the SAME lock the connection's request loop writes responses
/// through, so events and responses serialize — a write_all here can never
/// interleave bytes with one there. The write timeout was already set on the
/// underlying stream when the connection was accepted.
///
/// Returns immediately; the spawned thread shares `writer` and owns `rx`.
pub fn forward_events(writer: Arc<Mutex<UnixStream>>, rx: mpsc::Receiver<String>) {
    let spawn_res = std::thread::Builder::new()
        .name("subscriber-forward".into())
        .spawn(move || {
            for event_line in rx {
                let mut guard = writer.lock().unwrap_or_else(|e| e.into_inner());
                if guard.write_all(event_line.as_bytes()).is_err() {
                    break;
                }
            }
            // Forwarder done (channel closed by `emit` pruning the sender, OR a
            // write failed): shut the connection down so a client whose
            // subscription was pruned reconnects + re-subscribes (the
            // per-connection subscribe-once flag blocks re-subscribing here).
            let _ = writer
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .shutdown(Shutdown::Both);
        });
    if let Err(e) = spawn_res {
        // Spawn failed (e.g. transient EAGAIN). The closure never ran, so the
        // captured `stream` and `rx` drop here; the registered sender is pruned
        // by `State::emit` on the next event (no leak, no panic).
        log::warn!("could not spawn subscriber-forward thread ({e}); subscriber dropped");
    }
}

/// Register a new subscriber with the shared engine state.
///
/// Creates a bounded channel, stores the `SyncSender` end in
/// `State::subscribers`, and returns the `Receiver` end so the connection
/// handler can forward events to the wire.
///
/// Returns `None` if the engine refused the subscriber (the
/// `MAX_SUBSCRIBERS` cap was reached); in that case no forwarder should be
/// spawned.
pub fn register(state: &Arc<Mutex<State>>) -> Option<mpsc::Receiver<String>> {
    let (tx, rx) = mpsc::sync_channel::<String>(SUBSCRIBER_CHANNEL_CAP);
    // Poison-tolerant: register() runs OUTSIDE the dispatch catch_unwind, so a
    // raw .lock().unwrap() would turn a one-off State poison into a permanent
    // outage of event subscription for every client. lock_state recovers.
    if crate::lock_state(state).subscribe(tx) {
        Some(rx)
    } else {
        None
    }
}
