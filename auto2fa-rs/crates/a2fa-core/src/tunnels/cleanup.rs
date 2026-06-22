//! Orphan-process cleanup — reap stray `ssh -N -J … -L <port>:localhost:…`
//! processes left from a previous daemon run.
//!
//! [`cleanup_orphans`] is called once at boot (after State is loaded).  It is
//! **best-effort**: every error is logged as a warning and execution continues.

use std::time::Duration;

use crate::sys::run_cmd_bounded;

// ---------------------------------------------------------------------------
// Public helpers (also exposed for unit-testing)
// ---------------------------------------------------------------------------

/// Build the pgrep `-f` pattern for a given local port.
///
/// Matches the argv shape that [`crate::tunnels::forward::build_forward_argv`]
/// produces:  `ssh -N -J <jump> -L <lp>:localhost:<rp> user@node`
///
/// The pattern is anchored to `-N` (always the first arg) and the unique
/// `-L <port>:localhost:` fragment.  That combination is specific enough to
/// distinguish auto2fa tunnels from unrelated user ssh sessions.
pub fn orphan_pattern(port: u16) -> String {
    format!("-N.*-L {port}:localhost:")
}

/// Return `true` iff `ps_args` looks like an auto2fa-spawned ssh tunnel.
///
/// Three guards (stricter than the Python implementation):
/// 1. The **first whitespace token** is exactly `"ssh"` (rejects `sshd`,
///    shell wrappers, etc.).
/// 2. `"-J"` appears as a standalone whitespace-delimited token (rejects
///    ordinary `ssh -L` without a jump host).
/// 3. The `ExitOnForwardFailure=yes` option token is present — it is always
///    emitted by `build_forward_argv` and essentially never typed by hand, so
///    a user's own `ssh -N -J … -L …` session on a colliding port is NOT
///    killed (guards 1+2 alone would match it).
pub fn is_auto2fa_tunnel_proc(ps_args: &str) -> bool {
    let mut tokens = ps_args.split_whitespace();
    if tokens.next() != Some("ssh") {
        return false;
    }
    let has_jump = ps_args.split_whitespace().any(|t| t == "-J");
    let has_fingerprint = ps_args
        .split_whitespace()
        .any(|t| t == "ExitOnForwardFailure=yes");
    has_jump && has_fingerprint
}

// ---------------------------------------------------------------------------
// Main entry point
// ---------------------------------------------------------------------------

