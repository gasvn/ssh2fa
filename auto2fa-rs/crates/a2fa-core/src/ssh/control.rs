//! SSH ControlMaster socket path helpers and control-channel commands.
//!
//! Mirrors `get_ssh_control_path`, `update_symlink`, `cleanup_stale_connection`,
//! and the heartbeat `ssh -O check` / `ssh -O exit` calls in `backend.py`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{mpsc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use log::{info, warn};

/// Maximum time to wait for `ssh -O check` to respond.
///
/// A wedged control socket can hang the call indefinitely; we cap it at 5 s
/// and treat timeout as "master not alive".
const MASTER_CHECK_TIMEOUT: Duration = Duration::from_secs(5);

/// Maximum time to wait for `ssh -O exit` to respond.
///
/// Same hazard as `master_check`: a wedged / half-open control socket can hang
/// `ssh -O exit` indefinitely. We cap it at 5 s so teardown and the
/// pre-login `cleanup_stale_socket` can never block forever.
const MASTER_EXIT_TIMEOUT: Duration = Duration::from_secs(5);

/// Maximum time to wait for `ssh -G <host>` (ControlPath resolution).
const SSH_G_TIMEOUT: Duration = Duration::from_secs(5);

/// How often the bounded-run poll loop wakes up to check for child exit.
const BOUNDED_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Result of the cheap, fork-free ControlMaster liveness probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MasterLiveness {
    /// A master is listening on the socket (connect succeeded).
    Alive,
    /// Socket file absent, or present but no listener (ECONNREFUSED).
    Dead,
    /// No confident answer (transient error / would-block). Never escalates.
    Inconclusive,
}

// ---------------------------------------------------------------------------
// ControlPath scheme
// ---------------------------------------------------------------------------
//
// The base ControlPath is resolved from the user's ssh config via `ssh -G`,
// exactly like `get_ssh_control_path` in backend.py. This is essential for
// two reasons:
//   1. Correctness — Rust must honor a `ControlPath ~/.ssh/cm-ssh2fa-%h`
//      directive (with %h/%n/~ expansion done by ssh itself), not invent its
//      own path. Ignoring it would orphan the user's configured sockets.
//   2. Interop / handoff — using the SAME path the Python daemon used lets a
//      freshly-started Rust daemon ADOPT the live ControlMaster sockets instead
//      of re-triggering 2FA on every host.
//
// Resolution (mirrors get_ssh_control_path):
//   * `ssh -G <host>` → take the `controlpath` value.
//   * value "none"         → fall back to ~/.ssh/cm-<host>
//   * no controlpath / err → fall back to ~/.ssh/cm-ssh2fa-<host>
// The result is cached per host (ssh -G is cheap but not free, and the path is
// stable for the lifetime of the daemon).

fn control_base_cache() -> &'static Mutex<HashMap<String, PathBuf>> {
    static CACHE: OnceLock<Mutex<HashMap<String, PathBuf>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Parse the `controlpath` value out of `ssh -G` stdout (case-insensitive key).
/// Returns `None` if there is no controlpath line.
fn parse_ssh_g_controlpath(ssh_g_stdout: &str) -> Option<String> {
    for line in ssh_g_stdout.lines() {
        let mut it = line.splitn(2, ' ');
        if let (Some(key), Some(val)) = (it.next(), it.next()) {
            if key.eq_ignore_ascii_case("controlpath") {
                return Some(val.trim().to_string());
            }
        }
    }
    None
}

/// Turn an `ssh -G` controlpath value (or absence) into a concrete base path,
/// mirroring Python's `get_ssh_control_path` fallbacks.
fn control_base_from_ssh_g(host: &str, value: Option<&str>) -> PathBuf {
    match value {
        Some(v) if v.eq_ignore_ascii_case("none") => {
            dirs_home().join(".ssh").join(format!("cm-{host}"))
        }
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => dirs_home().join(".ssh").join(format!("cm-ssh2fa-{host}")),
    }
}

/// Run `ssh -G <host>` with a timeout and return its stdout, or `None` on
/// timeout / spawn failure / non-zero exit. `ssh -G` does not open a network
/// connection, but a wedged `Match exec`/`ProxyCommand` config could hang it.
fn run_ssh_g(host: &str) -> Option<String> {
    let (tx, rx) = mpsc::channel();
    let host_owned = host.to_string();
    // Own the child INSIDE the worker thread and poll `try_wait` against the
    // deadline so a wedged `ssh -G` (e.g. a hung `Match exec`/`ProxyCommand`)
    // is killed+reaped instead of left running as an orphan. Mirrors
    // `run_ssh_bounded`. (`Command::output()` would block until the child
    // exits, leaking the process past our recv_timeout.)
    let spawn_res = std::thread::Builder::new()
        .name("ssh-g".into())
        .spawn(move || {
            let mut g_args: Vec<String> = crate::config::paths::managed_config_args();
            g_args.push("-G".into());
            g_args.push(host_owned);
            let child = Command::new("ssh")
                .args(&g_args)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn();
            let mut child = match child {
                Ok(c) => c,
                Err(_) => {
                    let _ = tx.send(None);
                    return;
                }
            };

            let deadline = Instant::now() + SSH_G_TIMEOUT;
            loop {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        let stdout = if status.success() {
                            let mut s = String::new();
                            if let Some(mut out) = child.stdout.take() {
                                use std::io::Read;
                                let _ = out.read_to_string(&mut s);
                            }
                            Some(s)
                        } else {
                            None
                        };
                        let _ = tx.send(stdout);
                        return;
                    }
                    Ok(None) => {
                        if Instant::now() >= deadline {
                            let _ = child.kill();
                            let _ = child.wait();
                            let _ = tx.send(None);
                            return;
                        }
                        std::thread::sleep(BOUNDED_POLL_INTERVAL);
                    }
                    Err(_) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        let _ = tx.send(None);
                        return;
                    }
                }
            }
        });
    if let Err(e) = spawn_res {
        // Spawn failed (e.g. transient EAGAIN). The worker never ran, so don't
        // wait on the channel — return None and let the caller fall back to the
        // default ControlPath (resolve_control_base treats None as the fallback).
        warn!("could not spawn ssh-g thread ({e}); falling back to default ControlPath");
        return None;
    }
    // Give the worker a little slack beyond its own deadline to report back;
    // if even that elapses, treat as no result (the worker still reaps).
    match rx.recv_timeout(SSH_G_TIMEOUT + Duration::from_secs(1)) {
        Ok(out) => out,
        Err(_) => None,
    }
}

