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

/// Spawn a thread that reads from `rx` and writes each event string to
/// `stream`.  The thread exits cleanly when the channel closes (sender
/// dropped) or the stream write fails (client disconnected).
///
/// Returns immediately; the spawned thread owns `stream` and `rx`.
pub fn forward_events(mut stream: UnixStream, rx: mpsc::Receiver<String>) {
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
/// Stores the `Sender` end in `State::subscribers`; returns the `Receiver`
/// end so the connection handler can forward events to the wire.
pub fn register(state: &Arc<Mutex<State>>) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel::<String>();
    state.lock().unwrap().subscribe(tx);
    rx
}