/// Reap stray auto2fa tunnel processes left from a previous daemon run.
///
/// For each `local_port`:
/// 1. Run `pgrep -f <pattern>` (timeout ~2 s via a short-circuiting read).
/// 2. For each PID returned, confirm via `ps -o args= -p <pid>` that it is an
///    auto2fa-style process (`is_auto2fa_tunnel_proc`).
/// 3. If confirmed, send SIGTERM via `kill -TERM <pid>`.
///
/// All errors are swallowed after a `warn!` log — this is best-effort.
pub fn cleanup_orphans(local_ports: &[u16]) -> usize {
    let mut reaped = 0usize;

    for &port in local_ports {
        let pattern = orphan_pattern(port);

        // --- pgrep -----------------------------------------------------------
        // Bounded: a wedged pgrep must never pin the boot thread (this runs
        // before the accept loop; a stall here would wedge the whole daemon).
        // "--" is REQUIRED: the pattern starts with "-N", and without the
        // option terminator BSD pgrep parses it as flags ("illegal option --
        // N", exit 2) — which the exit-code check below silently treated as
        // "no matches", making orphan reaping a total no-op.
        let pgrep_out = match run_cmd_bounded("pgrep", &["-f", "--", &pattern], Duration::from_secs(2)) {
            Some(o) => o,
            None => {
                log::warn!("cleanup_orphans: pgrep timed out / failed for port {port}");
                continue;
            }
        };

        if pgrep_out.status.code() != Some(0) {
            // Non-zero → no matches (exit 1) or error (>1).  Either way, skip.
            continue;
        }

        let stdout = match std::str::from_utf8(&pgrep_out.stdout) {
            Ok(s) => s,
            Err(_) => continue,
        };

        for pid_str in stdout.split_whitespace() {
            let pid: u32 = match pid_str.trim().parse() {
                Ok(p) => p,
                Err(_) => continue,
            };

            // --- ps confirmation --------------------------------------------
            let ps_out = match run_cmd_bounded(
                "ps",
                &["-o", "args=", "-p", pid_str.trim()],
                Duration::from_secs(2),
            ) {
                Some(o) => o,
                None => {
                    log::warn!("cleanup_orphans: ps timed out / failed for pid {pid}");
                    continue;
                }
            };

            if ps_out.status.code() != Some(0) {
                // Process already gone.
                continue;
            }

            let args = match std::str::from_utf8(&ps_out.stdout) {
                Ok(s) => s.trim(),
                Err(_) => continue,
            };

            if !is_auto2fa_tunnel_proc(args) {
                continue;
            }

            // --- SIGTERM -----------------------------------------------------
            let kill_result = run_cmd_bounded("kill", &["-TERM", pid_str.trim()], Duration::from_secs(1));

            match kill_result {
                Some(o) if o.status.success() => {
                    log::info!("cleanup_orphans: sent SIGTERM to stray tunnel pid {pid} (port {port})");
                    reaped += 1;
                }
                Some(o) => {
                    // Already gone or permission denied — not an error.
                    let stderr = String::from_utf8_lossy(&o.stderr);
                    log::warn!("cleanup_orphans: kill -TERM {pid} failed: {stderr}");
                }
                None => {
                    log::warn!("cleanup_orphans: kill -TERM {pid} timed out / failed");
                }
            }
        }
    }

    reaped
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ----- is_auto2fa_tunnel_proc -------------------------------------------

    /// The exact argv shape `build_forward_argv` produces (incl. the
    /// ExitOnForwardFailure fingerprint) must be detected.
    #[test]
    fn auto2fa_tunnel_detected() {
        assert!(is_auto2fa_tunnel_proc(
            "ssh -N -J k6 -L 8888:localhost:8888 -o StrictHostKeyChecking=no \
             -o UserKnownHostsFile=/dev/null -o ExitOnForwardFailure=yes \
             -o ServerAliveInterval=15 user@node"
        ));
    }

    /// A hand-typed `ssh -N -J … -L …` (no ExitOnForwardFailure) on a
    /// colliding port belongs to the user — it must NOT be killed.
    #[test]
    fn hand_typed_jump_tunnel_rejected() {
        assert!(!is_auto2fa_tunnel_proc(
            "ssh -N -J k6 -L 8888:localhost:8888 user@node"
        ));
    }

    #[test]
    fn no_jump_flag_rejected() {
        // A plain ssh -L without -J must NOT be killed.
        assert!(!is_auto2fa_tunnel_proc(
            "ssh -L 8888:localhost:8888 -o ExitOnForwardFailure=yes user@host"
        ));
    }

    #[test]
    fn shell_wrapper_rejected() {
        // First token is not "ssh" → reject.
        assert!(!is_auto2fa_tunnel_proc(
            "bash -c 'ssh -N -J k6 -L 8888:localhost:8888 -o ExitOnForwardFailure=yes user@node'"
        ));
    }

    #[test]
    fn sshd_rejected() {
        // "sshd" starts with the string "ssh" but the first *token* is not
        // exactly "ssh".
        assert!(!is_auto2fa_tunnel_proc(
            "sshd -N -J k6 -L 8888:localhost:8888 -o ExitOnForwardFailure=yes user@node"
        ));
    }

    #[test]
    fn empty_string_rejected() {
        assert!(!is_auto2fa_tunnel_proc(""));
    }

    // ----- orphan_pattern ---------------------------------------------------

    #[test]
    fn pattern_contains_port_and_prefix() {
        let p = orphan_pattern(8888);
        assert!(p.contains("8888:localhost:"), "pattern missing port fragment: {p}");
        assert!(p.contains("-N"), "pattern missing -N anchor: {p}");
    }

    #[test]
    fn pattern_is_port_specific() {
        let p1 = orphan_pattern(8888);
        let p2 = orphan_pattern(9999);
        assert_ne!(p1, p2);
        assert!(p2.contains("9999:localhost:"));
    }

    /// REGRESSION: the pattern starts with "-N"; without the "--" option
    /// terminator BSD pgrep parses it as flags and exits 2 ("illegal option"),
    /// which the exit-code check silently treated as "no matches" — orphan
    /// reaping was a total no-op. Run the real pgrep exactly like
    /// `cleanup_orphans` does and assert it does NOT die with a usage error
    /// (exit 1 = clean no-match is fine; exit 0 = matched something is fine).
    #[test]
    fn pgrep_accepts_leading_dash_pattern() {
        let pattern = orphan_pattern(1); // port 1: guaranteed no real match
        let out = run_cmd_bounded("pgrep", &["-f", "--", &pattern], Duration::from_secs(5))
            .expect("pgrep must run");
        let code = out.status.code();
        assert!(
            code == Some(0) || code == Some(1),
            "pgrep must parse the dash-leading pattern (exit 0/1), got {code:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