/// Resolve the base ControlPath for `host`, returning `(path, authoritative)`.
///
/// `authoritative` is `true` iff `ssh -G` actually RAN (success — even when it
/// reports no `ControlPath` directive, which is a legitimate fallback to
/// `cm-ssh2fa-<host>`). It is `false` only when `ssh -G` timed out / failed /
/// couldn't spawn: the returned path is then a GUESS (the same fallback), and
/// callers that would act destructively on it — the boot stray-master sweep —
/// MUST treat `false` as "I don't actually know this host's path" and refrain.
///
/// CRITICAL: a non-authoritative result is NOT cached. The old code cached the
/// failure-fallback for the daemon's lifetime, so one transient `ssh -G` blip
/// at a deploy respawn poisoned the path forever — every adoption probed the
/// wrong path (→ full 2FA relogin) AND the stray sweep killed the host's real
/// live masters as "strays". Not caching lets the next call retry cleanly.
pub fn resolve_control_base_result(host: &str) -> (PathBuf, bool) {
    // Poison-tolerant: this cache is read on login workers AND the heartbeat
    // path — a panicked writer must not poison every future resolution.
    // A cached entry was, by construction, written from a successful ssh -G.
    if let Some(p) = control_base_cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(host)
    {
        return (p.clone(), true);
    }
    match run_ssh_g(host) {
        Some(stdout) => {
            let base =
                control_base_from_ssh_g(host, parse_ssh_g_controlpath(&stdout).as_deref());
            control_base_cache()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(host.to_string(), base.clone());
            (base, true)
        }
        None => (control_base_from_ssh_g(host, None), false),
    }
}

/// Resolve the **base** ControlPath for `host` (no `-<index>` suffix), cached.
///
/// Mirrors `get_ssh_control_path` in backend.py. On `ssh -G` failure returns
/// the (uncached) fallback — see [`resolve_control_base_result`] for the
/// authoritative-vs-guess distinction the stray sweep relies on.
pub fn resolve_control_base(host: &str) -> PathBuf {
    resolve_control_base_result(host).0
}

/// Return the **pool-member** ControlPath for a given host and pool index.
///
/// Mirrors the Python expression:
/// ```python
/// self.pool_control_paths = {
///     i: f"{self.target_control_path}-{i}" for i in range(POOL_SIZE)
/// }
/// ```
/// where `target_control_path` comes from `resolve_control_base` (`ssh -G`).
///
/// The active symlink (`target_control_path`) is stored **without** the
/// `-<index>` suffix and is managed separately by `update_symlink`.
pub fn control_path(host: &str, _index: usize) -> PathBuf {
    // Single-master: the one master binds the **stable base** ControlPath
    // directly — no `-<index>` suffix, no symlink indirection. This IS the path
    // the user's ssh config (`ControlPath ~/.ssh/cm-…-%h`) resolves to, so
    // `ssh <host>` attaches to the live master with nothing in between. The
    // `_index` parameter is retained for call-site compatibility (always 0).
    resolve_control_base(host)
}

/// Return the **active symlink** path (no pool index suffix).
///
/// ssh clients use this path; `update_symlink` keeps it pointing at the
/// currently-active pool member.
pub fn active_symlink_path(host: &str) -> PathBuf {
    resolve_control_base(host)
}

// ---------------------------------------------------------------------------
// Active-symlink management (mirrors `update_symlink` in backend.py)
// ---------------------------------------------------------------------------

/// No-op in the single-master model.
///
/// There is no active symlink anymore: the one master binds the stable base
/// ControlPath directly (see [`control_path`]), so there is nothing to point.
/// Retained as a `true`-returning no-op so the (now single-master) callers don't
/// need conditional logic. `_index` is always 0.
pub fn update_symlink(_host: &str, _index: usize) -> bool {
    true
}

/// Return the pool index the active symlink currently points at, if it exists
/// and ends in a `-<index>` suffix. Used at boot to adopt the slot ssh clients
/// are already multiplexing over.
pub fn symlink_target_index(host: &str) -> Option<usize> {
    let target = std::fs::read_link(active_symlink_path(host)).ok()?;
    parse_trailing_index(&target.to_string_lossy())
}

/// Parse the `-<index>` suffix off a pool socket file name. The base may contain
/// dashes (`cm-ssh2fa-...`) and dots, so we split on the LAST dash. Pool slot
/// suffixes are a SINGLE digit (< POOL_SIZE), so a hostname whose own last
/// dash-component is numeric (e.g. "…-gpu-01" → "01") is rejected rather than
/// misread as a slot index.
fn parse_trailing_index(name: &str) -> Option<usize> {
    name.rsplit_once('-')
        .filter(|(_, idx)| idx.len() == 1)
        .and_then(|(_, idx)| idx.parse::<usize>().ok())
        .filter(|idx| *idx < crate::ssh::master::POOL_SIZE)
}

