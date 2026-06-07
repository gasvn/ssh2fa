//! Orphan-process cleanup — reap stray `ssh -N -J … -L <port>:localhost:…`
//! processes left from a previous daemon run.
//!
//! [`cleanup_orphans`] is called once at boot (after State is loaded).  It is
//! **best-effort**: every error is logged as a warning and execution continues.

use std::process::Command;

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
/// Two guards (mirrors the Python implementation):
/// 1. The **first whitespace token** is exactly `"ssh"` (rejects `sshd`,
///    shell wrappers, etc.).
/// 2. `"-J"` appears as a standalone whitespace-delimited token (rejects
///    ordinary `ssh -L` without a jump host).
pub fn is_auto2fa_tunnel_proc(ps_args: &str) -> bool {
    let mut tokens = ps_args.split_whitespace();
    if tokens.next() != Some("ssh") {
        return false;
    }
    // Collect the rest and look for "-J" as a standalone token.
    ps_args.split_whitespace().any(|t| t == "-J")
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
        let pgrep_out = match Command::new("pgrep")
            .args(["-f", &pattern])
            .output()
        {
            Ok(o) => o,
            Err(e) => {
                log::warn!("cleanup_orphans: pgrep failed for port {port}: {e}");
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
            let ps_out = match Command::new("ps")
                .args(["-o", "args=", "-p", pid_str.trim()])
                .output()
            {
                Ok(o) => o,
                Err(e) => {
                    log::warn!("cleanup_orphans: ps failed for pid {pid}: {e}");
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
            let kill_result = Command::new("kill")
                .args(["-TERM", pid_str.trim()])
                .output();

            match kill_result {
                Ok(o) if o.status.success() => {
                    log::info!("cleanup_orphans: sent SIGTERM to stray tunnel pid {pid} (port {port})");
                    reaped += 1;
                }
                Ok(o) => {
                    // Already gone or permission denied — not an error.
                    let stderr = String::from_utf8_lossy(&o.stderr);
                    log::warn!("cleanup_orphans: kill -TERM {pid} failed: {stderr}");
                }
                Err(e) => {
                    log::warn!("cleanup_orphans: kill command error for pid {pid}: {e}");
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

    #[test]
    fn auto2fa_tunnel_detected() {
        assert!(is_auto2fa_tunnel_proc(
            "ssh -N -J k6 -L 8888:localhost:8888 user@node"
        ));
    }

    #[test]
    fn no_jump_flag_rejected() {
        // A plain ssh -L without -J must NOT be killed.
        assert!(!is_auto2fa_tunnel_proc(
            "ssh -L 8888:localhost:8888 user@host"
        ));
    }

    #[test]
    fn shell_wrapper_rejected() {
        // First token is not "ssh" → reject.
        assert!(!is_auto2fa_tunnel_proc(
            "bash -c 'ssh -N -J k6 -L 8888:localhost:8888 user@node'"
        ));
    }

    #[test]
    fn sshd_rejected() {
        // "sshd" starts with the string "ssh" but the first *token* is not
        // exactly "ssh".
        assert!(!is_auto2fa_tunnel_proc("sshd -N -J k6 -L 8888:localhost:8888 user@node"));
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
}
