//! Process-level system resource configuration.

use log::{info, warn};

/// Target soft limit for open file descriptors.
///
/// launchd starts user agents with a soft `RLIMIT_NOFILE` of only **256**,
/// which is far too low for a long-lived daemon that simultaneously holds an
/// IPC listener, up to `MAX_CONNS` accepted client connections, per-host SSH
/// ControlMaster sockets, pty master/slave fds during each login, retained
/// `ssh -L` / `ssh -N` tunnel child pipes, and the event channel. Under load
/// (or a transient fd spike during reconnect churn) 256 is exhausted, after
/// which **every** subsequent `spawn ssh` fails with "Too many open files" /
/// "dup of fd failed" — which then drives an endless restart + credential-read
/// storm (the failure observed in production).
///
/// We raise the SOFT limit toward this target, capped at the inherited HARD
/// limit (we never exceed it — that would need privileges) and at what the
/// kernel will actually grant (`setrlimit` is retried at smaller values).
const TARGET_NOFILE: u64 = 8192;

/// Candidate soft limits, tried high-to-low. macOS clamps `RLIMIT_NOFILE` to
/// `kern.maxfilesperproc`, so `setrlimit` to a large value can fail; we fall
/// back to progressively smaller targets so we always end up at the highest
/// value the kernel will grant.
const CANDIDATES: [u64; 4] = [TARGET_NOFILE, 4096, 2048, 1024];

/// Raise the process's soft `RLIMIT_NOFILE` toward [`TARGET_NOFILE`], capped at
/// the hard limit and at what the kernel grants. Returns `(old_soft, new_soft)`
/// on a successful raise, or `None` if the limit was already adequate or could
/// not be changed.
///
/// Best-effort and idempotent: any failure is logged and ignored (the daemon
/// still runs, just with the inherited limit). Call once at startup.
pub fn raise_fd_limit() -> Option<(u64, u64)> {
    // SAFETY: `get`/`setrlimit` are called with a valid resource id and a
    // properly-initialised `rlimit` struct; standard libc calls, no aliasing.
    unsafe {
        let mut lim = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) != 0 {
            warn!("getrlimit(RLIMIT_NOFILE) failed; keeping inherited fd limit");
            return None;
        }
        let old_soft = lim.rlim_cur as u64;
        let hard = lim.rlim_max as u64;
        let infinity = libc::RLIM_INFINITY as u64;

        // The most we may ask for: capped at the hard limit unless it's
        // unlimited.
        let ceiling = if hard == infinity { TARGET_NOFILE } else { hard };

        for &cand in CANDIDATES.iter() {
            let desired = cand.min(ceiling);
            if desired <= old_soft {
                continue; // already at least this high — nothing to gain
            }
            lim.rlim_cur = desired as libc::rlim_t;
            if libc::setrlimit(libc::RLIMIT_NOFILE, &lim) == 0 {
                info!("raised RLIMIT_NOFILE soft limit {old_soft} -> {desired} (hard {hard})");
                return Some((old_soft, desired));
            }
        }

        if old_soft < TARGET_NOFILE {
            warn!(
                "could not raise RLIMIT_NOFILE above soft={old_soft} (hard={hard}); \
                 keeping inherited limit"
            );
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn current_soft() -> u64 {
        // SAFETY: getrlimit with a valid resource id and init'd struct.
        unsafe {
            let mut lim = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
            assert_eq!(libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim), 0);
            lim.rlim_cur as u64
        }
    }

    #[test]
    fn raise_fd_limit_never_lowers_and_reports_truthfully() {
        let before = current_soft();
        let result = raise_fd_limit();
        let after = current_soft();

        // The soft limit must never go DOWN as a result of calling this.
        assert!(
            after >= before,
            "fd limit must not decrease: before={before} after={after}"
        );

        match result {
            Some((old, new)) => {
                // A reported raise must be a genuine increase, and the live
                // limit must reflect at least the new value.
                assert_eq!(old, before, "reported old soft must match pre-call value");
                assert!(new > old, "reported raise must be a real increase");
                assert!(
                    after >= new,
                    "live soft limit {after} must be >= reported new {new}"
                );
            }
            None => {
                // No raise reported → limit unchanged.
                assert_eq!(after, before, "no-op call must leave the limit unchanged");
            }
        }
    }

    #[test]
    fn raise_fd_limit_is_idempotent() {
        let _ = raise_fd_limit();
        let after_first = current_soft();
        // Second call should be a no-op (already adequate) and never lower it.
        let second = raise_fd_limit();
        let after_second = current_soft();
        assert!(after_second >= after_first);
        if after_first >= TARGET_NOFILE {
            assert!(
                second.is_none(),
                "once at/above target, a repeat call must report no raise"
            );
        }
    }
}