/// Remove the active symlink and both pool-member socket files for `host`.
pub fn remove_symlink(host: &str) {
    let target = active_symlink_path(host);
    let _ = std::fs::remove_file(&target);
}

// ---------------------------------------------------------------------------
// Control-channel commands (ssh -O …)
// ---------------------------------------------------------------------------

/// The outcome of a bounded control-channel ssh run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BoundedOutcome {
    /// Child exited within the deadline with a successful (0) status.
    Success,
    /// Child exited within the deadline with a non-zero status.
    Failure,
    /// Child did not exit before the deadline and was killed.
    TimedOut,
    /// The child could not be spawned, or `try_wait` errored.
    SpawnError,
}

impl BoundedOutcome {
    /// Treat only a clean exit as "success"; timeout / non-zero / spawn error
    /// are all failures from the caller's point of view.
    fn is_success(self) -> bool {
        matches!(self, BoundedOutcome::Success)
    }
}

/// Run a control-channel `ssh` command (`-O check` / `-O exit` etc.) with a
/// hard deadline, killing the child if it does not exit in time.
///
/// This is the single bounded-ssh chokepoint for control-channel commands: a
/// wedged / half-open ControlMaster socket can make `ssh -O …` hang
/// indefinitely, so we spawn the child with stdout/stderr nulled, poll
/// `try_wait()` every [`BOUNDED_POLL_INTERVAL`], and on deadline `kill()`+`wait()`
/// (reaping the child to avoid a zombie) and report [`BoundedOutcome::TimedOut`].
///
/// `label` is used only for log messages (e.g. "ssh -O check").
/// Reap a JUST-KILLED child without blocking unboundedly.
///
/// `run_ssh_bounded` runs on the heartbeat thread (via `master_check`). The
/// previous `child.wait()` after `child.kill()` could block FOREVER if the
/// killed ssh ignores SIGKILL while stuck in an uninterruptible syscall — which
/// would wedge the heartbeat (the exact class that wedged the pty `ChildReaper`).
/// Poll `try_wait` for a short deadline, then give up; the OS reaps the (tiny,
/// control-channel) zombie when the daemon exits. Bounded, never a wedge.
fn bounded_reap(child: &mut std::process::Child) {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match child.try_wait() {
            Ok(Some(_)) | Err(_) => return,
            Ok(None) => {
                if Instant::now() >= deadline {
                    return; // give up; OS reaps the zombie at daemon exit
                }
                std::thread::sleep(BOUNDED_POLL_INTERVAL);
            }
        }
    }
}

fn run_ssh_bounded(args: &[&str], host: &str, timeout: Duration, label: &str) -> BoundedOutcome {
    let mut child = match Command::new("ssh")
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            warn!("[{host}] {label} spawn failed: {e}");
            return BoundedOutcome::SpawnError;
        }
    };

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return if status.success() {
                    BoundedOutcome::Success
                } else {
                    BoundedOutcome::Failure
                };
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    warn!("[{host}] {label} timed out after {timeout:?} — killing");
                    let _ = child.kill();
                    bounded_reap(&mut child);
                    return BoundedOutcome::TimedOut;
                }
                std::thread::sleep(BOUNDED_POLL_INTERVAL);
            }
            Err(e) => {
                warn!("[{host}] {label} wait error: {e}");
                let _ = child.kill();
                bounded_reap(&mut child);
                return BoundedOutcome::SpawnError;
            }
        }
    }
}

/// Run `ssh -O check -o ControlPath=<path> <host>` and return `true` iff
/// exit code 0 (master is alive and responding).
///
/// This is the *local* check used by the heartbeat — it does NOT send a
/// network round-trip; it just asks the local ControlMaster process for
/// its status. Normally returns in milliseconds.
///
/// A [`MASTER_CHECK_TIMEOUT`] (5 s) is enforced via [`run_ssh_bounded`]: if the
/// child has not exited within that window (e.g. wedged control socket), the
/// process is killed and `false` is returned. This prevents the tick thread
/// from hanging forever.
pub fn master_check(control_path: &Path, host: &str) -> bool {
    let control_opt = format!("ControlPath={}", control_path.display());
    run_ssh_bounded(
        &["-O", "check", "-o", &control_opt, host],
        host,
        MASTER_CHECK_TIMEOUT,
        "ssh -O check",
    )
    .is_success()
}

/// Cheap, fork-free liveness probe for a ControlMaster socket.
///
/// Does a single **blocking** unix-domain `connect()` to `control_path` and maps
/// the outcome:
/// - connect succeeds              → [`MasterLiveness::Alive`] (a master is
///   listening; the kernel completes the connect against the listening socket
///   even if the master's user-space event loop is momentarily busy)
/// - `ECONNREFUSED`                → [`MasterLiveness::Dead`] (file exists, no
///   listener — the master died and left its socket behind)
/// - `NotFound`/`ENOENT`           → [`MasterLiveness::Dead`] (no master at all)
/// - any other error               → [`MasterLiveness::Inconclusive`]
///
/// A unix-domain connect is a *local* operation — it returns in microseconds and
/// cannot block on the network the way `ssh -O check` (which forks a process and
/// can hang for seconds on a stale connection) does. A blocking connect is used
/// deliberately: on macOS a *non-blocking* connect to a dead unix socket returns
/// success prematurely (the refusal only surfaces on later I/O), so it is not a
/// reliable liveness signal — a blocking connect refuses immediately. This
/// replaces `master_check` on the heartbeat hot path and honors the
/// no-wedge-on-the-heartbeat invariant (no subprocess, no network wait).
pub fn master_probe(control_path: &Path) -> MasterLiveness {
    match std::os::unix::net::UnixStream::connect(control_path) {
        Ok(_stream) => MasterLiveness::Alive, // dropped immediately → client disconnect
        Err(e) => match e.raw_os_error() {
            Some(libc::ECONNREFUSED) => MasterLiveness::Dead,
            Some(libc::ENOENT) => MasterLiveness::Dead,
            _ if e.kind() == std::io::ErrorKind::NotFound => MasterLiveness::Dead,
            _ => MasterLiveness::Inconclusive,
        },
    }
}

