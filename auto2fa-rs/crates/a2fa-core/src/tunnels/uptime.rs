use std::time::{SystemTime, UNIX_EPOCH};

/// Current wall-clock time as fractional Unix seconds.
///
/// Degrades to `0.0` if the system clock is set before 1970-01-01 (unrealistic,
/// but a pre-epoch clock must NOT panic: `now_unix` is called from the
/// maintenance loop and from worker threads that hold the State lock, where a
/// panic would poison it).
pub fn now_unix() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Compute live uptime: `base_sec` plus however long the current alive run
/// has been going (i.e. `now - alive_since`), or just `base_sec` when the
/// tunnel is not currently alive (`alive_since` is `None`).
///
/// Negative current-run durations are clamped to zero (clock skew safety).
pub fn live_uptime(base_sec: f64, alive_since: Option<f64>) -> f64 {
    match alive_since {
        None => base_sec,
        Some(since) => {
            let current_run = (now_unix() - since).max(0.0);
            base_sec + current_run
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_uptime_adds_current_run() {
        // base 100s, alive_since = now-10 => ~110
        let u = live_uptime(100.0, Some(now_unix() - 10.0));
        assert!((109.0..=112.0).contains(&u));
        // not alive => just base
        assert_eq!(live_uptime(100.0, None), 100.0);
    }

    #[test]
    fn zero_base_with_alive_since() {
        let since = now_unix() - 5.0;
        let u = live_uptime(0.0, Some(since));
        assert!((4.9..=6.1).contains(&u));
    }

    #[test]
    fn clamps_negative_current_run() {
        // alive_since in the future (clock skew) → clamped to 0
        let u = live_uptime(50.0, Some(now_unix() + 100.0));
        assert_eq!(u, 50.0);
    }

    #[test]
    fn now_unix_is_reasonable() {
        let t = now_unix();
        // Year 2024 in unix time ≈ 1_700_000_000; year 2100 ≈ 4_100_000_000
        assert!(t > 1_700_000_000.0 && t < 4_100_000_000.0);
    }
}
