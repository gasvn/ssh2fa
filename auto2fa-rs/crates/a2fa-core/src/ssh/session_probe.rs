//! Detecting a master that is "alive" but cannot carry a session.
//!
//! # The gap this closes
//!
//! `master_probe` (unix-socket connect) and `ssh -O check` both answer from the
//! multiplexer process. They confirm the master is RUNNING; they say nothing
//! about whether the far end will accept a new session. ssh's own keepalive
//! covers the network-dead case (see `MASTER_DEAD_DETECT_SECS`), but not this
//! one: when an HPC login node refuses new sessions — session limits, PAM
//! limits, a degraded node — the transport stays healthy, keepalives keep
//! flowing, `ssh -O check` keeps saying "Master running", and every session
//! request fails.
//!
//! Observed live: a host reported Connected with `ssh -O check` → "Master
//! running (pid=…)", while a real command over the same master exited 254 with
//! `mux_client_request_session` as its last word. The app said connected; the
//! user could not connect.
//!
//! The only way to know is to ask for a session. That is expensive enough that
//! WHEN to ask is a separate decision — see [`ProbeSchedule`].

use std::time::Duration;

use crate::ssh::failure::{actionable_failure, failure_reason};
use crate::sys::run_cmd_bounded;

/// Hard deadline for the probe command.
///
/// Measured against real cluster login nodes, a healthy warm master answered in
/// 4-16 s under load, so this is deliberately generous: a probe that times out
/// declares a WORKING master dead and costs the user a fresh 2FA login. It is
/// still a hard bound — the probe must never hang a caller.
pub const SESSION_PROBE_TIMEOUT: Duration = Duration::from_secs(25);

/// Ask the master for a real session and see whether it is granted.
///
/// Runs `true` — no output, no side effects on the remote host. Uses the
/// existing ControlPath with `ControlMaster=no` so it can only ever ride the
/// master being tested, and `BatchMode=yes` so it can never block on a prompt.
///
/// MUST NOT be called from the heartbeat thread: it can take tens of seconds.
/// Callers run it on a worker.
pub fn session_works(control_path: &std::path::Path, host: &str) -> bool {
    session_failure(control_path, host).is_none()
}

/// Ask the master for a real session and return an actionable explanation on
/// failure. The old boolean API proved that the session failed, then discarded
/// the server's stderr (`MaxSessions`, PAM denial, account policy, etc.)—the
/// exact information the user needs to fix the problem.
pub fn session_failure(control_path: &std::path::Path, host: &str) -> Option<String> {
    let cp = control_path.to_string_lossy().into_owned();
    let mut args: Vec<String> = crate::config::paths::managed_config_args();
    args.extend([
        "-o".into(), format!("ControlPath={cp}"),
        "-o".into(), "ControlMaster=no".into(),
        // If the mux disappears between scheduling and exec, OpenSSH normally
        // falls back to a fresh TCP connection. A user with a working SSH key
        // could then make this probe pass even though the master under test is
        // unusable. Existing-master reuse bypasses ProxyCommand; fallback must
        // hit /usr/bin/false and fail, so success proves THIS ControlPath ran
        // the remote command.
        "-o".into(), "ProxyCommand=/usr/bin/false".into(),
        "-o".into(), "BatchMode=yes".into(),
        "-o".into(), "LogLevel=ERROR".into(),
        "-o".into(), "ConnectTimeout=10".into(),
        host.into(),
        "true".into(),
    ]);
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    match run_cmd_bounded("ssh", &arg_refs, SESSION_PROBE_TIMEOUT) {
        Some(out) if out.status.success() => None,
        Some(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let stdout = String::from_utf8_lossy(&out.stdout);
            let transcript = if stderr.trim().is_empty() { &stdout } else { &stderr };
            Some(actionable_failure(&failure_reason(transcript)))
        }
        // Timed out or could not spawn — treat as "no answer", NOT as proof of
        // death. `ProbeSchedule` requires repeated failures before acting.
        None => Some(actionable_failure("SSH session probe timed out")),
    }
}

/// When to probe, and when a run of failures justifies rebuilding.
///
/// Kept as pure data so the policy is unit-tested without any ssh.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbeSchedule {
    /// Minimum gap between probes for one host.
    ///
    /// A session probe consumes a session slot on the far end — on a node that
    /// limits sessions, probing aggressively would help cause the failure it is
    /// looking for. Rare is the point.
    pub interval: Duration,
    /// Consecutive failures before the master is declared unusable.
    ///
    /// Never 1: a single failure is far more likely to be a slow login node
    /// than a dead master, and acting on it costs a needless 2FA login.
    pub failures_before_rebuild: u32,
}

impl Default for ProbeSchedule {
    fn default() -> Self {
        ProbeSchedule {
            interval: Duration::from_secs(240),
            failures_before_rebuild: 2,
        }
    }
}

impl ProbeSchedule {
    /// Is this host due for a probe?
    ///
    /// `since_last` is time since the previous probe; `None` means never
    /// probed, which is due immediately — a master adopted at daemon boot has
    /// never been proven to carry a session, and that is exactly the case that
    /// produced the live failure.
    pub fn is_due(&self, since_last: Option<Duration>) -> bool {
        match since_last {
            None => true,
            Some(d) => d >= self.interval,
        }
    }

    /// Should a master with this many consecutive probe failures be rebuilt?
    pub fn should_rebuild(&self, consecutive_failures: u32) -> bool {
        consecutive_failures >= self.failures_before_rebuild
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_never_probed_master_is_due_immediately() {
        // The adopted-at-boot case: never proven to carry a session.
        assert!(ProbeSchedule::default().is_due(None));
    }

    #[test]
    fn probing_respects_the_interval() {
        let s = ProbeSchedule::default();
        assert!(!s.is_due(Some(Duration::from_secs(10))));
        assert!(!s.is_due(Some(s.interval - Duration::from_secs(1))));
        assert!(s.is_due(Some(s.interval)));
        assert!(s.is_due(Some(s.interval + Duration::from_secs(600))));
    }

    /// One failure must NOT rebuild: a slow login node is far likelier than a
    /// dead master, and a needless rebuild costs the user a fresh 2FA login.
    #[test]
    fn a_single_failure_never_rebuilds() {
        let s = ProbeSchedule::default();
        assert!(!s.should_rebuild(0));
        assert!(!s.should_rebuild(1));
        assert!(s.should_rebuild(2));
        assert!(s.should_rebuild(9));
    }

    /// The interval must stay far above the probe timeout, or probes could
    /// overlap and stack session requests on a struggling node.
    #[test]
    fn the_interval_leaves_room_for_a_slow_probe() {
        let s = ProbeSchedule::default();
        assert!(
            s.interval >= SESSION_PROBE_TIMEOUT * 4,
            "interval {:?} too close to the {:?} timeout",
            s.interval,
            SESSION_PROBE_TIMEOUT
        );
    }

    /// The timeout must tolerate a genuinely slow-but-healthy node. Measured
    /// 4-16s on real login nodes under load.
    #[test]
    fn the_timeout_tolerates_a_slow_healthy_node() {
        assert!(SESSION_PROBE_TIMEOUT >= Duration::from_secs(20));
    }
}