/// Send `ssh -O exit` to cleanly shut down the ControlMaster for a pool slot.
///
/// Failures are logged but not propagated — an exit may legitimately fail if
/// the master is already dead.
///
/// A [`MASTER_EXIT_TIMEOUT`] (5 s) is enforced via [`run_ssh_bounded`]: a wedged
/// control socket can make `ssh -O exit` hang forever, which would wedge
/// teardown. On deadline the child is killed and the failure is logged.
pub fn master_exit(control_path: &Path, host: &str) {
    let control_opt = format!("ControlPath={}", control_path.display());
    match run_ssh_bounded(
        &["-O", "exit", "-o", &control_opt, host],
        host,
        MASTER_EXIT_TIMEOUT,
        "ssh -O exit",
    ) {
        BoundedOutcome::Success => info!("[{host}] ControlMaster exited cleanly"),
        BoundedOutcome::TimedOut => {
            warn!("[{host}] ssh -O exit timed out (killed) — master may be wedged")
        }
        BoundedOutcome::Failure => {
            warn!("[{host}] ssh -O exit returned non-zero (master may already be dead)")
        }
        BoundedOutcome::SpawnError => warn!("[{host}] ssh -O exit failed to spawn"),
    }
}

/// Remove any stale socket file at `path`, optionally sending `ssh -O exit`
/// first (polite teardown). Mirrors `cleanup_stale_connection` in backend.py
/// minus the zombie-kill logic (that is handled at a higher layer).
///
/// The polite `ssh -O exit` is bounded by [`MASTER_EXIT_TIMEOUT`] via
/// [`run_ssh_bounded`] so a wedged socket cannot block the pre-login cleanup
/// forever. The socket file is force-removed afterward regardless of outcome.
pub fn cleanup_stale_socket(path: &Path, host: &str) {
    // SAFETY GATE: never disturb a master that is currently listening. A live
    // listener means real client sessions may be multiplexed on it — removing
    // its socket or killing it would drop the user's sessions. The reconnect
    // path only reaches cleanup when we've decided the master is gone; this is
    // belt-and-suspenders against a master that recovered in between.
    if master_probe(path) == MasterLiveness::Alive {
        warn!(
            "[{host}] cleanup_stale_socket: master is ALIVE on {} — refusing to clean",
            path.display()
        );
        return;
    }

    // 1. Polite exit via the control socket if it's still there — a graceful
    //    close of the socket's current owner before we hard-kill any remaining
    //    masters in step 2.
    if path.exists() {
        let control_opt = format!("ControlPath={}", path.display());
        let _ = run_ssh_bounded(
            &["-o", &control_opt, "-O", "exit", host],
            host,
            MASTER_EXIT_TIMEOUT,
            "ssh -O exit (cleanup)",
        );
    }

    // 2. PID-based zombie-kill: ALWAYS sweep every ControlMaster on this exact
    //    ControlPath. A polite `ssh -O exit` only closes the socket's CURRENT
    //    owner — any lingering orphan on the same path (its socket already
    //    unlinked + replaced by a newer master, or the socket already gone)
    //    survives it. Gating the sweep on `exit != Success` let duplicate
    //    masters pile up across reconnects (observed: 2 live + 2 orphan k8
    //    masters; and the original 4.5h socket-deleted orphan). Since this runs
    //    pre-login / teardown for this slot, killing every `[mux]` on the path is
    //    correct. Ports backend.py's zombie-kill the Rust rewrite dropped. Common
    //    case (no `[mux]` found) is a single cheap bounded pgrep.
    kill_orphaned_master(path, host);

    // 3. Force-remove the socket file if still present.
    if path.exists() {
        if let Err(e) = std::fs::remove_file(path) {
            warn!("[{host}] Could not remove stale socket {}: {e}", path.display());
        } else {
            info!("[{host}] Removed stale socket {}", path.display());
        }
    }
}

