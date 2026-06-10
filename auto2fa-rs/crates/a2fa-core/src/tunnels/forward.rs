use std::process::{Child, Command};
use std::time::Duration;

use crate::error::{Error, Result};
use crate::tunnels::probe::probe_port_ready;

/// SSH options we always pass — mirroring tunnels.py `start()`.
const SSH_OPTS: &[(&str, &str)] = &[
    ("StrictHostKeyChecking", "no"),
    ("UserKnownHostsFile", "/dev/null"),
    ("ExitOnForwardFailure", "yes"),
    ("ServerAliveInterval", "15"),
];

/// Build the argument list for the `ssh -N -J …` tunnel command.
///
/// This is a pure function — no I/O — so it is fully unit-testable without
/// a real cluster.
///
/// # Arguments
/// - `jump`       — jump host (e.g. `"rc.fas.harvard.edu"`)
/// - `user`       — UNIX user on the compute node (e.g. `"jdoe"`)
/// - `node`       — compute node (e.g. `"holygpu01"`)
/// - `local_port` — local port to bind
/// - `remote_port`— remote port on the node
///
/// Returns the argument *list* (does **not** include `"ssh"` itself).
pub fn build_forward_argv(
    jump: &str,
    user: &str,
    node: &str,
    local_port: u16,
    remote_port: u16,
) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();

    args.push("-N".into());
    args.push("-J".into());
    args.push(jump.to_string());
    args.push("-L".into());
    args.push(format!("{local_port}:localhost:{remote_port}"));

    for (key, val) in SSH_OPTS {
        args.push("-o".into());
        args.push(format!("{key}={val}"));
    }

    args.push(format!("{user}@{node}"));
    args
}

/// Spawn the `ssh -N -J …` port-forward process.
///
/// Returns the child handle on success, or `Err(Error::Internal(_))` if the
/// process could not be spawned (e.g. `ssh` not in PATH, OS resource limit).
///
/// **Note:** spawning succeeds even when the tunnel itself hasn't finished
/// negotiating.  Call [`probe_and_settle`] after this to wait for the port
/// to become ready.
pub fn start_forward(
    jump: &str,
    user: &str,
    node: &str,
    local_port: u16,
    remote_port: u16,
) -> Result<Child> {
    // A leading "-" in any of these would be parsed by ssh as an OPTION
    // (e.g. a node named "-oProxyCommand=…" runs a local command). No shell
    // is involved so spaces/semicolons are inert, but argument injection
    // isn't — reject outright.
    for (label, v) in [("jump", jump), ("user", user), ("node", node)] {
        if v.starts_with('-') {
            return Err(Error::BadParams(format!(
                "invalid {label} '{v}': must not start with '-'"
            )));
        }
    }
    let argv = build_forward_argv(jump, user, node, local_port, remote_port);
    // The retained `ssh -N` child is long-lived and nobody ever drains its
    // output pipes.  If stderr (or stdout) were PIPED, a chatty ssh could fill
    // the ~64KB kernel pipe buffer and block on write — silently stalling the
    // forward while `try_wait` still reports the child alive (health check
    // fooled).  Discard all output to /dev/null so the kernel drops it and the
    // child can never block on I/O.  Short-lived probe/login paths that need
    // ssh's stderr for diagnostics (see tunnels/discovery.rs,
    // tunnels/post_connect.rs) use their own Command invocations and are
    // unaffected.
    Command::new("ssh")
        .args(&argv)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| Error::Internal(format!("ssh spawn failed: {e}")))
}

/// Outcome of [`probe_and_settle`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// Port became ready AND the ssh child is still running — tunnel alive.
    Ready,
    /// Timeout elapsed without a successful connect; child terminated.
    TimedOut,
    /// The port answered but the ssh child had already EXITED — the listener
    /// belongs to some OTHER process (local port collision: with
    /// `ExitOnForwardFailure=yes` ssh dies instantly when the bind fails,
    /// while the foreign listener answers the probe). Marking this "alive"
    /// would route user traffic to the wrong process and flap forever.
    ChildExited,
}

/// Wait for `local_port` to become connectable, then decide the tunnel status.
///
/// Wraps [`probe_port_ready`] so that **any** error (including panics unwound
/// through `catch_unwind`) terminates the child process — matching the
/// Python fix: "on probe EXCEPTION also terminate the child".
///
/// Returns:
/// - `Ok((ProbeOutcome::Ready, child))`       — tunnel is **alive**.
/// - `Ok((ProbeOutcome::TimedOut, child))`    — child terminated before returning.
/// - `Ok((ProbeOutcome::ChildExited, child))` — port collision; child reaped.
/// - `Err(_)` — probe itself raised an error; child is terminated.
pub fn probe_and_settle(
    mut child: Child,
    local_port: u16,
    timeout: Duration,
) -> Result<(ProbeOutcome, Child)> {
    // Use catch_unwind so a panic inside probe_port_ready still tears down
    // the child (the explicit safety guarantee stated in the task spec).
    let probe_result = std::panic::catch_unwind(|| probe_port_ready(local_port, timeout));

    match probe_result {
        Ok(true) => {
            // The port answers — but is it OUR ssh listening, or a foreign
            // process that was already bound? With ExitOnForwardFailure=yes a
            // bind failure kills ssh almost instantly, so a dead child here
            // means the probe connected to someone else's listener.
            match child.try_wait() {
                Ok(Some(status)) => {
                    log::warn!(
                        "probe: port {local_port} answers but ssh child already exited \
                         ({status}) — local port in use by another process"
                    );
                    Ok((ProbeOutcome::ChildExited, child))
                }
                // Still running (or try_wait errored — assume alive; the
                // maintenance child-liveness check backstops).
                _ => Ok((ProbeOutcome::Ready, child)),
            }
        }
        Ok(false) => {
            // Timeout: kill the ssh child.
            let _ = child.kill();
            let _ = child.wait();
            Ok((ProbeOutcome::TimedOut, child))
        }
        Err(panic_payload) => {
            // Probe panicked: still kill the child, then re-surface as Error.
            let _ = child.kill();
            let _ = child.wait();
            let msg = if let Some(s) = panic_payload.downcast_ref::<&str>() {
                s.to_string()
            } else if let Some(s) = panic_payload.downcast_ref::<String>() {
                s.clone()
            } else {
                "unknown panic in probe".to_string()
            };
            Err(Error::Internal(format!("probe panicked: {msg}")))
        }
    }
}

