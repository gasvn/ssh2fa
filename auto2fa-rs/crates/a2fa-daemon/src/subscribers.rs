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
use std::time::Duration;

use a2fa_core::engine::State;

/// Bound on the per-subscriber event channel. A connected-but-not-reading
/// client backs up at most this many events before `State::emit` drops it,
/// rather than growing the heap without limit.
const SUBSCRIBER_CHANNEL_CAP: usize = 1024;

/// Write timeout on the subscriber socket. If a stuck client wedges the
/// forwarder's `write_all`, it errors out after this long and the forwarder
/// exits (dropping the receiver → the sender is pruned by `emit`).
const FORWARD_WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// Spawn a thread that reads from `rx` and writes each event string to
/// `stream`.  The thread exits cleanly when the channel closes (sender
/// dropped) or the stream write fails / times out (client disconnected or
/// wedged).
///
/// Returns immediately; the spawned thread owns `stream` and `rx`.
pub fn forward_events(mut stream: UnixStream, rx: mpsc::Receiver<String>) {
    // A stuck client must not pin this thread forever on a blocked write.
    let _ = stream.set_write_timeout(Some(FORWARD_WRITE_TIMEOUT));
    std::thread::spawn(move || {
        for event_line in rx {
            if stream.write_all(event_line.as_bytes()).is_err() {
                break;
            }
        }
        // Best-effort shutdown — ignore errors.
        let _ = stream.shutdown(Shutdown::Both);
    });
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
    if state.lock().unwrap().subscribe(tx) {
        Some(rx)
    } else {
        None
    }
}