/// Kill an orphaned/wedged ControlMaster process by PID when socket-based
/// `ssh -O exit` can't (wedged control socket, or socket already deleted).
///
/// ssh sets the persistent master's proctitle to `ssh: <ControlPath> [mux]`, so
/// we match the unique per-slot ControlPath and require the `[mux]` marker
/// before killing — so a transient `ssh -O`/login client that merely passes the
/// same path is never hit. SIGTERM, brief grace, then SIGKILL (mirrors
/// backend.py:169-176). Bounded helpers only; runs on the login/teardown worker
/// (never the heartbeat tick).
/// Ask the live master who owns `control_path`: parse the pid out of
/// `ssh -O check`'s "Master running (pid=NNN)" (printed to stderr).
/// `None` → no master / timeout / unparseable.
pub fn master_owner_pid(control_path: &Path, host: &str) -> Option<i32> {
    let control_opt = format!("ControlPath={}", control_path.display());
    let (tx, rx) = mpsc::channel();
    let host_owned = host.to_string();
    let spawn_res = std::thread::Builder::new()
        .name("ssh-check-pid".into())
        .spawn(move || {
            let child = Command::new("ssh")
                .args(["-O", "check", "-o", &control_opt, &host_owned])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .spawn();
            let mut child = match child {
                Ok(c) => c,
                Err(_) => {
                    let _ = tx.send(None);
                    return;
                }
            };
            let deadline = Instant::now() + MASTER_CHECK_TIMEOUT;
            loop {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        let out = if status.success() {
                            let mut s = String::new();
                            if let Some(mut e) = child.stderr.take() {
                                use std::io::Read;
                                let _ = e.read_to_string(&mut s);
                            }
                            Some(s)
                        } else {
                            None
                        };
                        let _ = tx.send(out);
                        return;
                    }
                    Ok(None) => {
                        if Instant::now() >= deadline {
                            let _ = child.kill();
                            let _ = child.wait();
                            let _ = tx.send(None);
                            return;
                        }
                        std::thread::sleep(Duration::from_millis(25));
                    }
                    Err(_) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        let _ = tx.send(None);
                        return;
                    }
                }
            }
        });
    if spawn_res.is_err() {
        return None;
    }
    let text = rx
        .recv_timeout(MASTER_CHECK_TIMEOUT + Duration::from_secs(1))
        .ok()
        .flatten()?;
    parse_master_pid(&text)
}