/// Forcibly stop a running forward child.
///
/// Sends SIGKILL (on Unix) and reaps the process.  Errors are silently
/// ignored — the child may have already exited.
pub fn stop_forward(mut child: Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_argv(
        jump: &str,
        user: &str,
        node: &str,
        local: u16,
        remote: u16,
    ) -> Vec<String> {
        build_forward_argv(jump, user, node, local, remote)
    }

    #[test]
    fn argv_contains_no_forward_flag() {
        let argv = make_argv("rc.fas.harvard.edu", "jdoe", "holygpu01", 8080, 8888);
        assert!(argv.contains(&"-N".to_string()), "must contain -N");
    }

    #[test]
    fn argv_contains_jump_flag() {
        let argv = make_argv("rc.fas.harvard.edu", "jdoe", "holygpu01", 8080, 8888);
        let j_idx = argv.iter().position(|a| a == "-J").expect("-J missing");
        assert_eq!(argv[j_idx + 1], "rc.fas.harvard.edu");
    }

    #[test]
    fn argv_contains_local_forward_spec() {
        let argv = make_argv("jump.host", "alice", "node01", 7777, 9999);
        assert!(
            argv.contains(&"7777:localhost:9999".to_string()),
            "argv must contain '7777:localhost:9999': {argv:?}"
        );
    }

    #[test]
    fn argv_contains_user_at_node() {
        let argv = make_argv("jump.host", "bob", "compute42", 5000, 5000);
        assert!(
            argv.contains(&"bob@compute42".to_string()),
            "argv must contain 'bob@compute42': {argv:?}"
        );
    }

    #[test]
    fn argv_has_exit_on_forward_failure() {
        let argv = make_argv("j", "u", "n", 1024, 1025);
        let opts_str: Vec<_> = argv
            .iter()
            .filter(|a| a.contains("ExitOnForwardFailure"))
            .collect();
        assert!(
            !opts_str.is_empty(),
            "argv must contain ExitOnForwardFailure option"
        );
    }

    #[test]
    fn argv_has_strict_host_checking_no() {
        let argv = make_argv("j", "u", "n", 1024, 1025);
        assert!(
            argv.iter().any(|a| a.contains("StrictHostKeyChecking=no")),
            "missing StrictHostKeyChecking=no"
        );
    }

    #[test]
    fn argv_structure_order() {
        // Ensure -N is first, -J is before -L, and user@node is last.
        let argv = make_argv("jump", "user", "node", 8080, 8080);
        assert_eq!(argv[0], "-N");
        let j_pos = argv.iter().position(|a| a == "-J").unwrap();
        let l_pos = argv.iter().position(|a| a == "-L").unwrap();
        assert!(j_pos < l_pos, "-J must come before -L");
        assert!(argv.last().unwrap().contains('@'), "last arg must be user@node");
    }

    /// A leading "-" in jump/user/node would be parsed by ssh as an option
    /// (argument injection, e.g. node "-oProxyCommand=…") — must be rejected
    /// before any spawn.
    #[test]
    fn start_forward_rejects_leading_dash() {
        assert!(start_forward("-oProxyCommand=x", "u", "n", 1, 2).is_err());
        assert!(start_forward("j", "-u", "n", 1, 2).is_err());
        assert!(start_forward("j", "u", "-n", 1, 2).is_err());
    }

    /// REGRESSION (port-collision false-Alive): if the local port answers but
    /// the ssh child has already exited (ExitOnForwardFailure kills it at a
    /// failed bind while a FOREIGN listener answers the probe), the outcome
    /// must be ChildExited — never Ready. Marking it alive routed user traffic
    /// to the wrong process and flapped forever.
    #[test]
    fn probe_with_dead_child_and_foreign_listener_is_child_exited() {
        // Foreign listener on an ephemeral port.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        // A child that exits immediately (stands in for ssh dying at bind).
        let mut child = Command::new("true").spawn().unwrap();
        let _ = child.wait(); // ensure it has exited (wait reaps; try_wait then reports Some)
        let (outcome, mut child) =
            probe_and_settle(child, port, Duration::from_secs(3)).unwrap();
        assert_eq!(outcome, ProbeOutcome::ChildExited);
        let _ = child.kill();
        let _ = child.wait();
    }

    /// The healthy path: port answers AND the child is still running → Ready.
    #[test]
    fn probe_with_live_child_and_listener_is_ready() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let child = Command::new("sleep").arg("30").spawn().unwrap();
        let (outcome, mut child) =
            probe_and_settle(child, port, Duration::from_secs(3)).unwrap();
        assert_eq!(outcome, ProbeOutcome::Ready);
        let _ = child.kill();
        let _ = child.wait();
    }
}
