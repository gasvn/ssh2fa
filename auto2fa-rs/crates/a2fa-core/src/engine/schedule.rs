//! Timing helpers for heartbeat probing, cooldown, and back-off.
//!
//! These are pure functions of `Instant`/`Duration` with no I/O, so they are
//! trivially unit-testable without mocking.  The engine tick loop calls them
//! to decide whether to probe a master or restart a tunnel.
//!
//! # Mirroring `backend.py` timing constants
//!
//! | Python constant               | Value     | Rust equivalent         |
//! |-------------------------------|-----------|-------------------------|
//! | heartbeat probe interval      | ~5 s      | `PROBE_INTERVAL`        |
//! | cooldown after N OTP failures | 60 s      | (in `ssh::master`)      |
//! | wake-recover retry delays     | 10/20/30/60/120 s | `WAKE_RETRY_DELAYS` |
//! | tick sleep                    | 0.5 s     | `TICK_INTERVAL`         |

use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Time between local `ssh -O check` heartbeat probes for a single host.
pub const PROBE_INTERVAL: Duration = Duration::from_secs(5);

/// Tick sleep duration — matches daemon.py `_state_poll_loop` 0.5s.
pub const TICK_INTERVAL: Duration = Duration::from_millis(500);

/// Back-off retry delays after a wake-recover event (mirrors daemon.py
/// `_delayed_restart` delays: `[10, 20, 30, 60, 120]`).
pub const WAKE_RETRY_DELAYS: &[Duration] = &[
    Duration::from_secs(10),
    Duration::from_secs(20),
    Duration::from_secs(30),
    Duration::from_secs(60),
    Duration::from_secs(120),
];

// ---------------------------------------------------------------------------
// Timing predicates
// ---------------------------------------------------------------------------

/// Returns `true` if enough time has elapsed since `last_probe` to warrant
/// another heartbeat probe, or if `last_probe` is `None` (never probed).
///
/// Uses `backoff` as the interval when `backoff` is larger than
/// `PROBE_INTERVAL` (e.g. after ping-pong detection the engine can pass in
/// the `probe_backoff_until` duration). Normally pass `PROBE_INTERVAL`.
pub fn should_probe(now: Instant, last_probe: Option<Instant>, interval: Duration) -> bool {
    match last_probe {
        None => true,
        Some(t) => now.duration_since(t) >= interval,
    }
}

/// Returns `true` if `now` is before `cooldown_until`, i.e. we are still
/// sitting out a cooldown period.
pub fn in_cooldown(now: Instant, cooldown_until: Option<Instant>) -> bool {
    match cooldown_until {
        None => false,
        Some(t) => now < t,
    }
}

/// Returns `true` if `now` is within the back-off window (the engine should
/// not probe or restart while in backoff).
pub fn in_backoff(now: Instant, backoff_until: Option<Instant>) -> bool {
    in_cooldown(now, backoff_until)
}

/// Compute the remaining duration of a cooldown/backoff, or `Duration::ZERO`
/// if it has expired or was never set.
pub fn remaining(now: Instant, until: Option<Instant>) -> Duration {
    match until {
        None => Duration::ZERO,
        Some(t) => t.saturating_duration_since(now),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn should_probe_when_never_probed() {
        assert!(should_probe(Instant::now(), None, PROBE_INTERVAL));
    }

    #[test]
    fn should_probe_false_when_too_soon() {
        let now = Instant::now();
        // Last probe was just 1 second ago; interval is 5 s → not yet.
        let last = now - Duration::from_secs(1);
        assert!(!should_probe(now, Some(last), PROBE_INTERVAL));
    }

    #[test]
    fn should_probe_true_when_interval_elapsed() {
        let now = Instant::now();
        let last = now - Duration::from_secs(6);
        assert!(should_probe(now, Some(last), PROBE_INTERVAL));
    }

    #[test]
    fn in_cooldown_false_when_none() {
        assert!(!in_cooldown(Instant::now(), None));
    }

    #[test]
    fn in_cooldown_true_when_future() {
        let until = Instant::now() + Duration::from_secs(10);
        assert!(in_cooldown(Instant::now(), Some(until)));
    }

    #[test]
    fn in_cooldown_false_when_past() {
        // Set cooldown_until in the past.
        let until = Instant::now() - Duration::from_secs(1);
        assert!(!in_cooldown(Instant::now(), Some(until)));
    }

    #[test]
    fn remaining_zero_when_expired() {
        let past = Instant::now() - Duration::from_secs(5);
        assert_eq!(remaining(Instant::now(), Some(past)), Duration::ZERO);
    }

    #[test]
    fn remaining_nonzero_when_active() {
        let future = Instant::now() + Duration::from_secs(30);
        let r = remaining(Instant::now(), Some(future));
        // Allow a small delta for test execution time
        assert!(r > Duration::from_secs(25), "remaining={r:?}");
    }

    #[test]
    fn wake_retry_delays_has_five_entries() {
        assert_eq!(WAKE_RETRY_DELAYS.len(), 5);
    }

    #[test]
    fn wake_retry_delays_are_ascending() {
        let d = WAKE_RETRY_DELAYS;
        for i in 1..d.len() {
            assert!(d[i] > d[i - 1], "delays must be ascending: {:?}", d);
        }
    }
}