/// Parse "Master running (pid=NNN)" → NNN.
fn parse_master_pid(check_output: &str) -> Option<i32> {
    let idx = check_output.find("pid=")?;
    let rest = &check_output[idx + 4..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// Extract the socket path from an ssh master proctitle `ssh: <path> [mux]`.
/// Returns `None` for anything that isn't a master line — in particular a
/// multiplexed CLIENT (`ssh -S <path> host cmd`) has the path in its argv but
/// NO `[mux]` proctitle, so it is never matched. Tolerates a path containing
/// spaces (extracts the whole span between `ssh: ` and ` [mux]`).
fn mux_socket_path(cmd: &str) -> Option<&str> {
    let rest = cmd.split_once("ssh: ")?.1;
    let idx = rest.find(" [mux]")?;
    Some(rest[..idx].trim())
}

/// Strip a trailing `-<digits>` pool-slot suffix from a control path token to
/// recover the base. `cm-ssh2fa-h-0` → `cm-ssh2fa-h`; a token with no such
/// suffix (a plain-base master, or a host name ending in `-<digits>` with no
/// slot) is returned unchanged.
fn strip_slot_suffix(tok: &str) -> &str {
    match tok.rfind('-') {
        Some(i) if !tok[i + 1..].is_empty() && tok[i + 1..].chars().all(|c| c.is_ascii_digit()) => {
            &tok[..i]
        }
        _ => tok,
    }
}

/// Kill every `[mux]` master claiming `control_path` EXCEPT the one that
/// actually owns the socket. Returns the number killed.
///
/// WHY: boot adoption (zero-relogin kill-9 deploys) adopts the socket OWNER —
/// but duplicate masters from earlier daemon generations on the SAME path
/// were never targeted by anything: the stale-socket sweep only runs when a
/// slot RESTARTS, and adopted slots don't restart. Observed live: 4 deploys
/// in one day accumulated ~10 duplicate masters, each holding an
/// authenticated connection to the cluster.
pub fn sweep_duplicate_masters(control_path: &Path, host: &str) -> usize {
    let owner = match master_owner_pid(control_path, host) {
        Some(p) => p,
        None => return 0, // no live owner — the restart path's sweep handles it
    };
    let needle = control_path.to_string_lossy().into_owned();
    let found = match crate::sys::run_cmd_bounded("pgrep", &["-f", "--", &needle], Duration::from_secs(2)) {
        Some(o) if o.status.code() == Some(0) => o,
        _ => return 0,
    };
    let mut killed = 0usize;
    for pid_str in String::from_utf8_lossy(&found.stdout).split_whitespace() {
        let pid: i32 = match pid_str.trim().parse() {
            Ok(p) => p,
            Err(_) => continue,
        };
        if pid == owner {
            continue;
        }
        // Confirm: a [mux] master whose proctitle path EXACTLY equals this
        // slot path. Substring matching let a host whose path is a PREFIX of
        // another's (e.g. `gpu` vs `gpu-01`) kill the other host's masters.
        let cmd = crate::sys::run_cmd_bounded("ps", &["-o", "command=", "-p", pid_str.trim()], Duration::from_secs(2))
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        if mux_socket_path(&cmd) != Some(needle.as_str()) {
            continue;
        }
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
        std::thread::sleep(Duration::from_millis(300));
        unsafe {
            if libc::kill(pid, 0) == 0 {
                libc::kill(pid, libc::SIGKILL);
            }
        }
        warn!("[{host}] killed DUPLICATE ControlMaster pid {pid} (owner {owner}, {needle})");
        killed += 1;
    }
    killed
}

/// Boot-time sweep of `cm-ssh2fa-*` masters whose ControlPath base is not
/// among `valid_bases` (the resolved bases of every known host). These are
/// strays from a CHANGED path resolution (ssh-config edits): no per-slot
/// sweep ever targets the old path again, so they leaked forever — observed
/// live as 6h-old `cm-ssh2fa-b8-*` masters after b8's base became
/// `cm-ssh2fa-boslogin08…`. Only our own `cm-ssh2fa-` prefix is touched;
/// custom user ControlPaths are never swept here.
pub fn sweep_stray_masters(valid_bases: &[PathBuf]) -> usize {
    let found = match crate::sys::run_cmd_bounded("pgrep", &["-f", "--", "cm-ssh2fa-"], Duration::from_secs(2)) {
        Some(o) if o.status.code() == Some(0) => o,
        _ => return 0,
    };
    let valid: Vec<String> = valid_bases
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    let mut killed = 0usize;
    for pid_str in String::from_utf8_lossy(&found.stdout).split_whitespace() {
        let pid: i32 = match pid_str.trim().parse() {
            Ok(p) => p,
            Err(_) => continue,
        };
        let cmd = crate::sys::run_cmd_bounded("ps", &["-o", "command=", "-p", pid_str.trim()], Duration::from_secs(2))
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        // Must be a [mux] master whose socket path is one of OURS.
        let path_tok = match mux_socket_path(&cmd) {
            Some(p) if p.contains("cm-ssh2fa-") => p,
            _ => continue,
        };
        // NOT a stray if the full token equals a valid base (a plain-base
        // master, incl. hosts whose name ends in -<digits>) OR if the
        // slot-stripped base does (a normal `<base>-<slot>` master). Checking
        // BOTH avoids mis-stripping a hostname like `node-01` into `node`.
        let base = strip_slot_suffix(path_tok);
        if valid.iter().any(|v| v == path_tok || v == base) {
            continue; // belongs to a known host — adoption/dup-sweep owns it
        }
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
        std::thread::sleep(Duration::from_millis(300));
        unsafe {
            if libc::kill(pid, 0) == 0 {
                libc::kill(pid, libc::SIGKILL);
            }
        }
        warn!("killed STRAY ControlMaster pid {pid} on retired path {path_tok}");
        killed += 1;
    }
    killed
}

fn kill_orphaned_master(control_path: &Path, host: &str) {
    // SAFETY GATE: a listening master is serving clients — do not kill it.
    if master_probe(control_path) == MasterLiveness::Alive {
        return;
    }
    let needle = control_path.to_string_lossy().into_owned();
    let found = match crate::sys::run_cmd_bounded("pgrep", &["-f", "--", &needle], Duration::from_secs(2)) {
        Some(o) if o.status.success() => o, // exit 0 → at least one match
        _ => return,                         // no match / pgrep unavailable
    };
    for pid_str in String::from_utf8_lossy(&found.stdout).split_whitespace() {
        let pid_str = pid_str.trim();
        let pid: i32 = match pid_str.parse() {
            Ok(p) => p,
            Err(_) => continue,
        };
        // Confirm it's the ssh MUX master for THIS exact path, not a transient
        // client and not a prefix-collision (`gpu` vs `gpu-01`): pgrep -f
        // matches substrings, so require the proctitle socket path to equal
        // the needle exactly.
        let cmd = crate::sys::run_cmd_bounded("ps", &["-o", "command=", "-p", pid_str], Duration::from_secs(2))
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        if mux_socket_path(&cmd) != Some(needle.as_str()) {
            continue;
        }
        // SAFETY: kill() with a valid signal id; harmless (ESRCH) if the pid
        // already exited. SIGTERM, 300ms grace, then SIGKILL if still alive.
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
        std::thread::sleep(Duration::from_millis(300));
        unsafe {
            if libc::kill(pid, 0) == 0 {
                libc::kill(pid, libc::SIGKILL);
            }
        }
        warn!("[{host}] killed orphaned ControlMaster pid {pid} ({needle})");
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn dirs_home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ----- parse_master_pid --------------------------------------------------

    #[test]
    fn parse_master_pid_basics() {
        assert_eq!(parse_master_pid("Master running (pid=12345)\n"), Some(12345));
        assert_eq!(parse_master_pid("Master running (pid=1)"), Some(1));
        assert_eq!(parse_master_pid("No ControlPath specified"), None);
        assert_eq!(parse_master_pid("pid="), None);
        assert_eq!(parse_master_pid(""), None);
    }

    // ----- mux_socket_path / strip_slot_suffix -------------------------------

    #[test]
    fn mux_socket_path_extracts_master_only() {
        assert_eq!(
            mux_socket_path("ssh: /Users/x/.ssh/cm-ssh2fa-b8-0 [mux]"),
            Some("/Users/x/.ssh/cm-ssh2fa-b8-0")
        );
        // A multiplexed CLIENT (no [mux] proctitle) → None (never a victim).
        assert_eq!(
            mux_socket_path("ssh -O check -o ControlPath=/Users/x/.ssh/cm-ssh2fa-b8-0 b8"),
            None
        );
        assert_eq!(mux_socket_path("/usr/bin/ssh-agent"), None);
        assert_eq!(mux_socket_path(""), None);
    }

    /// REGRESSION (prefix collision): exact-token match means host `gpu`'s
    /// slot path is NOT a match for host `gpu-01`'s master line.
    #[test]
    fn mux_path_is_exact_not_substring() {
        let gpu0 = "ssh: /h/.ssh/cm-ssh2fa-gpu-0 [mux]";
        let gpu01_0 = "ssh: /h/.ssh/cm-ssh2fa-gpu-01-0 [mux]";
        assert_eq!(mux_socket_path(gpu0), Some("/h/.ssh/cm-ssh2fa-gpu-0"));
        assert_eq!(mux_socket_path(gpu01_0), Some("/h/.ssh/cm-ssh2fa-gpu-01-0"));
        assert_ne!(mux_socket_path(gpu01_0), Some("/h/.ssh/cm-ssh2fa-gpu-0"));
    }

    #[test]
    fn strip_slot_suffix_cases() {
        assert_eq!(strip_slot_suffix("/h/cm-ssh2fa-b8-0"), "/h/cm-ssh2fa-b8");
        assert_eq!(strip_slot_suffix("/h/cm-ssh2fa-b8-1"), "/h/cm-ssh2fa-b8");
        // Host name ending in -<digits>, normal slot suffix stripped once.
        assert_eq!(strip_slot_suffix("/h/cm-ssh2fa-node-01-0"), "/h/cm-ssh2fa-node-01");
        // Plain base (no slot suffix) returned unchanged.
        assert_eq!(strip_slot_suffix("/h/cm-ssh2fa-node-01"), "/h/cm-ssh2fa-node");
        // …which is exactly why sweep_stray_masters ALSO checks the full token
        // against valid_bases (covered by the both-arms `||` there).
    }

    /// sweep_duplicate_masters with no live owner must be a no-op (the
    /// restart path's stale-socket sweep owns that case).
    #[test]
    fn sweep_duplicates_noop_without_owner() {
        let bogus = std::path::Path::new("/tmp/a2fa-test-no-such-socket-xyz");
        assert_eq!(sweep_duplicate_masters(bogus, "nosuchhost-xyz"), 0);
    }

    /// sweep_stray_masters with every running master's base in the valid set
    /// must kill nothing (count can only reflect TRUE strays).
    #[test]
    fn sweep_strays_respects_valid_bases() {
        // Collect the bases of any LIVE cm-ssh2fa masters on this machine
        // and declare them all valid — the sweep must then be a no-op.
        let out = crate::sys::run_cmd_bounded(
            "pgrep",
            &["-f", "--", "cm-ssh2fa-"],
            std::time::Duration::from_secs(2),
        );
        let mut valid: Vec<PathBuf> = Vec::new();
        if let Some(o) = out {
            for pid in String::from_utf8_lossy(&o.stdout).split_whitespace() {
                if let Some(ps) = crate::sys::run_cmd_bounded(
                    "ps",
                    &["-o", "command=", "-p", pid],
                    std::time::Duration::from_secs(2),
                ) {
                    let cmd = String::from_utf8_lossy(&ps.stdout).into_owned();
                    if let Some(tok) = cmd.split_whitespace().find(|t| t.contains("cm-ssh2fa-")) {
                        let base = match tok.rfind('-') {
                            Some(i)
                                if !tok[i + 1..].is_empty()
                                    && tok[i + 1..].chars().all(|c| c.is_ascii_digit()) =>
                            {
                                &tok[..i]
                            }
                            _ => tok,
                        };
                        valid.push(PathBuf::from(base));
                    }
                }
            }
        }
        assert_eq!(
            sweep_stray_masters(&valid),
            0,
            "all live bases declared valid — nothing may be killed"
        );
    }

    // -- Pure resolution helpers (deterministic, no ssh invoked) ------------

    #[test]
    fn parse_controlpath_picks_value_case_insensitive() {
        let out = "user me\nControlPath /home/me/.ssh/cm-ssh2fa-host.example-0\nport 22\n";
        assert_eq!(
            parse_ssh_g_controlpath(out).as_deref(),
            Some("/home/me/.ssh/cm-ssh2fa-host.example-0")
        );
        // lowercase key (ssh -G normalizes to lowercase)
        let out2 = "controlpath none\n";
        assert_eq!(parse_ssh_g_controlpath(out2).as_deref(), Some("none"));
        // no controlpath line
        assert_eq!(parse_ssh_g_controlpath("user me\nport 22\n"), None);
    }

    #[test]
    fn control_base_fallbacks_match_python() {
        // explicit "none" → ~/.ssh/cm-<host>
        let none = control_base_from_ssh_g("b8", Some("none"));
        assert!(none.to_string_lossy().ends_with("/.ssh/cm-b8"), "{none:?}");
        // no controlpath line → ~/.ssh/cm-ssh2fa-<host>
        let missing = control_base_from_ssh_g("b8", None);
        assert!(
            missing.to_string_lossy().ends_with("/.ssh/cm-ssh2fa-b8"),
            "{missing:?}"
        );
        // concrete value (already %h/~ expanded by ssh) → used verbatim
        let concrete = control_base_from_ssh_g(
            "b8",
            Some("/Users/me/.ssh/cm-ssh2fa-boslogin08.rc.fas.harvard.edu"),
        );
        assert_eq!(
            concrete,
            PathBuf::from("/Users/me/.ssh/cm-ssh2fa-boslogin08.rc.fas.harvard.edu")
        );
    }

    // -- Public path API (structural, environment-robust) -------------------

    #[test]
    fn control_path_is_stable_per_host_index() {
        // Use a synthetic host that won't have an ssh config entry → the path
        // is resolved deterministically via the fallback, independent of the
        // machine's ~/.ssh/config.
        let h = "auto2fa-unittest-synthetic-host";
        let a = control_path(h, 0);
        let b = control_path(h, 0);
        assert_eq!(a, b);
        // Single-master: the index is ignored — every call returns the same
        // stable base path.
        assert_eq!(control_path(h, 0), control_path(h, 1));
    }

    #[test]
    fn control_path_is_the_stable_base_with_no_index_suffix() {
        // Single-master: control_path returns the base ControlPath directly
        // (what the user's ssh config resolves to) — no `-<index>` suffix and
        // no symlink. It equals active_symlink_path (also the base).
        let h = "auto2fa-unittest-synthetic-host";
        let p = control_path(h, 0);
        let s = p.to_string_lossy();
        assert!(!s.ends_with("-0"), "base path must have no index suffix: {s}");
        assert!(!s.ends_with("-1"), "base path must have no index suffix: {s}");
        assert!(s.contains(h), "expected host in fallback path: {s}");
        assert_eq!(p, active_symlink_path(h));
    }

    #[test]
    fn control_path_different_hosts_differ() {
        assert_ne!(
            control_path("auto2fa-unittest-host-a", 0),
            control_path("auto2fa-unittest-host-b", 0)
        );
    }

    #[test]
    fn parse_trailing_index_handles_dashed_base() {
        // Single-master (POOL_SIZE=1): only slot -0 is a valid index; the old
        // two-slot -1 suffix is no longer recognized.
        assert_eq!(
            parse_trailing_index("/Users/me/.ssh/cm-ssh2fa-boslogin08.rc.fas.harvard.edu-1"),
            None
        );
        assert_eq!(parse_trailing_index("cm-ssh2fa-k6-0"), Some(0));
        assert_eq!(parse_trailing_index("no-index-here"), None);
        assert_eq!(parse_trailing_index("plainname"), None);
        // A hostname's OWN numeric dash-component must not be misread as a
        // slot index: multi-digit ("…-gpu-01") and out-of-range ("…-5") are
        // rejected (slots are a single digit < POOL_SIZE).
        assert_eq!(parse_trailing_index("cm-ssh2fa-gpu-01"), None);
        assert_eq!(parse_trailing_index("cm-ssh2fa-node-5"), None);
    }

    /// `master_check` with a bogus (non-existent) control socket must return
    /// `false` quickly — well within the 5 s timeout.
    #[test]
    fn master_check_bogus_path_returns_false_quickly() {
        use std::time::Instant;
        let bogus = std::path::Path::new("/tmp/bogus-auto2fa-test-socket-does-not-exist");
        let t0 = Instant::now();
        let result = master_check(bogus, "localhost");
        let elapsed = t0.elapsed();
        assert!(!result, "bogus path must return false");
        // ssh -O check fails immediately for a missing socket — must be << 5 s.
        assert!(
            elapsed < std::time::Duration::from_secs(4),
            "master_check took too long: {elapsed:?}"
        );
    }

    /// The bounded helper must return promptly for an instantly-failing
    /// command (`ssh -O exit` against a nonexistent control path with
    /// BatchMode → fails fast) — well within the timeout. This exercises the
    /// one bounded-ssh chokepoint that `master_exit` / `cleanup_stale_socket`
    /// route through.
    #[test]
    fn run_ssh_bounded_fails_fast_for_missing_socket() {
        use std::time::Instant;
        let control_opt = "ControlPath=/tmp/bogus-auto2fa-test-socket-does-not-exist";
        let t0 = Instant::now();
        let outcome = run_ssh_bounded(
            &["-o", "BatchMode=yes", "-o", control_opt, "-O", "exit", "localhost"],
            "localhost",
            MASTER_EXIT_TIMEOUT,
            "ssh -O exit (test)",
        );
        let elapsed = t0.elapsed();
        // No live master at the bogus path → ssh -O exit fails fast (NOT a
        // timeout).
        assert!(
            !outcome.is_success(),
            "exit against a missing socket must not succeed"
        );
        assert_ne!(
            outcome,
            BoundedOutcome::TimedOut,
            "instantly-failing command must not hit the deadline"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(4),
            "run_ssh_bounded took too long: {elapsed:?}"
        );
    }
}

#[cfg(test)]
mod probe_tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    #[test]
    fn probe_alive_when_listener_present() {
        let dir = std::env::temp_dir().join(format!("a2fa-probe-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let sock = dir.join("alive.sock");
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).unwrap();
        assert_eq!(master_probe(&sock), MasterLiveness::Alive);
        drop(listener);
        let _ = std::fs::remove_file(&sock);
    }

    #[test]
    fn probe_dead_when_socket_file_absent() {
        let sock = std::env::temp_dir().join("a2fa-probe-absent-does-not-exist.sock");
        let _ = std::fs::remove_file(&sock);
        assert_eq!(master_probe(&sock), MasterLiveness::Dead);
    }

    #[test]
    fn cleanup_is_noop_when_master_is_listening() {
        let dir = std::env::temp_dir().join(format!("a2fa-cleanup-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let sock = dir.join("live.sock");
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).unwrap();
        // A live listener must survive cleanup: the socket file is untouched.
        cleanup_stale_socket(&sock, "testhost");
        assert!(sock.exists(), "cleanup must not remove a live master's socket");
        assert_eq!(master_probe(&sock), MasterLiveness::Alive);
        drop(listener);
        let _ = std::fs::remove_file(&sock);
    }

    #[test]
    fn probe_dead_when_socket_lingers_without_listener() {
        // std's UnixListener does NOT unlink on drop, so the file lingers with
        // no listener — exactly the "master died, socket left behind" case.
        let dir = std::env::temp_dir().join(format!("a2fa-probe-stale-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let sock = dir.join("stale.sock");
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).unwrap();
        drop(listener); // fd closed, file remains
        // Poll to tolerate a TEST-ONLY race: the a2fa-core suite has tests that
        // fork+exec subprocesses, and on macOS CLOEXEC is set non-atomically
        // after socket(), so a concurrent fork can briefly inherit our listener
        // fd and keep the socket connectable until that short-lived child exits.
        // The probe itself is correct (verified single-threaded); we just wait
        // out the race. (Production is unaffected: the daemon never binds master
        // sockets, so it can't keep a dead master's socket alive.)
        let mut result = master_probe(&sock);
        for _ in 0..100 {
            if result == MasterLiveness::Dead {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
            result = master_probe(&sock);
        }
        assert_eq!(result, MasterLiveness::Dead);
        let _ = std::fs::remove_file(&sock);
    }
}
