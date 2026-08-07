//! IPC handlers for host-related methods.
//!
//! Methods: ping, list_hosts, host_toggle, host_mount_toggle,
//!          host_rotate, host_add, host_test_credentials.
//!
//! Parity: `Auto2FADaemon.handle_request` in daemon.py.
//!
//! # Live-SSH methods
//! `host_toggle`, `host_mount_toggle`, `host_rotate`, `host_add`, and
//! `host_test_credentials` all call real core functions.
//! Methods that require blocking I/O (start_master, sshfs, test login) do so
//! OFF the State mutex lock — see `crate::workers` for the threading helpers.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock};

use a2fa_core::config::{load_meta, passwords_path, update_meta, HostMeta};
use a2fa_core::creds::platform_store;
use a2fa_core::creds::{delete_credentials, get_otpauth, get_password, store_credentials};
use a2fa_core::engine::State;
use a2fa_core::error::{Error, Result};
use a2fa_core::model::Host;
use a2fa_core::sys::run_cmd_bounded;
use a2fa_core::totp::{describe_otp, extract_secret, extract_secret_optional, totp_now_detailed};
use serde_json::{json, Value};

use crate::managers::{spawn_managed_start, spawn_managed_stop, HostManagers};
use crate::workers::{spawn_host_start, spawn_host_stop, OtpRegistry};

// ---------------------------------------------------------------------------
// Snapshot helpers (mirror `_host_snapshot` in daemon.py)
// ---------------------------------------------------------------------------

/// Build a JSON snapshot of a single `Host`, matching daemon.py's
/// `_host_snapshot` return dict exactly.
pub fn host_snapshot(h: &Host) -> Value {
    json!({
        "host": h.host,
        "status": h.status,
        "active": h.active,
        "is_master_ready": h.is_master_ready,
        "pool_index": h.pool_index,
        "pool_alive": h.pool_alive,
        "is_mounted": h.is_mounted,
        "last_msg": h.last_msg,
    })
}

// ---------------------------------------------------------------------------
// ping
// ---------------------------------------------------------------------------

pub fn ping(state: &Arc<Mutex<State>>) -> Result<Value> {
    let _guard = crate::lock_state(state);
    Ok(json!({ "ok": true, "pid": std::process::id() }))
}

// ---------------------------------------------------------------------------
// list_hosts
// ---------------------------------------------------------------------------

pub fn list_hosts(state: &Arc<Mutex<State>>) -> Result<Value> {
    let guard = crate::lock_state(state);
    let snaps: Vec<Value> = guard.hosts.iter().map(host_snapshot).collect();
    Ok(json!(snaps))
}

// ---------------------------------------------------------------------------
// host_toggle
// ---------------------------------------------------------------------------

/// Toggle a host's active/inactive state.
///
/// If inactive → mark active in State + spawn a background worker that calls
/// `start_master` (blocking ssh pty).
/// If active → spawn a background worker that calls `stop_all` (ssh -O exit)
/// + marks inactive.
///
/// The OTP lock registry is passed in as a daemon-global `Arc<OtpRegistry>`.
/// Handler callers that don't have the registry (e.g. tests) can call the
/// test-only `host_toggle_simple` variant that only flips the flag.
pub fn host_toggle(state: &Arc<Mutex<State>>, params: &Value) -> Result<Value> {
    host_toggle_with_registry(state, params, None)
}

/// Full implementation — optionally takes a registry so tests can inject one.
pub fn host_toggle_with_registry(
    state: &Arc<Mutex<State>>,
    params: &Value,
    registry: Option<Arc<OtpRegistry>>,
) -> Result<Value> {
    let host_name = params["host"]
        .as_str()
        .ok_or_else(|| Error::BadParams("host required".into()))?
        .to_owned();

    // Snapshot the current active state under a BRIEF lock…
    let currently_active = {
        let guard = crate::lock_state(state);
        guard
            .hosts
            .iter()
            .find(|h| h.host == host_name)
            .ok_or_else(|| Error::NotFound(format!("host {host_name}")))?
            .active
    };
    // …then fetch credentials from the Keychain with NO lock held. macOS
    // serializes Keychain access process-wide and a locked Keychain blocks on
    // a SecurityAgent prompt — doing this inside the lock_state block would
    // wedge EVERY State user (heartbeat, all handlers) behind one prompt.
    let (password_opt, otpauth_opt) = if currently_active {
        (None, None) // deactivation needs no creds — skip the Keychain entirely
    } else {
        let ks = platform_store();
        (
            get_password(&ks, &host_name).ok().flatten(),
            get_otpauth(&ks, &host_name).ok().flatten(),
        )
    };

    if currently_active {
        // Deactivate: reset circuit breakers in State + spawn stop worker.
        {
            let mut guard = crate::lock_state(state);
            if let Some(h) = guard.hosts.iter_mut().find(|h| h.host == host_name) {
                h.active = false;
                h.last_msg = "Deactivating…".into();
            }
        }
        spawn_host_stop(host_name.clone(), Arc::clone(state));
    } else {
        // Activate: flip active flag + reset circuit breakers + spawn start worker.
        let password = password_opt.unwrap_or_default();
        let otpauth = otpauth_opt.unwrap_or_default();
        let secret = extract_secret(&otpauth).unwrap_or_default();

        {
            let mut guard = crate::lock_state(state);
            if let Some(h) = guard.hosts.iter_mut().find(|h| h.host == host_name) {
                h.active = true;
                h.last_msg = "Connecting…".into();
            }
        }

        let reg = registry.unwrap_or_default();
        spawn_host_start(
            host_name.clone(),
            0, // always start slot 0 on toggle
            password,
            secret,
            reg,
            Arc::clone(state),
        );
    }

    // Return the current snapshot (start/stop complete asynchronously).
    let guard = crate::lock_state(state);
    let snap = guard
        .hosts
        .iter()
        .find(|h| h.host == host_name)
        .map(host_snapshot)
        .unwrap_or(Value::Null);
    Ok(snap)
}

// ---------------------------------------------------------------------------
// host_toggle_managed — uses persistent HostManagers (production path)
// ---------------------------------------------------------------------------

/// Toggle a host using the persistent `HostManagers` registry.
///
/// Behaves like `host_toggle_with_registry` but:
/// * Uses `spawn_managed_start` / `spawn_managed_stop` so cooldown / failure
///   counts survive across retries (the circuit-breaker-reset bug is fixed).
/// * After slot 0 becomes ready, kicks off `spawn_warmup_slot1` (staggered,
///   ~5 s) to pre-warm the spare pool slot.
/// * On deactivate: `spawn_managed_stop` which calls `stop_all` and
///   `reset_circuit_breakers` on the persistent `PoolState`.
///
/// When `managers` or `registry` are `None`, falls back to the legacy
/// transient behaviour (used by tests that don't supply a context).
/// Persist a host's auto-connect flag to passwords.json so a toggle survives a
/// daemon restart. Without this, `host_toggle` only flipped the in-memory
/// `active` flag, so a stopped host came back (boot auto-start re-read
/// autoConnect=true) on the next launch — the "stop doesn't work" bug.
/// Best-effort + off the State lock. Goes through `update_meta` so the
/// load→modify→save is serialized against concurrent handler threads
/// (host_add) — separate load_meta/save_meta calls raced and lost updates.
fn persist_host_autoconnect(host: &str, on: bool) {
    let path = passwords_path();
    let res = update_meta(&path, |meta| {
        meta.entry(host.to_string())
            .and_modify(|m| m.auto_connect = on)
            .or_insert(HostMeta { auto_connect: on });
    });
    if let Err(e) = res {
        log::warn!("host_toggle: failed to persist autoConnect={on} for {host}: {e}");
    }
}

pub fn host_toggle_managed(
    state: &Arc<Mutex<State>>,
    params: &Value,
    managers: Option<Arc<HostManagers>>,
    registry: Option<Arc<OtpRegistry>>,
) -> Result<Value> {
    let host_name = params["host"]
        .as_str()
        .ok_or_else(|| Error::BadParams("host required".into()))?
        .to_owned();

    // Snapshot the active flag while holding the lock.
    // NO Keychain read happens on this handler thread — `spawn_managed_start`
    // and `spawn_warmup_slot1` read the creds inside their own worker threads,
    // so a stalled "Always Allow" prompt can never wedge the IPC handler.
    let currently_active = {
        let guard = crate::lock_state(state);
        let host = guard
            .hosts
            .iter()
            .find(|h| h.host == host_name)
            .ok_or_else(|| Error::NotFound(format!("host {host_name}")))?;
        host.active
    };

    match (managers, registry) {
        (Some(mgrs), Some(reg)) => {
            if currently_active {
                // Deactivate: update State flag + spawn stop (uses persistent pool).
                {
                    let mut guard = crate::lock_state(state);
                    if let Some(h) = guard.hosts.iter_mut().find(|h| h.host == host_name) {
                        h.active = false;
                        h.last_msg = "Deactivating…".into();
                    }
                }
                // Persist so the stop survives a daemon restart.
                persist_host_autoconnect(&host_name, false);
                spawn_managed_stop(host_name.clone(), Arc::clone(state), Arc::clone(&mgrs));
            } else {
                // Activate: reset circuit breakers (on the persistent state) + start.
                // Reset circuit breakers so a manual toggle gives a fresh start.
                mgrs.with_pool_mut(&host_name, |p| p.reset_circuit_breakers());

                {
                    let mut guard = crate::lock_state(state);
                    if let Some(h) = guard.hosts.iter_mut().find(|h| h.host == host_name) {
                        h.active = true;
                        h.last_msg = "Connecting…".into();
                        h.status = "Connecting".into();
                    }
                }
                // Persist so the host re-connects on the next daemon restart.
                persist_host_autoconnect(&host_name, true);

                // Single master — spawn the one master (reads creds in-thread).
                spawn_managed_start(
                    host_name.clone(),
                    0,
                    Arc::clone(&reg),
                    Arc::clone(state),
                    Arc::clone(&mgrs),
                );
            }
        }
        // Legacy fallback (no persistent managers — used by unit tests).
        _ => {
            return host_toggle_with_registry(state, params, None);
        }
    }

    let guard = crate::lock_state(state);
    let snap = guard
        .hosts
        .iter()
        .find(|h| h.host == host_name)
        .map(host_snapshot)
        .unwrap_or(Value::Null);
    Ok(snap)
}

// ---------------------------------------------------------------------------
// host_mount_toggle
// ---------------------------------------------------------------------------

/// Per-host in-flight latch for mount/unmount, mirroring `totp_in_flight`.
///
/// sshfs/umount can block for many seconds (or wedge entirely on a dead login
/// node). Without this latch, repeated `host_mount_toggle` calls for the same
/// host (a held key, a TUI auto-refresh) each spawn ANOTHER sshfs→ssh subtree,
/// piling up wedged mounts/processes — the unbounded-spawn class. The latch caps
/// it to at most one mount op per host in flight; concurrent callers get "busy".
fn mount_in_flight() -> &'static Mutex<HashSet<String>> {
    static IN_FLIGHT: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    IN_FLIGHT.get_or_init(|| Mutex::new(HashSet::new()))
}

/// RAII guard releasing a host's `mount_in_flight` entry on every exit path.
struct MountInFlightGuard {
    host: String,
}

impl Drop for MountInFlightGuard {
    fn drop(&mut self) {
        mount_in_flight()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.host);
    }
}

/// First non-empty line of `s`, capped, for a one-line status row.
///
/// sshfs can print several lines (a warning plus the real error); a status row
/// shows one, and the full text is in the log.
fn first_line(s: &str) -> String {
    let line = s.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    if line.chars().count() > 120 {
        let cut: String = line.chars().take(117).collect();
        format!("{cut}…")
    } else {
        line.to_owned()
    }
}

/// Locate the `sshfs` binary, or explain how to install it for this platform.
///
/// Bounded — `which` is instant, but never block. Under launchd the daemon's
/// PATH is the plist's minimal system set, which does NOT include
/// /usr/local/bin or /opt/homebrew/bin: `which sshfs` fails there even with
/// sshfs installed (mount was dead in production), so the well-known install
/// prefixes are also probed by absolute path.
///
/// Resolved LAZILY, at the point of mounting. It used to run at the top of
/// `host_mount_toggle`, which made UNMOUNTING impossible on a machine without
/// sshfs — unmounting needs only `fusermount`/`umount`, and a user whose sshfs
/// was uninstalled (or who is on a box that never had it) was left with a mount
/// they could not remove and a message telling them to install a mounting tool.
fn locate_sshfs() -> Result<String> {
    let which_ok = run_cmd_bounded("which", &["sshfs"], std::time::Duration::from_secs(5))
        .map(|o| o.status.success())
        .unwrap_or(false);
    if which_ok {
        return Ok("sshfs".into());
    }
    let candidates: &[&str] = if cfg!(target_os = "macos") {
        &["/usr/local/bin/sshfs", "/opt/homebrew/bin/sshfs"]
    } else {
        &["/usr/bin/sshfs", "/usr/local/bin/sshfs"]
    };
    if let Some(p) = candidates.iter().find(|p| std::path::Path::new(p).is_file()) {
        return Ok((*p).to_string());
    }
    Err(Error::Internal(if cfg!(target_os = "macos") {
        "sshfs not installed — install macFUSE + sshfs to use this feature".into()
    } else {
        "sshfs not installed — install it (Debian/Ubuntu: sudo apt install sshfs) \
         to use this feature"
            .into()
    }))
}

/// Unmount a FUSE filesystem with the tool this platform gives an unprivileged
/// user, falling back through the alternatives until one reports success.
///
/// Returns `true` if a command succeeded. Bounded on every attempt: a wedged
/// mount whose server is gone is exactly when these can block.
fn force_unmount(mount_point: &str) -> bool {
    use std::time::Duration;
    let (cmd, lead) = a2fa_core::platform::unmount_command();
    let mut attempts: Vec<(&str, Vec<&str>)> = vec![(cmd, lead.to_vec())];
    for (c, a) in a2fa_core::platform::unmount_fallbacks() {
        attempts.push((c, a.to_vec()));
    }
    for (cmd, lead) in attempts {
        let mut args = lead;
        args.push(mount_point);
        if let Some(o) = run_cmd_bounded(cmd, &args, Duration::from_secs(10)) {
            if o.status.success() {
                return true;
            }
        }
    }
    false
}

/// Reap the leaked artifacts of a FAILED sshfs mount.
///
/// On macOS sshfs's backend (`go-nfsv4`) is a separately-daemonized process:
/// when the mount fails (or `run_cmd_bounded` kills the sshfs child on its
/// deadline), the backend survives, holding a half-made mount. On Linux sshfs
/// itself is that process. Either way it is targeted by the exact mount point
/// so an unrelated mount is never touched. Bounded helpers only; runs off the
/// State lock.
fn reap_failed_sshfs(mount_point: &std::path::Path) {
    use std::time::Duration;
    let mp = mount_point.to_string_lossy().into_owned();
    // 1. Kill the leaked backend(s) whose argv carries this mount path.
    if let Some(o) = run_cmd_bounded("pgrep", &["-f", &mp], Duration::from_secs(2)) {
        if o.status.success() {
            for pid in String::from_utf8_lossy(&o.stdout).split_whitespace() {
                let cmd = run_cmd_bounded("ps", &["-o", "command=", "-p", pid], Duration::from_secs(2))
                    .map(|x| String::from_utf8_lossy(&x.stdout).into_owned())
                    .unwrap_or_default();
                // Only kill sshfs / its FUSE backend for THIS mount path.
                if a2fa_core::platform::fuse_process_markers()
                    .iter()
                    .any(|m| cmd.contains(m))
                {
                    let _ = run_cmd_bounded("kill", &["-9", pid], Duration::from_secs(1));
                }
            }
        }
    }
    // 2. Force-unmount a half-made mount, then remove the now-empty dir.
    let _ = force_unmount(&mp);
    let _ = std::fs::remove_dir(mount_point); // only succeeds if empty
}

/// Toggle sshfs mount for a host: mount if not mounted, unmount if mounted.
///
/// Every external command (which/umount/sshfs) runs through `run_cmd_bounded`
/// with a hard kill-on-deadline so a wedged login node can never pin the handler
/// thread forever; sshfs carries `ConnectTimeout=10` so its embedded ssh fails
/// fast; and a per-host in-flight latch prevents duplicate mount subtrees.
/// Mirrors `SSHHostManager.toggle_mount` in backend.py.
pub fn host_mount_toggle(state: &Arc<Mutex<State>>, params: &Value) -> Result<Value> {
    let host_name = params["host"]
        .as_str()
        .ok_or_else(|| Error::BadParams("host required".into()))?
        .to_owned();

    // WHICH remote directory to mount. Defaults to "/" (the original
    // behaviour). Mounting the directory you actually work in — rather than the
    // filesystem root every time — is the difference between a mount you use
    // and one you re-navigate on every login.
    let remote_path = opt_str(params, "remote_path")?
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or("/")
        .to_owned();
    validate_remote_path(&remote_path)?;

    // Parsed and validated BEFORE the mount latch is claimed, like remote_path
    // above: a malformed request must not briefly block legitimate mount work
    // for this host. (Caught by a test that flaked only when run alongside
    // another test using the same host name — the latch is process-global.)
    let explicit_point = opt_str(params, "mount_point")?.map(std::path::PathBuf::from);
    if let Some(p) = &explicit_point {
        // Only ever act on paths inside our own mounts root.
        if !p.starts_with(mounts_root()) {
            return Err(Error::BadParams(
                "mount_point must be inside the SSH2FA mounts folder".into(),
            ));
        }
    }

    // Claim the per-host latch FIRST (RAII release on every path). A second
    // toggle for the same host while one is in flight returns "busy" instead of
    // stacking another sshfs→ssh subtree.
    {
        let mut inflight = mount_in_flight().lock().unwrap_or_else(|e| e.into_inner());
        if !inflight.insert(host_name.clone()) {
            return Err(Error::Internal(format!(
                "mount/unmount already in progress for {host_name}"
            )));
        }
    }
    let _mount_guard = MountInFlightGuard { host: host_name.clone() };

    // Verify the host exists. Its cached `is_mounted` flag is deliberately NOT
    // used to decide what to do — the kernel mount table below is the ground
    // truth, and the flag goes stale whenever a volume is ejected in Finder or
    // dies with the network.
    {
        let guard = crate::lock_state(state);
        guard
            .hosts
            .iter()
            .find(|h| h.host == host_name)
            .ok_or_else(|| Error::NotFound(format!("host {host_name}")))?;
    }

    // Validate the host name is mount-safe (no '/' or '..').
    // host_add validates names on the way in; this guards legacy entries.
    if host_name.contains('/') || host_name.contains("..") || host_name.is_empty() {
        return Err(Error::BadParams("invalid host name for mount".into()));
    }

    // ~/Mounts/<host>/<slug> — several folders per host can be mounted at once.
    //
    // An explicit `mount_point` addresses an EXISTING mount directly. Callers
    // unmounting one of several mounts must use it: the mount table's `source`
    // column cannot be mapped back to a remote path in general (with the fuse-t
    // backend it reads `fuse-t:/<volname>`, not `<host>:<path>`), and the slug
    // in the path is lossy — `/a/b` and `/a-b` produce the same one.
    let mount_point = explicit_point
        .clone()
        .unwrap_or_else(|| mount_point_for(&host_name, &remote_path));

    // Decide from the kernel mount table, never by stat'ing the path: a wedged
    // macFUSE mount makes stat block forever, and that is exactly the state a
    // user is in when they reach for unmount.
    let active = a2fa_core::mounts::list_active_mounts(&mounts_root());
    let this_is_mounted = active.iter().any(|m| m.mount_point == mount_point);

    // An explicit mount_point names an EXISTING mount to unmount. If it is not
    // mounted, this is not a mount request that happens to have a path — the
    // toggle would fall through and mount the DEFAULT remote path ("/") at that
    // directory, i.e. mount something the caller never asked for. Reachable as
    // a race: the UI lists a mount, it disappears, the user then clicks it.
    if explicit_point.is_some() && !this_is_mounted {
        return Ok(json!({
            "host": host_name,
            "mounted": false,
            "mount_point": mount_point.to_string_lossy(),
            "remote_path": remote_path,
            "note": "already unmounted",
        }));
    }
    // The pre-existing layout mounted the host at ~/Mounts/<host> itself, which
    // blocks creating subdirectories under it. Unmount that legacy mount before
    // mounting into the new layout.
    let legacy_point = mounts_root().join(&host_name);
    if !this_is_mounted {
        if let Some(legacy) = active.iter().find(|m| m.host == host_name && m.slug.is_empty()) {
            log::info!(
                "[{host_name}] unmounting legacy single-mount at {} to switch to the per-folder layout",
                legacy.mount_point.display()
            );
            let lp = legacy_point.to_string_lossy().into_owned();
            let _ = force_unmount(&lp);
        }
    }

    // Why a mount attempt failed, surfaced in the RPC reply. Empty on success
    // and on the unmount path.
    let mut mount_failure = String::new();

    if this_is_mounted {
        // Unmount.
        {
            let mut guard = crate::lock_state(state);
            if let Some(h) = guard.hosts.iter_mut().find(|h| h.host == host_name) {
                h.last_msg = "Unmounting…".into();
            }
        }
        let mp_str = mount_point.to_string_lossy().into_owned();
        // Bounded, and platform-correct: a kernel-stuck unmount on a wedged
        // FUSE mount must not pin the handler thread forever, and on Linux only
        // fusermount can do this without root.
        let _ = force_unmount(&mp_str);
        // Judge ONLY by the actual mount state. Requiring umount's exit status
        // wedged the latch: if macFUSE had ALREADY auto-unmounted (network
        // drop), `umount -f` fails ("not currently mounted") → unmounted=false
        // → is_mounted stuck true and every retry hit the same failing branch.
        // Re-read the mount table (no stat — see above).
        let still = a2fa_core::mounts::list_active_mounts(&mounts_root());
        let unmounted = !still.iter().any(|m| m.mount_point == mount_point);
        // Tidy the now-empty per-host directory so ~/Mounts doesn't accumulate
        // husks; harmless if other mounts for this host keep it non-empty.
        if unmounted {
            let _ = std::fs::remove_dir(&mount_point);
            let _ = std::fs::remove_dir(mounts_root().join(&host_name));
        }
        let host_still_mounted = still.iter().any(|m| m.host == host_name);
        let mut guard = crate::lock_state(state);
        if let Some(h) = guard.hosts.iter_mut().find(|h| h.host == host_name) {
            h.is_mounted = host_still_mounted;
            h.last_msg = if unmounted { "Unmounted" } else { "Unmount failed" }.into();
        }
    } else {
        // Mount. Resolve sshfs HERE, not at the top of the handler: unmounting
        // must keep working on a machine that has no sshfs (see locate_sshfs).
        let sshfs_bin = locate_sshfs()?;
        let _ = std::fs::create_dir_all(&mount_point);
        {
            let mut guard = crate::lock_state(state);
            if let Some(h) = guard.hosts.iter_mut().find(|h| h.host == host_name) {
                h.last_msg = "Mounting…".into();
            }
        }
        let mp_str2 = mount_point.to_string_lossy().into_owned();
        let src = format!("{host_name}:{remote_path}");
        // ConnectTimeout=10 makes the embedded ssh fail fast on a dead/slow
        // login node (the single highest-value change — without it sshfs hangs
        // on TCP connect / auth and a stat of the half-made mount can freeze
        // Finder/Spotlight machine-wide). run_cmd_bounded is a generous 45s
        // backstop: sshfs daemonizes after a successful mount so .wait already
        // returns on success; the deadline only fires on a never-returning child.
        // `volname` is macFUSE-only — Linux sshfs REJECTS unknown options, so it
        // comes from the platform module rather than being hardcoded here.
        let opts = format!(
            "reconnect,ConnectTimeout=10,ServerAliveInterval=15,ServerAliveCountMax=3,\
             {}StrictHostKeyChecking=no,UserKnownHostsFile=/dev/null",
            a2fa_core::platform::sshfs_platform_opts(&host_name)
        );
        let result = run_cmd_bounded(
            &sshfs_bin,
            &[&src, &mp_str2, "-o", &opts],
            std::time::Duration::from_secs(45),
        );
        // Why sshfs failed, in sshfs's own words. Previously the whole Output
        // was collapsed to a bool, so a failed mount reached the user as a bare
        // `mounted: false` with NO reason — "no such remote directory", "host
        // key changed" and "permission denied" were indistinguishable, from the
        // UI and from the log alike.
        let mut failure = String::new();
        let mounted = match result {
            None => {
                failure = format!("sshfs did not finish within 45s (mounting {src})");
                false
            }
            Some(o) => {
                let exited_ok = o.status.success();
                // sshfs backgrounds itself once the mount is established, so
                // the kernel mount table — not the exit status — is the
                // authority on whether it actually worked.
                let in_table = a2fa_core::mounts::list_active_mounts(&mounts_root())
                    .iter()
                    .any(|m| m.mount_point == mount_point);
                if !exited_ok || !in_table {
                    let stderr = String::from_utf8_lossy(&o.stderr).trim().to_owned();
                    let stdout = String::from_utf8_lossy(&o.stdout).trim().to_owned();
                    let said = if !stderr.is_empty() {
                        stderr
                    } else if !stdout.is_empty() {
                        stdout
                    } else if exited_ok {
                        // Exit 0 but nothing mounted: sshfs reported success
                        // and the mount is not there.
                        "sshfs exited 0 but no mount appeared".to_owned()
                    } else {
                        format!("sshfs exited with {}", o.status)
                    };
                    failure = said;
                }
                exited_ok && in_table
            }
        };
        if !mounted {
            log::warn!("[{host_name}] mount of {remote_path} failed: {failure}");
            // A failed/killed sshfs can leave a DAEMONIZED backend running
            // (on macOS the separate go-nfsv4 process; run_cmd_bounded only
            // killed the direct child) plus a half-made mount and the created
            // dir. Reap them so failed mounts don't leak (observed: 5+
            // orphaned go-nfsv4 processes).
            reap_failed_sshfs(&mount_point);
        }
        let mut guard = crate::lock_state(state);
        if let Some(h) = guard.hosts.iter_mut().find(|h| h.host == host_name) {
            h.is_mounted = mounted;
            h.last_msg = if mounted {
                format!("Mounted {remote_path}")
            } else {
                // Carry the reason into the row the user is looking at, rather
                // than a bare "Mount failed" they have to go log-diving for.
                format!("Mount failed — {}", first_line(&failure))
            };
        }
        mount_failure = failure;
    }

    // Report the mount point + what is mounted there. The app opens this in
    // Finder on a successful mount, so it must come back from the RPC rather
    // than being re-derived (and re-guessed) client-side.
    //
    // `mounted` answers "is THE POINT THIS CALL ADDRESSED mounted", not "does
    // this host have some mount". Asking the looser question made a failed
    // mount report `mounted: true` whenever any OTHER folder on the same host
    // happened to be mounted — the caller then opened a directory that its own
    // request had just failed to create.
    let active = a2fa_core::mounts::list_active_mounts(&mounts_root());
    let is_mounted_now = active.iter().any(|m| m.mount_point == mount_point);
    let host_has_other_mounts = active
        .iter()
        .any(|m| m.host == host_name && m.mount_point != mount_point);
    let mut reply = json!({
        "host": host_name,
        "mounted": is_mounted_now,
        "mount_point": mount_point.to_string_lossy(),
        "remote_path": remote_path,
        // The host row shows a single mounted/not indicator, which must stay
        // lit while any other folder on the host is still mounted.
        "host_has_other_mounts": host_has_other_mounts,
    });
    // Only present when something went wrong — a client can then say WHY
    // instead of just "mounted: false".
    if !mount_failure.is_empty() {
        reply["error"] = json!(mount_failure);
    }
    Ok(reply)
}

/// Validate a user-supplied remote directory for `sshfs host:<path>`.
///
/// This is one argv element (no shell), so quoting/injection is not the risk —
/// a malformed value is. Require an absolute path, and reject control
/// characters, which would corrupt the argument and produce a baffling sshfs
/// error rather than a clear one here.
fn validate_remote_path(path: &str) -> Result<()> {
    if !path.starts_with('/') {
        return Err(Error::BadParams(format!(
            "remote path must be absolute (start with '/'), got {path:?}"
        )));
    }
    if path.chars().any(|c| c.is_control()) {
        return Err(Error::BadParams(
            "remote path must not contain control characters".into(),
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// host_mounts / host_mount_repair — several folders at once, and un-wedging
// ---------------------------------------------------------------------------

/// Root under which every sshfs mount lives: `~/Mounts`.
fn mounts_root() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    std::path::PathBuf::from(home).join("Mounts")
}

/// Where a given (host, remote path) is mounted: `~/Mounts/<host>/<slug>`.
///
/// The old layout mounted a host at `~/Mounts/<host>` itself, which allowed
/// exactly ONE mount per host — you could pin five folders and still only reach
/// one at a time. Nesting under the host directory lifts that limit while
/// keeping everything for a host in one place in Finder.
fn mount_point_for(host: &str, remote_path: &str) -> std::path::PathBuf {
    mounts_root()
        .join(host)
        .join(a2fa_core::mounts::slug_for(remote_path))
}

/// List what is actually mounted, read from the kernel mount table.
///
/// Never stats a mount point: a wedged macFUSE mount would block that forever,
/// and this is exactly what the UI calls while the user is trying to fix one.
pub fn host_mounts(state: &Arc<Mutex<State>>, params: &Value) -> Result<Value> {
    let root = mounts_root();
    let all = a2fa_core::mounts::list_active_mounts(&root);

    // Optional host filter; without it, report everything (the app renders the
    // whole set in one pass rather than one RPC per host).
    let filter = opt_str(params, "host")?;
    if let Some(h) = filter {
        if !a2fa_core::model::is_safe_host_name(h) {
            return Err(Error::BadParams("invalid host name".into()));
        }
    }

    let mounts: Vec<Value> = all
        .iter()
        .filter(|m| filter.is_none_or(|h| m.host == h))
        .map(|m| {
            json!({
                "host": m.host,
                "mount_point": m.mount_point.to_string_lossy(),
                "source": m.source,
                // A legacy single-mount has no slug; flag it so the UI can
                // explain why it cannot sit alongside others.
                "legacy": m.slug.is_empty(),
            })
        })
        .collect();

    // Keep State's coarse is_mounted flag honest with reality — an externally
    // unmounted volume (Finder eject, network drop) otherwise left the row
    // claiming a mount that no longer exists.
    {
        let mounted_hosts: HashSet<&str> = all.iter().map(|m| m.host.as_str()).collect();
        let mut guard = crate::lock_state(state);
        for h in guard.hosts.iter_mut() {
            h.is_mounted = mounted_hosts.contains(h.host.as_str());
        }
    }

    Ok(json!({ "mounts": mounts }))
}

/// Force-unmount a wedged mount and clean up after it.
///
/// After a network drop macFUSE can leave a mount that is still in the mount
/// table but whose every I/O hangs — Finder beachballs on it and a normal
/// `umount` will not shift it. There is no safe way to DETECT that state
/// (detecting it means touching the mount, which is what hangs), so this is an
/// explicit user action: they can see Finder hanging, and this is the button
/// that fixes it.
///
/// Reuses `reap_failed_sshfs`, which force-unmounts, kills the orphaned macFUSE
/// backend for that exact mount point, and removes the empty directory.
pub fn host_mount_repair(state: &Arc<Mutex<State>>, params: &Value) -> Result<Value> {
    let host_name = host_param(params)?;

    // Repair every mount for the host, or just one if a path is given.
    let only_point = opt_str(params, "mount_point")?;
    let root = mounts_root();
    let targets: Vec<std::path::PathBuf> = a2fa_core::mounts::list_active_mounts(&root)
        .into_iter()
        .filter(|m| m.host == host_name)
        .filter(|m| only_point.is_none_or(|p| m.mount_point.to_string_lossy() == p))
        .map(|m| m.mount_point)
        .collect();

    if targets.is_empty() {
        return Ok(json!({ "host": host_name, "repaired": 0,
                          "reason": "nothing is mounted for this host" }));
    }

    // The per-host mount latch keeps this from racing a mount/unmount.
    {
        let mut inflight = mount_in_flight().lock().unwrap_or_else(|e| e.into_inner());
        if !inflight.insert(host_name.clone()) {
            return Err(Error::Internal(format!(
                "mount/unmount already in progress for {host_name}"
            )));
        }
    }
    let _guard = MountInFlightGuard { host: host_name.clone() };

    for mp in &targets {
        log::info!("[{host_name}] repairing mount at {}", mp.display());
        reap_failed_sshfs(mp);
    }

    // Re-read the table: anything still listed did not come loose.
    let still = a2fa_core::mounts::list_active_mounts(&root)
        .into_iter()
        .filter(|m| targets.contains(&m.mount_point))
        .count();

    {
        let mut guard = crate::lock_state(state);
        if let Some(h) = guard.hosts.iter_mut().find(|h| h.host == host_name) {
            h.is_mounted = still > 0;
            h.last_msg = if still == 0 { "Mounts repaired" } else { "Repair incomplete" }.into();
        }
    }

    Ok(json!({
        "host": host_name,
        "repaired": targets.len() - still,
        "still_mounted": still,
    }))
}

// ---------------------------------------------------------------------------
// host_rotate
// ---------------------------------------------------------------------------

/// Manual rotation is a **no-op** in the single-master model — there is no
/// spare slot to rotate to. Retained so the IPC surface (and any older client)
/// keeps working instead of erroring on an unknown method.
pub fn host_rotate(
    state: &Arc<Mutex<State>>,
    params: &Value,
    _managers: Option<Arc<HostManagers>>,
) -> Result<Value> {
    let host_name = params["host"]
        .as_str()
        .ok_or_else(|| Error::BadParams("host required".into()))?;

    // Verify the host is active (keeps the old error contract for a bad host).
    {
        let guard = crate::lock_state(state);
        guard
            .hosts
            .iter()
            .find(|h| h.host == host_name && h.active)
            .ok_or_else(|| Error::NotFound("host not active".into()))?;
    }

    log::info!("[{host_name}] host_rotate is a no-op (single master)");
    Ok(Value::Null)
}

// ---------------------------------------------------------------------------
// host_add
// ---------------------------------------------------------------------------

/// Validate a host name — delegates to the canonical
/// [`a2fa_core::model::is_safe_host_name`] so the add-time guard and the
/// State-load filter share ONE definition (no drift).
fn valid_host_name(host: &str) -> bool {
    a2fa_core::model::is_safe_host_name(host)
}

/// Add a host: validate name, extract secret, write Keychain + passwords.json,
/// add to State, and optionally spawn a master-start.
///
/// Mirrors `_add_host_persistent` + the `HOST_ADD` handler in daemon.py.
pub fn host_add(
    state: &Arc<Mutex<State>>,
    params: &Value,
    managers: Option<Arc<HostManagers>>,
    registry: Option<Arc<OtpRegistry>>,
) -> Result<Value> {
    let host_name = params["host"]
        .as_str()
        .ok_or_else(|| Error::BadParams("host required".into()))?
        .to_owned();

    if !valid_host_name(&host_name) {
        return Err(Error::BadParams(
            "invalid host name (letters, digits, '.', '-', '_' only; no '/' or '..')".into(),
        ));
    }

    let password = params
        .get("password")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned();

    let otpauth_url = params
        .get("otpauth_url")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_owned();

    let auto_connect = params
        .get("auto_connect")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // Extract the TOTP secret from the URL (validates the URL format).
    //
    // An EMPTY otpauth_url is legitimate: plenty of servers authenticate with a
    // password alone. Such a host is stored with an empty secret, and the login
    // path never calls the OTP provider because the server never prints an OTP
    // prompt. (If one ever does, the provider fails with an actionable
    // "no 2FA secret is saved" message instead of hanging.)
    let secret = extract_secret_optional(&otpauth_url)
        .map_err(|e| Error::BadParams(format!("invalid otpauth URL: {e}")))?
        .unwrap_or_default();

    // Check for duplicates before doing any I/O. A genuinely first host writes
    // directly into the stable store, so it needs no legacy-upgrade gate.
    let is_first_host = {
        let guard = crate::lock_state(state);
        if guard.hosts.iter().any(|h| h.host == host_name) {
            return Err(Error::Duplicate(format!("host {host_name} already exists")));
        }
        guard.hosts.is_empty()
    };

    // Write credentials to the Keychain on a BOUNDED WORKER thread — never on
    // this connection-handler thread. macOS serializes Keychain access
    // process-wide; with the login Keychain locked (post-reboot / password
    // change), SecItemAdd blocks on a SecurityAgent prompt and an inline call
    // would wedge this handler forever AND stall every other Keychain user
    // (login workers, host_totp) behind it. Same pattern as host_totp below:
    // worker + recv_timeout. An abandoned worker that completes late is
    // harmless — the creds it stores are exactly what a retry would store.
    {
        let (tx, rx) = std::sync::mpsc::channel();
        let host_owned = host_name.clone();
        let password_owned = password.clone();
        let otpauth_owned = otpauth_url.clone();
        let spawn_res = std::thread::Builder::new()
            .name(format!("host_add-keychain:{host_name}"))
            .spawn(move || {
                let ks = platform_store();
                let result = store_credentials(&ks, &host_owned, &password_owned, &otpauth_owned);
                let _ = tx.send(result);
            });
        if let Err(e) = spawn_res {
            log::warn!("host_add: failed to spawn keychain worker for {host_name}: {e}");
            return Err(Error::Internal(format!(
                "could not start credential store for {host_name} — try again"
            )));
        }
        match rx.recv_timeout(std::time::Duration::from_secs(10)) {
            Ok(inner) => inner?,
            Err(_) => {
                return Err(Error::Internal(
                    "Keychain write timed out (is the login Keychain locked?) — try again"
                        .into(),
                ));
            }
        }
    }
    if is_first_host {
        crate::managers::mark_credential_storage_ready();
    }
    // The stored creds just changed — drop any cached copy so the next login
    // re-reads the new secret instead of serving a stale one.
    crate::managers::invalidate_creds_cache(&host_name);

    // Update passwords.json metadata (serialized read-modify-write — a
    // concurrent host toggle on another handler thread must not be lost).
    let meta_path = passwords_path();
    if let Err(e) = update_meta(&meta_path, |meta| {
        meta.insert(host_name.clone(), HostMeta { auto_connect });
    }) {
        // Non-fatal: credentials are in Keychain; meta is cosmetic.
        log::warn!("host_add: failed to persist passwords.json: {e}");
    }

    // Add to in-memory State.
    let new_host = Host {
        host: host_name.clone(),
        status: "Idle".into(),
        active: auto_connect,
        is_master_ready: false,
        pool_index: 0,
        pool_alive: 0,
        is_mounted: false,
        last_msg: "Added".into(),
    };
    let snap = {
        let mut guard = crate::lock_state(state);
        let s = host_snapshot(&new_host);
        guard.hosts.push(new_host);
        s
    };

    // If auto_connect, kick off a master-start THROUGH the managed system:
    // the daemon-global OtpRegistry (so a shared TOTP secret is serialized
    // against other in-flight logins — a private registry could replay the
    // same code twice in one window) and HostManagers (so the heartbeat
    // health-checks/restarts this master; the legacy spawn_host_start wrote
    // only to State and left the registry slot Init = never monitored).
    if auto_connect {
        match (managers, registry) {
            (Some(mgrs), Some(reg)) => {
                spawn_managed_start(
                    host_name.clone(),
                    0,
                    Arc::clone(&reg),
                    Arc::clone(state),
                    Arc::clone(&mgrs),
                );
                let _ = (reg, mgrs); // single master — no slot-1 warm-up
            }
            _ => {
                // Legacy fallback (tests only — production dispatch always
                // passes both).
                let reg = OtpRegistry::new();
                spawn_host_start(
                    host_name.clone(),
                    0,
                    password,
                    secret,
                    reg,
                    Arc::clone(state),
                );
            }
        }
    }

    Ok(snap)
}

// ---------------------------------------------------------------------------
// host_test_credentials
// ---------------------------------------------------------------------------

/// Dry-run credential test — runs a one-shot ssh login to verify password +
/// OTP WITHOUT writing anything to disk or spawning a long-lived master.
///
/// Mirrors `_test_credentials` in daemon.py.  Spawns ssh with
/// `ControlMaster=no ControlPath=none` so it NEVER reuses an existing master
/// — this is the critical safety property that prevents a stale master from
/// silently returning "success" with wrong creds.
///
/// Returns `{"ok": bool, "reason": str}`.
///
/// NOTE: this runs synchronously in the handler thread.  In a full async
/// daemon it should be moved to a blocking thread; for the daemon's Tokio
/// runtime the caller wraps this in `spawn_blocking`.  As an IPC RPC it
/// is still acceptable to block briefly (the client has a generous timeout).
/// When BOTH `password` and `otpauth_url` are omitted, the host's **stored**
/// credentials are tested instead — that's how the app's per-host credential
/// view offers "Test login" without first pulling the secrets into the UI.
pub fn host_test_credentials(
    _state: &Arc<Mutex<State>>,
    params: &Value,
    registry: Option<Arc<OtpRegistry>>,
) -> Result<Value> {
    let host = params
        .get("host")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned();

    if host.is_empty() {
        return Ok(json!({ "ok": false, "reason": "host required" }));
    }
    // The host name flows into ssh argv (final arg) AND the temp log path
    // `auto2fa-testlogin-<host>-<pid>.log`. Reject unsafe names (leading dash =
    // ssh option injection; '/' or '..' = path traversal) — the Add-host
    // wizard sends a user-typed value here BEFORE the host_add guard runs.
    // Checked before the stored-credential fallback below, which uses the name
    // as a Keychain account key.
    if !valid_host_name(&host) {
        return Ok(json!({
            "ok": false,
            "reason": "invalid host name (use letters/digits/._- , not starting with '-' or '.')"
        }));
    }

    let (password, otpauth_url) = match creds_under_test(params) {
        Err(reason) => return Ok(json!({ "ok": false, "reason": reason })),
        Ok(CredsUnderTest::Supplied {
            password,
            otpauth_url,
        }) => (password, otpauth_url),
        // Nothing supplied → test what's stored for this host (bounded Keychain
        // read on a worker, never on this handler thread).
        Ok(CredsUnderTest::Stored) => {
            let host_owned = host.clone();
            let stored = run_keychain_bounded(
                "credential read",
                &host,
                CREDENTIAL_OP_TIMEOUT,
                move || {
                    let ks = platform_store();
                    Ok((
                        get_password(&ks, &host_owned)?.unwrap_or_default(),
                        get_otpauth(&ks, &host_owned)?.unwrap_or_default(),
                    ))
                },
            );
            match stored {
                Ok((p, o)) if !p.is_empty() || !o.trim().is_empty() => (p, o),
                Ok(_) => {
                    return Ok(json!({
                        "ok": false,
                        "reason": format!("no credentials stored for {host}")
                    }));
                }
                Err(e) => {
                    return Ok(json!({ "ok": false, "reason": e.to_string() }));
                }
            }
        }
    };

    // Validate the otpauth URL before attempting any ssh I/O. An EMPTY value
    // means "this host has no 2FA" (password-only login) — a supported setup,
    // so it must not be rejected as a malformed URL. The closure below still
    // fails loudly if such a server does prompt for a code.
    let otpauth_url = otpauth_url.trim().to_owned();
    let secret = match extract_secret_optional(&otpauth_url) {
        Ok(s) => s.unwrap_or_default(),
        Err(e) => {
            return Ok(json!({
                "ok": false,
                "reason": format!("invalid otpauth URL: {e}")
            }));
        }
    };

    // Run the one-shot login attempt on this thread.
    // (In production the daemon server wraps handlers in a worker pool.)
    //
    // OTP source: when the daemon-global registry is available (production
    // dispatch always passes it), generate codes THROUGH the replay guard. A
    // bare `totp_now` here could submit the exact code an in-flight managed
    // login (same shared Duo secret) just used — the server rejects the
    // replay and the REAL login fails, bumping its circuit breaker.
    let (ok, reason) = match registry {
        Some(reg) => {
            let otp_fn = crate::workers::make_otp_closure(secret.clone(), host.clone(), reg);
            test_login(&host, &password, otp_fn)
        }
        None => {
            // Legacy fallback (tests only).
            let secret_owned = secret.clone();
            test_login(&host, &password, move || {
                if secret_owned.is_empty() {
                    return Err(a2fa_core::error::Error::BadParams(
                        crate::workers::NO_OTP_SECRET.into(),
                    ));
                }
                a2fa_core::totp::totp_now(&secret_owned)
            })
        }
    };
    Ok(json!({ "ok": ok, "reason": reason }))
}

/// Which credentials a dry-run should exercise.
enum CredsUnderTest {
    /// Read the host's saved credentials and test those.
    Stored,
    /// Test exactly these. An empty `otpauth_url` is a password-only login.
    Supplied {
        password: String,
        otpauth_url: String,
    },
}

/// Decide what [`host_test_credentials`] should test, from its params alone.
///
/// Pure (no Keychain, no ssh) so the three modes are unit-testable; `Err` is
/// the `reason` string to hand back with `ok: false`.
///
/// Supplying exactly ONE of the pair is refused rather than defaulted. Before
/// password-only hosts existed, an omitted `otpauth_url` was caught for free by
/// the URL parse; now an empty secret is a MEANINGFUL value ("no 2FA"), so
/// defaulting the missing half would quietly test something other than what the
/// caller asked about — and neither may be silently back-filled from the store,
/// which would let a half-supplied test appear to pass on saved credentials.
fn creds_under_test(params: &Value) -> std::result::Result<CredsUnderTest, String> {
    let password = params.get("password").and_then(|v| v.as_str());
    let otpauth = params.get("otpauth_url").and_then(|v| v.as_str());
    match (password, otpauth) {
        (None, None) => Ok(CredsUnderTest::Stored),
        (Some(password), Some(otpauth_url)) => Ok(CredsUnderTest::Supplied {
            password: password.to_owned(),
            otpauth_url: otpauth_url.to_owned(),
        }),
        _ => Err("send both 'password' and 'otpauth_url' (use \"\" for a host with no 2FA), \
                  or neither to test the stored credentials"
            .into()),
    }
}

/// Attempt a one-shot, isolated SSH login.
///
/// Uses `a2fa_core::ssh::pty_auth::run_login` with a temporary ControlPath so
/// there is no interaction with the live master pool. The OTP closure is
/// supplied by the caller (routed through the daemon-global replay guard in
/// production).
///
/// Returns `(true, "")` on success or `(false, reason)` on failure.
fn test_login(
    host: &str,
    password: &str,
    otp_fn: impl Fn() -> a2fa_core::error::Result<String>,
) -> (bool, String) {
    use a2fa_core::ssh::pty_auth::{run_login, LoginOutcome};

    // Build a temp log path.
    let tmp_dir = std::env::temp_dir();
    // (No ControlPath needed — test login uses ControlPath=none.)

    // Build argv exactly like _test_credentials in daemon.py:
    // -v -E <log> -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null
    // -o ConnectTimeout=10 -o PreferredAuthentications=keyboard-interactive,password
    // -o ControlMaster=no -o ControlPath=none <host> echo __auto2fa_login_ok__
    let log_path = tmp_dir.join(format!("auto2fa-testlogin-{host}-{}.log", std::process::id()));
    // Pre-create the verbose log at 0600 so it's never world-readable even if the
    // best-effort cleanup below is interrupted. It carries host/user/cipher
    // metadata (NOT credentials — those go via the PTY), but keep it private.
    // ssh -E APPENDS to the existing file, so our empty 0600 file keeps its mode.
    {
        use std::os::unix::fs::OpenOptionsExt;
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(&log_path);
    }
    let mut argv: Vec<String> = a2fa_core::config::paths::managed_config_args();
    argv.extend([
        "-v".into(),
        "-E".into(), log_path.to_string_lossy().into_owned(),
        "-o".into(), "StrictHostKeyChecking=no".into(),
        "-o".into(), "UserKnownHostsFile=/dev/null".into(),
        "-o".into(), "ConnectTimeout=10".into(),
        "-o".into(), "PreferredAuthentications=keyboard-interactive,password".into(),
        // CRITICAL: disable master reuse so the test actually tests the supplied creds.
        "-o".into(), "ControlMaster=no".into(),
        "-o".into(), "ControlPath=none".into(),
        host.into(),
        // run_login matches this marker as its command-mode success signal.
        "echo".into(), a2fa_core::ssh::pty_auth::LOGIN_OK_MARKER.into(),
    ]);

    let result = run_login(&argv, password, otp_fn);
    let ssh_log = std::fs::read_to_string(&log_path).unwrap_or_default();

    // Clean up temp files (surface a failure instead of silently leaving it).
    if let Err(e) = std::fs::remove_file(&log_path) {
        log::warn!("test-login: could not remove {log_path:?}: {e}");
    }

    match result {
        Ok(LoginOutcome::Success) => (true, String::new()),
        Ok(LoginOutcome::AuthFailed { reason }) => (
            false,
            a2fa_core::ssh::failure::actionable_failure(&reason),
        ),
        Ok(LoginOutcome::Timeout { output }) => {
            let detail = if output.trim().is_empty() && ssh_log.trim().is_empty() {
                "Connection timed out".into()
            } else {
                format!(
                    "Login timed out; last SSH message: {}",
                    a2fa_core::ssh::failure::failure_reason_from_sources(
                        &output,
                        &ssh_log,
                    )
                )
            };
            (false, a2fa_core::ssh::failure::actionable_failure(&detail))
        }
        Ok(LoginOutcome::Eof { output }) => {
            let reason = a2fa_core::ssh::failure::failure_reason_from_sources(
                &output,
                &ssh_log,
            );
            (false, a2fa_core::ssh::failure::actionable_failure(&reason))
        }
        Err(e) => (
            false,
            a2fa_core::ssh::failure::actionable_failure(&e.to_string()),
        ),
    }
}

// ---------------------------------------------------------------------------
// host_totp
// ---------------------------------------------------------------------------

/// Daemon-global set of `"<op>:<host>"` keys with a Keychain operation in
/// flight.
///
/// macOS serializes Keychain access process-wide, so a hung "Always Allow"
/// prompt blocks the worker thread until it is answered (~30 s from the app's
/// poll rollover). Without a guard, every IPC call for that host would spawn
/// another worker that immediately blocks behind the same prompt — one leaked
/// thread per call. The latch caps it to AT MOST one in-flight worker per
/// (operation, host); concurrent callers get a "busy" error and retry.
///
/// The key includes the OPERATION so the app's rotating-code polling
/// (`host_totp`) can never make an unrelated credential read/write report
/// "busy" for the same host.
fn keychain_in_flight() -> &'static Mutex<HashSet<String>> {
    static IN_FLIGHT: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    IN_FLIGHT.get_or_init(|| Mutex::new(HashSet::new()))
}

/// RAII guard releasing a `keychain_in_flight` entry on every exit path
/// (worker completion or panic). Mirrors `StartGuard` in managers.rs.
struct KeychainInFlightGuard {
    key: String,
}

impl Drop for KeychainInFlightGuard {
    fn drop(&mut self) {
        keychain_in_flight()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.key);
    }
}

/// Deadline for a USER-INITIATED credential read/write (the app's per-host
/// "Password & setup" view).
///
/// Deliberately generous. Measured on a real install: the FIRST Keychain read of
/// a given account after a daemon restart can take well over 10s (macOS
/// re-evaluates the item's ACL against the new process), while later reads are
/// instant. A 10s bound turned that into a spurious "timed out — try again" on
/// the first open of the sheet for each host. The in-flight latch — not the
/// deadline — is what prevents a thread pile-up behind a hung prompt, so waiting
/// longer here is safe: at most ONE worker per (operation, host) can ever exist,
/// and the only thread that waits is the one connection handler that asked.
const CREDENTIAL_OP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// THE chokepoint for every Keychain touch made from an IPC handler thread.
///
/// Runs `f` on a short-lived worker thread and joins it with a hard deadline,
/// while holding a per-(op, host) in-flight latch. This is required — not
/// optional — because:
/// * Keychain reads/writes MUST NOT run on the connection-handler thread: a
///   locked login Keychain blocks on a SecurityAgent prompt, which would wedge
///   the handler (and, since macOS serializes Keychain access process-wide,
///   stall every other Keychain user behind it).
/// * The latch prevents piling up one leaked thread per retry behind that same
///   prompt (the resource-exhaustion class that previously hung the machine).
///
/// On timeout the latch stays held until the abandoned worker actually finishes
/// — deliberately, so retries return "busy" instead of spawning more threads.
fn run_keychain_bounded<T, F>(
    op: &str,
    host: &str,
    timeout: std::time::Duration,
    f: F,
) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    let key = format!("{op}:{host}");
    {
        let mut inflight = keychain_in_flight().lock().unwrap_or_else(|e| e.into_inner());
        if !inflight.insert(key.clone()) {
            return Err(Error::Internal(format!(
                "{op} already in flight for {host} — try again"
            )));
        }
    }

    let (tx, rx) = std::sync::mpsc::channel();
    let worker_key = key.clone();
    // Builder::spawn + captured Result: a thread-creation failure (EAGAIN under
    // thread exhaustion) must not panic AND must not wedge the latch — the
    // guard only runs once the closure starts, so release it here on that path.
    let spawn_res = std::thread::Builder::new()
        .name(format!("{op}:{host}"))
        .spawn(move || {
            // Release the latch BEFORE unblocking the caller: if the guard
            // outlived the send, a caller that immediately retried (or a second
            // client) could see a spurious "already in flight" for a call that
            // had actually finished. The inner scope also releases it on panic.
            let result = {
                let _inflight_guard = KeychainInFlightGuard { key: worker_key };
                f()
            };
            let _ = tx.send(result);
        });
    if let Err(e) = spawn_res {
        keychain_in_flight()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&key);
        log::warn!("failed to spawn {op} worker for {host}: {e}");
        return Err(Error::Internal(format!(
            "{op} could not start for {host} — try again"
        )));
    }

    match rx.recv_timeout(timeout) {
        Ok(inner) => inner,
        // Disconnected = the worker died without sending (a panic inside `f`).
        // Distinguish it from the deadline so the log says what happened.
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err(Error::Internal(format!(
            "{op} failed unexpectedly for {host} — try again"
        ))),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(Error::Internal(format!(
            "{op} timed out for {host} (is the login Keychain locked?) — try again"
        ))),
    }
}

/// Compute the current 6-digit TOTP code for a host, for live display in the
/// app (authenticator-style rotating code).
///
/// READ-ONLY: this only computes the code that the user's authenticator would
/// currently show. It has NO side effects — it does not consume, submit, or
/// replay-guard the OTP (that registry path is reserved for the login flow).
/// It returns ONLY the code + timing and NEVER the secret.
///
/// Returns `{ "code": "123456", "period": 30, "seconds_remaining": <1..=30> }`.
pub fn host_totp(state: &Arc<Mutex<State>>, params: &Value) -> Result<Value> {
    let host_name = params["host"]
        .as_str()
        .ok_or_else(|| Error::BadParams("host required".into()))?
        .to_owned();

    // Verify the host exists in State.
    {
        let guard = crate::lock_state(state);
        if !guard.hosts.iter().any(|h| h.host == host_name) {
            return Err(Error::NotFound(format!("host {host_name}")));
        }
    }

    // INVARIANT (see crate::managers::load_creds, ~lines 59-67): Keychain reads
    // MUST NOT happen on a shared/handler thread — `run_keychain_bounded` is the
    // chokepoint that enforces the worker + hard timeout + per-host in-flight
    // latch. Do NOT log the code or the secret.
    let host_owned = host_name.clone();
    let (code, period, remaining) = run_keychain_bounded(
        "totp read",
        &host_name,
        std::time::Duration::from_secs(5),
        move || {
            let otpauth = get_otpauth(&platform_store(), &host_owned)?
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| Error::NotFound(format!("no 2FA secret for {host_owned}")))?;
            totp_now_detailed(&otpauth)
        },
    )?;

    Ok(json!({
        "code": code,
        "period": period,
        "seconds_remaining": remaining,
    }))
}

// ---------------------------------------------------------------------------
// credentials_consolidate — collapse per-host Keychain items into one
// ---------------------------------------------------------------------------

/// Fold every host's legacy per-host Keychain items into the single vault item.
///
/// WHY this is worth a dedicated method: older releases stored two independently
/// protected items per host. A six-host install could therefore surface twelve
/// dialogs during a signing-identity migration. After this runs, every host is
/// in one item owned by the daemon's stable identity, so later updates are quiet.
///
/// It is deliberately explicit rather than automatic at boot: the migration
/// itself must read all the old items, which costs the old per-item prompts one
/// last time. That belongs behind a button the user pressed, with an
/// explanation, not a mystery burst of dialogs at launch.
pub fn credentials_consolidate(state: &Arc<Mutex<State>>, _params: &Value) -> Result<Value> {
    let _upgrade_guard = crate::managers::try_begin_credential_upgrade().ok_or_else(|| {
        Error::Internal("The one-time secure storage update is already running".into())
    })?;
    let hosts: Vec<String> = {
        let guard = crate::lock_state(state);
        guard.hosts.iter().map(|h| h.host.clone()).collect()
    };

    // Bounded worker, like every other Keychain touch. Generous deadline: this
    // reads up to 2N items and may sit behind N authorization prompts.
    let hosts_for_worker = hosts.clone();
    let report = run_keychain_bounded(
        "credential consolidation",
        "all-hosts",
        std::time::Duration::from_secs(180),
        move || a2fa_core::creds::vault::migrate_to_vault(&platform_store(), &hosts_for_worker),
    )?;
    crate::managers::mark_credential_storage_ready();

    // Cached creds are still valid (same secrets, new location), but drop them
    // anyway so the next read proves the vault works.
    for h in &hosts {
        crate::managers::invalidate_creds_cache(h);
    }

    log::info!(
        "credential consolidation: {} migrated, {} already in the vault, {} with nothing stored",
        report.migrated, report.already, report.missing
    );
    Ok(json!({
        "migrated": report.migrated,
        "already": report.already,
        "missing": report.missing,
        "total_hosts": hosts.len(),
    }))
}

// ---------------------------------------------------------------------------
// host_list_dir — browse remote folders for the mount picker
// ---------------------------------------------------------------------------

/// List the directories inside a remote path, over the host's existing master.
///
/// Exists so pinning a mount folder is a matter of BROWSING to it rather than
/// typing an absolute path from memory. Runs no login of its own: it reuses the
/// warm ControlMaster, so it costs no 2FA and fails fast if the master is down.
pub fn host_list_dir(state: &Arc<Mutex<State>>, params: &Value) -> Result<Value> {
    let host_name = host_param(params)?;

    // Require a ready master — without one this would try to open a NEW ssh
    // connection (and a 2FA prompt) just to fill a folder picker.
    {
        let guard = crate::lock_state(state);
        let host = guard
            .hosts
            .iter()
            .find(|h| h.host == host_name)
            .ok_or_else(|| Error::NotFound(host_name.clone()))?;
        if !host.is_master_ready {
            return Err(Error::Internal(format!(
                "{host_name} isn't connected — connect it first to browse its folders"
            )));
        }
    }

    let path = opt_str(params, "path")?
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or("/")
        .to_owned();
    validate_remote_path(&path)?;

    let cp = a2fa_core::ssh::control::active_symlink_path(&host_name);
    let entries = a2fa_core::tunnels::discovery::list_remote_dirs(&host_name, &cp, &path)
        .map_err(|e| Error::Internal(e.to_string()))?;

    let dirs: Vec<Value> = entries
        .iter()
        .filter(|e| e.is_dir)
        .map(|e| {
            // Join without doubling the slash at root.
            let full = if path == "/" {
                format!("/{}", e.name)
            } else {
                format!("{}/{}", path, e.name)
            };
            json!({ "name": e.name, "path": full })
        })
        .collect();

    Ok(json!({
        "host": host_name,
        "path": path,
        "entries": dirs,
    }))
}

// ---------------------------------------------------------------------------
// host_remove — the missing other half of host_add
// ---------------------------------------------------------------------------

/// Drop `host` from a tunnel's pinned jump-host list.
///
/// Returns the new value: `None` means "any ready host", which is what an
/// emptied list must become. A tunnel pinned ONLY to a host that is then
/// removed would otherwise wait forever — the jump lookup finds no matching
/// ready host, so the tunnel parks at "waiting for jump host" and never even
/// attempts a start, which means the recovery-failure auto-stop never fires
/// either. Falling back to "any ready host" keeps it working instead of
/// stranding it.
pub(crate) fn jump_candidates_without(
    candidates: &Option<Vec<String>>,
    host: &str,
) -> Option<Vec<String>> {
    let list = candidates.as_ref()?;
    if !list.iter().any(|c| c == host) {
        return candidates.clone();
    }
    let kept: Vec<String> = list.iter().filter(|c| c.as_str() != host).cloned().collect();
    if kept.is_empty() { None } else { Some(kept) }
}

/// Deregister a host completely: stop its master, delete both Keychain entries,
/// drop it from passwords.json, and remove it from State.
///
/// Until this existed a host could be added but never removed — a decommissioned
/// cluster kept its entry, kept being retried, and kept its password + TOTP
/// secret in the Keychain forever. The only escape was `brew uninstall --zap`.
/// (`delete_credentials` already existed in core but was reachable only from the
/// migration path.)
///
/// Deleting credentials is irreversible, so this is deliberately explicit: the
/// caller passes the host name and the app confirms first.
///
/// Ordering matters. The master is stopped FIRST (an orphaned ControlMaster with
/// no owning entry can never be adopted or cleaned up again), then the Keychain
/// entries go, then the on-disk metadata, then State. A failure to delete the
/// Keychain entries is logged but does NOT abort the removal: leaving the host
/// registered because its secrets could not be deleted is the worse outcome —
/// the user asked for it gone, and stale Keychain items are visible/removable in
/// Keychain Access.
pub fn host_remove(
    state: &Arc<Mutex<State>>,
    params: &Value,
    managers: Option<Arc<HostManagers>>,
) -> Result<Value> {
    let host_name = host_param(params)?;
    require_host(state, &host_name)?;

    // 1. Mark inactive so the heartbeat stops trying to restart it while we
    //    tear down.
    {
        let mut guard = crate::lock_state(state);
        if let Some(h) = guard.hosts.iter_mut().find(|h| h.host == host_name) {
            h.active = false;
            h.last_msg = "Removing…".into();
        }
    }

    // 2. Unmount BEFORE stopping the master, while the connection can still
    //    carry a clean unmount.
    //
    //    Skipping this was actively harmful, not merely untidy: the sshfs mount
    //    outlives the host entry, the master it rides is torn down a moment
    //    later, and the result is a WEDGED mount that hangs Finder — with the
    //    host row now gone, leaving no way to unmount it from the app at all.
    let mounts = a2fa_core::mounts::list_active_mounts(&mounts_root());
    for m in mounts.iter().filter(|m| m.host == host_name) {
        log::info!("[{host_name}] unmounting {} before removal", m.mount_point.display());
        let mp = m.mount_point.to_string_lossy().into_owned();
        let _ = force_unmount(&mp);
    }
    // Anything that refused to unmount gets the full reap (kills the orphaned
    // macFUSE backend) — a removed host must never leave a wedged mount behind.
    let still: Vec<_> = a2fa_core::mounts::list_active_mounts(&mounts_root())
        .into_iter()
        .filter(|m| m.host == host_name)
        .collect();
    for m in &still {
        log::warn!("[{host_name}] {} did not unmount cleanly — reaping", m.mount_point.display());
        reap_failed_sshfs(&m.mount_point);
    }
    let _ = std::fs::remove_dir(mounts_root().join(&host_name));

    // 3. Now stop the master.
    let managers_for_cleanup = managers.clone();
    if let Some(mgrs) = managers {
        spawn_managed_stop(host_name.clone(), Arc::clone(state), mgrs);
    }

    // 4. Delete both Keychain entries on the bounded worker (never inline).
    let host_owned = host_name.clone();
    let cred_result = run_keychain_bounded(
        "credential delete",
        &host_name,
        CREDENTIAL_OP_TIMEOUT,
        move || delete_credentials(&platform_store(), &host_owned),
    );
    let credentials_deleted = match cred_result {
        Ok(()) => true,
        Err(e) => {
            log::warn!(
                "[{host_name}] could not delete Keychain credentials during removal: {e} \
                 (removing the host anyway; the entries remain in Keychain Access)"
            );
            false
        }
    };
    // Whatever happened above, never serve cached secrets for a removed host.
    crate::managers::invalidate_creds_cache(&host_name);

    // 5. Drop the passwords.json entry (serialized read-modify-write).
    if let Err(e) = update_meta(&passwords_path(), |meta| {
        meta.remove(&host_name);
    }) {
        log::warn!("[{host_name}] could not update passwords.json during removal: {e}");
    }

    // 6. Drop it from State so it disappears from list_hosts immediately, and
    //    release any tunnel that was pinned to it (see
    //    `jump_candidates_without` — a tunnel pinned only to this host would
    //    otherwise wait for a jump host that can never appear).
    let released: Vec<String> = {
        let mut guard = crate::lock_state(state);
        guard.hosts.retain(|h| h.host != host_name);
        let mut released = Vec::new();
        for t in guard.tunnels.iter_mut() {
            let updated = jump_candidates_without(&t.jump_candidates, &host_name);
            if updated != t.jump_candidates {
                t.jump_candidates = updated;
                t.last_msg = format!("jump host '{host_name}' was removed — using any ready host");
                released.push(t.name.clone());
            }
        }
        released
    };
    // Drop the daemon's per-host state too. Keyed by NAME, so without this a
    // host added back under the same name inherits the removed one's circuit
    // breaker (see HostManagers::forget) and silently refuses to connect.
    if let Some(mgrs) = &managers_for_cleanup {
        mgrs.forget(&host_name);
    }

    if !released.is_empty() {
        log::info!(
            "[{host_name}] released {} tunnel(s) pinned to this host: {}",
            released.len(),
            released.join(", ")
        );
        crate::handlers::tunnels::persist_tunnels(state);
    }

    log::info!("[{host_name}] host removed (credentials_deleted={credentials_deleted})");
    Ok(json!({
        "ok": true,
        "host": host_name,
        "credentials_deleted": credentials_deleted,
    }))
}

// ---------------------------------------------------------------------------
// host_credentials — describe what's stored, WITHOUT revealing it
// ---------------------------------------------------------------------------

/// Confirm the host exists in State, returning a `NotFound` otherwise.
fn require_host(state: &Arc<Mutex<State>>, host: &str) -> Result<()> {
    let guard = crate::lock_state(state);
    if guard.hosts.iter().any(|h| h.host == host) {
        Ok(())
    } else {
        Err(Error::NotFound(format!("host {host}")))
    }
}

/// An OPTIONAL string parameter — absent is fine, present-but-wrong-type is not.
///
/// `params.get(k).and_then(|v| v.as_str())` silently maps a number, array or
/// object to `None`, i.e. "not provided". For a parameter that SELECTS what an
/// action operates on, that turns a malformed request into a different action:
/// `host_mount_toggle {"remote_path": 123}` fell back to "/" and mounted the
/// filesystem root the caller never asked for, and a wrong-typed `mount_point`
/// stopped addressing an existing mount and toggled one instead. Found by
/// fuzzing the live IPC surface.
fn opt_str<'a>(params: &'a Value, key: &str) -> Result<Option<&'a str>> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.as_str())),
        Some(other) => Err(Error::BadParams(format!(
            "{key} must be a string, got {}",
            match other {
                Value::Number(_) => "a number",
                Value::Bool(_) => "a boolean",
                Value::Array(_) => "an array",
                Value::Object(_) => "an object",
                _ => "another type",
            }
        ))),
    }
}

/// An OPTIONAL boolean parameter — absent is `false`, present-but-wrong-type is
/// an error.
///
/// Same reasoning as [`opt_str`]: a flag that decides whether a stored secret is
/// DELETED must not read `{"clear_otp_secret": "yes"}` as "no" (or, worse, some
/// future truthiness rule as "yes"). Malformed input gets rejected, not
/// reinterpreted.
fn opt_bool(params: &Value, key: &str) -> Result<bool> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(Error::BadParams(format!("{key} must be a boolean"))),
    }
}

/// Read the `host` param, validating it as a safe host name.
///
/// The name is used as a Keychain account key and (for the test login) as ssh
/// argv, so the same guard `host_add` applies on the way in is re-applied here
/// — these methods take a client-supplied string.
fn host_param(params: &Value) -> Result<String> {
    let host = params["host"]
        .as_str()
        .ok_or_else(|| Error::BadParams("host required".into()))?
        .to_owned();
    if !valid_host_name(&host) {
        return Err(Error::BadParams(
            "invalid host name (letters, digits, '.', '-', '_' only; no '/' or '..')".into(),
        ));
    }
    Ok(host)
}

/// Describe the credentials stored for a host **without returning any secret**.
///
/// This is what the app's per-host "Password & setup" view loads to show
/// *whether* a password and a 2FA secret exist, how long the password is, and
/// which account the 2FA secret belongs to (issuer / account / period). Safe to
/// call for plain display — see `host_reveal_credentials` for the gated path
/// that returns the actual secrets.
pub fn host_credentials(state: &Arc<Mutex<State>>, params: &Value) -> Result<Value> {
    let host_name = host_param(params)?;
    require_host(state, &host_name)?;

    // Keychain read on the bounded worker (never this handler thread).
    let host_owned = host_name.clone();
    let (password, otpauth) = run_keychain_bounded(
        "credential read",
        &host_name,
        CREDENTIAL_OP_TIMEOUT,
        move || {
            let ks = platform_store();
            let password = get_password(&ks, &host_owned)?.unwrap_or_default();
            let otpauth = get_otpauth(&ks, &host_owned)?.unwrap_or_default();
            Ok((password, otpauth))
        },
    )?;

    // The persisted auto-connect intent (file read — no Keychain, cheap).
    let auto_connect = load_meta(&passwords_path())
        .get(&host_name)
        .map(|m| m.auto_connect)
        .unwrap_or(false);

    // Describe the 2FA secret. A stored secret that no longer parses is
    // reported as an error string rather than silently looking absent — that's
    // the difference between "add a secret" and "your secret is corrupt".
    let mut otp = json!({});
    let mut otp_error = Value::Null;
    if !otpauth.trim().is_empty() {
        match describe_otp(&otpauth) {
            Ok(d) => {
                otp = json!({
                    "issuer": d.issuer,
                    "account": d.account,
                    "algorithm": d.algorithm,
                    "digits": d.digits,
                    "period": d.period,
                });
            }
            Err(e) => otp_error = json!(e.to_string()),
        }
    }

    Ok(json!({
        "host": host_name,
        "has_password": !password.is_empty(),
        // Length only — enough for the UI to render a realistic dot mask and to
        // tell "nothing stored" from "stored", without leaking the value.
        "password_length": password.chars().count(),
        "has_otp_secret": !otpauth.trim().is_empty(),
        "otp": otp,
        "otp_error": otp_error,
        "auto_connect": auto_connect,
    }))
}

// ---------------------------------------------------------------------------
// host_reveal_credentials — the explicit, audited secret-returning path
// ---------------------------------------------------------------------------

/// Return the stored password and otpauth URL for a host **in plaintext**.
///
/// Deliberately a separate method from [`host_credentials`] so no display path
/// can return secrets by accident: a client must ask for them by name. The app
/// gates this behind device-owner authentication (Touch ID / login password)
/// before calling it, and the call is logged (host only — never the values) so
/// a reveal is visible in the daemon log.
///
/// The socket is already owner-only and `host_add` accepts the same secrets over
/// it, so this adds no new transport exposure.
pub fn host_reveal_credentials(state: &Arc<Mutex<State>>, params: &Value) -> Result<Value> {
    let host_name = host_param(params)?;
    require_host(state, &host_name)?;

    let host_owned = host_name.clone();
    let (password, otpauth) = run_keychain_bounded(
        "credential read",
        &host_name,
        CREDENTIAL_OP_TIMEOUT,
        move || {
            let ks = platform_store();
            let password = get_password(&ks, &host_owned)?;
            let otpauth = get_otpauth(&ks, &host_owned)?;
            Ok((password, otpauth))
        },
    )?;

    // Audit line — the fact of the reveal, never the secrets themselves.
    log::info!("[{host_name}] stored credentials revealed to a local client");

    Ok(json!({
        "host": host_name,
        "password": password,
        "otpauth_url": otpauth,
    }))
}

// ---------------------------------------------------------------------------
// host_set_credentials — change the password / 2FA secret of an existing host
// ---------------------------------------------------------------------------

/// Update the stored password and/or 2FA secret for an already-registered host.
///
/// Params: `host` plus at least one of `password`, `otpauth_url`,
/// `clear_otp_secret`. Whichever field is omitted keeps its current stored
/// value; `clear_otp_secret: true` DELETES the stored 2FA secret, turning the
/// host into a password-only one (the mirror image of adding a host with the
/// 2FA field left blank). An empty `otpauth_url` stays an error — dropping a
/// secret has to be asked for explicitly, never by sending a blank field.
///
/// The Keychain stores password and otpauth as two entries that
/// [`store_credentials`] writes together (with rollback), so a partial update
/// still needs the counterpart's current value: the read of the counterpart AND
/// the write happen inside ONE bounded worker, i.e. one Keychain session behind
/// one in-flight latch.
///
/// A live master is NOT torn down — an established ControlMaster needs no
/// credentials, so the new ones take effect on the next login. The response
/// carries `reconnect_required` so the app can offer "reconnect now" (which goes
/// through the existing stop→start path) rather than this handler inventing a
/// second reconnect mechanism.
pub fn host_set_credentials(state: &Arc<Mutex<State>>, params: &Value) -> Result<Value> {
    let host_name = host_param(params)?;
    require_host(state, &host_name)?;

    let new_password = opt_str(params, "password")?.map(|s| s.to_owned());
    let clear_otp = opt_bool(params, "clear_otp_secret")?;
    let new_otpauth = match opt_str(params, "otpauth_url")?.map(|s| s.trim().to_owned()) {
        // Both at once is contradictory — refuse rather than guess which one
        // the caller meant.
        Some(_) if clear_otp => {
            return Err(Error::BadParams(
                "pass either 'otpauth_url' or 'clear_otp_secret', not both".into(),
            ))
        }
        other => other,
    };

    if new_password.is_none() && new_otpauth.is_none() && !clear_otp {
        return Err(Error::BadParams(
            "nothing to change — pass 'password', 'otpauth_url' and/or 'clear_otp_secret'".into(),
        ));
    }

    // Validate the 2FA secret BEFORE touching the Keychain: storing an
    // unparseable secret would leave the host unable to log in, and the failure
    // would only surface ~30 s later inside a login worker.
    if let Some(ref url) = new_otpauth {
        if url.is_empty() {
            return Err(Error::BadParams(
                "otpauth_url is empty — omit the field to keep the current 2FA secret, or pass clear_otp_secret to remove it".into(),
            ));
        }
        extract_secret(url).map_err(|e| Error::BadParams(format!("invalid otpauth URL: {e}")))?;
    }
    if let Some(ref pw) = new_password {
        if pw.is_empty() {
            return Err(Error::BadParams(
                "password is empty — omit the field to keep the current password".into(),
            ));
        }
    }

    let mut changed: Vec<&'static str> = Vec::new();
    if new_password.is_some() {
        changed.push("password");
    }
    if new_otpauth.is_some() {
        changed.push("otp_secret");
    }
    if clear_otp {
        changed.push("otp_secret removed");
    }

    // Read-counterpart + write in ONE bounded worker.
    let host_owned = host_name.clone();
    let pw_arg = new_password.clone();
    // Clearing is just "write an empty secret" — the same shape as a rewrite,
    // so it takes the identical single-worker read-counterpart + write path.
    let otp_arg = if clear_otp {
        Some(String::new())
    } else {
        new_otpauth.clone()
    };
    run_keychain_bounded(
        "credential write",
        &host_name,
        CREDENTIAL_OP_TIMEOUT,
        move || {
            let ks = platform_store();
            let password = match pw_arg {
                Some(p) => p,
                None => get_password(&ks, &host_owned)?.unwrap_or_default(),
            };
            let otpauth = match otp_arg {
                Some(o) => o,
                None => get_otpauth(&ks, &host_owned)?.unwrap_or_default(),
            };
            store_credentials(&ks, &host_owned, &password, &otpauth)
        },
    )?;

    // The stored creds just changed — drop any cached copy so the next login
    // re-reads them instead of serving the old ones for the daemon's lifetime.
    crate::managers::invalidate_creds_cache(&host_name);
    log::info!(
        "[{host_name}] stored credentials updated ({})",
        changed.join(", ")
    );

    // A live/active host keeps running on its existing master; tell the client
    // so it can offer a reconnect.
    let reconnect_required = {
        let guard = crate::lock_state(state);
        guard
            .hosts
            .iter()
            .find(|h| h.host == host_name)
            .map(|h| h.active || h.is_master_ready)
            .unwrap_or(false)
    };

    Ok(json!({
        "ok": true,
        "host": host_name,
        "changed": changed,
        "reconnect_required": reconnect_required,
    }))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use a2fa_core::engine::State;
    use std::sync::{Arc, Mutex};

    fn make_state_with_host(name: &str, active: bool) -> Arc<Mutex<State>> {
        let mut state = State::with_tunnels(vec![]);
        state.hosts.push(Host {
            host: name.into(),
            status: "Idle".into(),
            active,
            is_master_ready: false,
            pool_index: 0,
            pool_alive: 0,
            is_mounted: false,
            last_msg: "OK".into(),
        });
        Arc::new(Mutex::new(state))
    }

    #[test]
    fn ping_returns_ok_pid() {
        let state = Arc::new(Mutex::new(State::with_tunnels(vec![])));
        let v = ping(&state).unwrap();
        assert_eq!(v["ok"], true);
        assert!(v["pid"].as_u64().unwrap() > 0);
    }

    #[test]
    fn list_hosts_empty() {
        let state = Arc::new(Mutex::new(State::with_tunnels(vec![])));
        let v = list_hosts(&state).unwrap();
        assert!(v.as_array().unwrap().is_empty());
    }

    #[test]
    fn list_hosts_one() {
        let state = make_state_with_host("k6", true);
        let v = list_hosts(&state).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["host"], "k6");
    }

    // host_toggle — State mutation is synchronous; the ssh worker is fire-and-
    // forget.  We verify the in-memory flag flip and the error paths.
    // We do NOT call host_toggle_with_registry in unit tests because it spawns
    // a real ssh worker thread that blocks on pty I/O; live-cluster verification
    // is deferred to the integration test suite.

    #[test]
    fn host_toggle_activates_flag_directly() {
        // Verify the State flag flip logic independently of the ssh worker.
        // This mirrors what host_toggle_with_registry does synchronously:
        // read host.active (false) → set to true.
        let state = make_state_with_host("k6", false);
        {
            let mut guard = crate::lock_state(&state);
            let h = guard.hosts.iter_mut().find(|h| h.host == "k6").unwrap();
            // Simulate what the handler does synchronously.
            h.active = true;
            h.last_msg = "Connecting…".into();
        }
        assert!(crate::lock_state(&state).hosts[0].active);
    }

    #[test]
    fn host_toggle_deactivates_flag_directly() {
        let state = make_state_with_host("k6", true);
        {
            let mut guard = crate::lock_state(&state);
            let h = guard.hosts.iter_mut().find(|h| h.host == "k6").unwrap();
            h.active = false;
            h.last_msg = "Deactivating…".into();
        }
        assert!(!crate::lock_state(&state).hosts[0].active);
    }

    #[test]
    fn bounded_recv_timeout_returns_error_without_hanging() {
        // Mirrors the host_totp bounded-thread pattern: if the worker (a hung
        // Keychain "Always Allow" prompt) never sends, recv_timeout must return
        // an error promptly instead of blocking the handler forever.
        let (tx, rx) = std::sync::mpsc::channel::<Result<()>>();
        std::thread::spawn(move || {
            // Never sends within the timeout window — simulates a wedged read.
            std::thread::sleep(std::time::Duration::from_secs(60));
            let _ = tx.send(Ok(()));
        });
        let start = std::time::Instant::now();
        let outcome: Result<()> = match rx.recv_timeout(std::time::Duration::from_millis(200)) {
            Ok(inner) => inner,
            Err(_) => Err(Error::Internal("totp read timed out".into())),
        };
        let elapsed = start.elapsed();
        assert!(matches!(outcome, Err(Error::Internal(_))), "expected timeout error");
        assert!(elapsed < std::time::Duration::from_secs(2), "must not block past the bound");
    }

    #[test]
    fn keychain_in_flight_blocks_second_concurrent_claim() {
        // First claim for an (op, host) key succeeds; a second concurrent claim
        // for the SAME key must be rejected (insert returns false) until released.
        let key = "totp read:guard-test-host";
        // Ensure a clean slate (other tests may have used the set).
        keychain_in_flight().lock().unwrap().remove(key);

        // First claim.
        assert!(
            keychain_in_flight().lock().unwrap().insert(key.to_owned()),
            "first claim must succeed"
        );
        // Second concurrent claim is blocked.
        assert!(
            !keychain_in_flight().lock().unwrap().insert(key.to_owned()),
            "second concurrent claim must be blocked"
        );

        // The RAII guard releases the latch on drop.
        {
            let _g = KeychainInFlightGuard { key: key.to_owned() };
        }
        // After release, a new claim succeeds again.
        assert!(
            keychain_in_flight().lock().unwrap().insert(key.to_owned()),
            "claim must succeed after the guard released the latch"
        );
        // Clean up.
        keychain_in_flight().lock().unwrap().remove(key);
    }

    /// The latch key includes the OPERATION, so the app's rotating-code polling
    /// (`totp read`) must never make a credential read/write for the same host
    /// report "busy" — that would break "Reveal password" on any host whose chip
    /// is on screen.
    #[test]
    fn keychain_latch_is_per_operation_not_just_per_host() {
        let host = "latch-op-test-host";
        let totp_key = format!("totp read:{host}");
        let read_key = format!("credential read:{host}");
        keychain_in_flight().lock().unwrap().remove(&totp_key);
        keychain_in_flight().lock().unwrap().remove(&read_key);

        // Claim the TOTP-read slot for this host…
        assert!(keychain_in_flight().lock().unwrap().insert(totp_key.clone()));
        // …a DIFFERENT operation on the same host is still free to claim.
        assert!(
            keychain_in_flight().lock().unwrap().insert(read_key.clone()),
            "a different operation on the same host must not be blocked"
        );
        keychain_in_flight().lock().unwrap().remove(&totp_key);
        keychain_in_flight().lock().unwrap().remove(&read_key);
    }

    /// `run_keychain_bounded` must return a retryable busy error (and NOT spawn
    /// a second worker) while the same (op, host) key is claimed.
    #[test]
    fn run_keychain_bounded_returns_busy_while_claimed() {
        let host = "bounded-busy-host";
        let key = format!("credential read:{host}");
        keychain_in_flight().lock().unwrap().insert(key.clone());
        let err = run_keychain_bounded(
            "credential read",
            host,
            std::time::Duration::from_secs(1),
            || Ok(()),
        )
        .unwrap_err();
        assert!(
            matches!(err, Error::Internal(ref m) if m.contains("already in flight")),
            "expected busy error, got {err:?}"
        );
        keychain_in_flight().lock().unwrap().remove(&key);
    }

    /// A closure that never returns must NOT pin the caller: the bound fires and
    /// an error comes back promptly (the whole point of the chokepoint).
    #[test]
    fn run_keychain_bounded_times_out_without_hanging() {
        let host = "bounded-timeout-host";
        keychain_in_flight()
            .lock()
            .unwrap()
            .remove(&format!("credential read:{host}"));
        let start = std::time::Instant::now();
        let err = run_keychain_bounded(
            "credential read",
            host,
            std::time::Duration::from_millis(150),
            || {
                std::thread::sleep(std::time::Duration::from_secs(30));
                Ok(())
            },
        )
        .unwrap_err();
        assert!(start.elapsed() < std::time::Duration::from_secs(2), "must not block past the bound");
        assert!(
            matches!(err, Error::Internal(ref m) if m.contains("timed out")),
            "expected timeout error, got {err:?}"
        );
    }

    /// The happy path passes the closure's value through unchanged and releases
    /// the latch, so a second call succeeds.
    #[test]
    fn run_keychain_bounded_passes_value_through_and_releases() {
        let host = "bounded-ok-host";
        for expected in [1u32, 2u32] {
            let got = run_keychain_bounded(
                "credential read",
                host,
                std::time::Duration::from_secs(5),
                move || Ok(expected),
            )
            .unwrap();
            assert_eq!(got, expected);
        }
        assert!(
            !keychain_in_flight()
                .lock()
                .unwrap()
                .contains(&format!("credential read:{host}")),
            "latch must be released after a successful run"
        );
    }

    #[test]
    fn mount_in_flight_blocks_second_concurrent_claim() {
        let host = "mount-guard-test-host";
        mount_in_flight().lock().unwrap().remove(host);
        assert!(
            mount_in_flight().lock().unwrap().insert(host.to_owned()),
            "first mount claim must succeed"
        );
        assert!(
            !mount_in_flight().lock().unwrap().insert(host.to_owned()),
            "second concurrent mount claim must be blocked (no duplicate sshfs subtree)"
        );
        {
            let _g = MountInFlightGuard { host: host.to_owned() };
        }
        assert!(
            mount_in_flight().lock().unwrap().insert(host.to_owned()),
            "mount claim must succeed after the guard released the latch"
        );
        mount_in_flight().lock().unwrap().remove(host);
    }

    #[test]
    fn first_line_takes_one_line_and_caps_it() {
        assert_eq!(first_line("boom"), "boom");
        // sshfs often prints a warning first and the real error after; a
        // status row shows one line, the log keeps the rest.
        assert_eq!(first_line("\n\n  real error \nnoise"), "real error");
        assert_eq!(first_line(""), "");
        let long = "x".repeat(400);
        let cut = first_line(&long);
        assert!(cut.chars().count() <= 120, "got {}", cut.chars().count());
        assert!(cut.ends_with('…'), "a truncated line must say so");
        // Multi-byte input must not panic or split a character.
        let wide = "文".repeat(400);
        assert!(first_line(&wide).chars().count() <= 120);
    }

    #[test]
    fn host_mount_toggle_busy_when_in_flight() {
        // With the latch already held for a host, host_mount_toggle returns a
        // busy error instead of spawning a duplicate mount op.
        let host = "mount-busy-test-host";
        let state = Arc::new(Mutex::new(State::with_tunnels(vec![])));
        mount_in_flight().lock().unwrap().insert(host.to_owned());
        let err = host_mount_toggle(&state, &json!({"host": host})).unwrap_err();
        assert!(
            matches!(err, Error::Internal(ref m) if m.contains("already in progress")),
            "expected busy error, got {err:?}"
        );
        mount_in_flight().lock().unwrap().remove(host);
    }

    #[test]
    fn host_toggle_not_found() {
        let state = Arc::new(Mutex::new(State::with_tunnels(vec![])));
        let err = host_toggle(&state, &json!({"host": "ghost"})).unwrap_err();
        assert!(matches!(err, Error::NotFound(_)));
    }

    #[test]
    fn host_toggle_missing_host_param() {
        let state = Arc::new(Mutex::new(State::with_tunnels(vec![])));
        let err = host_toggle(&state, &json!({})).unwrap_err();
        assert!(matches!(err, Error::BadParams(_)));
    }

    #[test]
    fn host_rotate_is_noop_for_active_host() {
        // Single-master: rotation is a no-op that succeeds for an active host
        // and leaves pool_index untouched.
        let state = make_state_with_host("k6", true);
        crate::lock_state(&state).hosts[0].pool_index = 0;
        host_rotate(&state, &json!({"host": "k6"}), None).unwrap();
        assert_eq!(crate::lock_state(&state).hosts[0].pool_index, 0);
    }

    #[test]
    fn host_rotate_not_active_returns_not_found() {
        let state = make_state_with_host("k6", false);
        let err = host_rotate(&state, &json!({"host": "k6"}), None).unwrap_err();
        assert!(matches!(err, Error::NotFound(_)));
    }

    #[test]
    fn valid_host_name_accepts_safe_names() {
        assert!(valid_host_name("k6"));
        assert!(valid_host_name("holy_gpu01"));
        assert!(valid_host_name("node-1.cluster"));
        assert!(valid_host_name("_underscore_start"));
    }

    #[test]
    fn valid_host_name_rejects_unsafe() {
        assert!(!valid_host_name(""));
        assert!(!valid_host_name("a/b"));
        assert!(!valid_host_name("a..b"));
        assert!(!valid_host_name("-bad"));
        assert!(!valid_host_name(".bad"));
    }

    #[test]
    fn host_add_bad_host_name_returns_bad_params() {
        let state = Arc::new(Mutex::new(State::with_tunnels(vec![])));
        let err = host_add(
            &state,
            &json!({"host": "a/b", "password": "x", "otpauth_url": "otpauth://totp/x?secret=ABC"}),
            None,
            None,
        )
        .unwrap_err();
        assert!(matches!(err, Error::BadParams(_)));
    }

    #[test]
    fn host_add_invalid_otpauth_url_returns_bad_params() {
        let state = Arc::new(Mutex::new(State::with_tunnels(vec![])));
        let err = host_add(
            &state,
            &json!({"host": "k6", "password": "x", "otpauth_url": "otpauth://totp/no-secret-here"}),
            None,
            None,
        )
        .unwrap_err();
        assert!(matches!(err, Error::BadParams(_)));
    }

    /// An account WITHOUT 2FA is registered by leaving the secret blank. The
    /// blank must survive validation — it used to be rejected as "invalid
    /// otpauth URL", which made such an account impossible to add at all.
    ///
    /// Asserted without touching the real Keychain: the host is already in
    /// State, so `host_add` stops at the duplicate check, which sits AFTER the
    /// secret extraction. Reaching it proves the blank secret was accepted.
    #[test]
    fn host_add_accepts_a_blank_otpauth_for_a_password_only_host() {
        for blank in ["", "   "] {
            let state = make_state_with_host("k6", false);
            let err = host_add(
                &state,
                &json!({"host": "k6", "password": "x", "otpauth_url": blank}),
                None,
                None,
            )
            .unwrap_err();
            assert!(
                matches!(err, Error::Duplicate(_)),
                "a blank 2FA secret must not be rejected as a bad URL, got {err:?}"
            );
        }
    }

    /// The blank-is-fine rule must not become "anything is fine": an unusable
    /// secret is still refused at entry rather than 30 s later in a login worker.
    #[test]
    fn host_add_still_rejects_a_typod_otpauth() {
        let state = Arc::new(Mutex::new(State::with_tunnels(vec![])));
        let err = host_add(
            &state,
            &json!({"host": "k6", "password": "x",
                    "otpauth_url": "otpauth://totp/Example:user?issuer=Example"}),
            None,
            None,
        )
        .unwrap_err();
        assert!(
            matches!(err, Error::BadParams(ref m) if m.contains("invalid otpauth")),
            "got {err:?}"
        );
    }

    /// The dry-run's three modes, decided without any Keychain or ssh I/O.
    /// `otpauth_url: ""` must resolve to "test these credentials" (a
    /// password-only login) — the handler then runs a real ssh login, which is
    /// why only the decision is asserted here.
    #[test]
    fn creds_under_test_supports_password_only_and_stored_modes() {
        assert!(matches!(
            creds_under_test(&json!({"host": "k6"})).unwrap(),
            CredsUnderTest::Stored
        ));
        let supplied = creds_under_test(&json!({"host": "k6", "password": "pw",
                                                "otpauth_url": ""}))
            .unwrap();
        match supplied {
            CredsUnderTest::Supplied {
                password,
                otpauth_url,
            } => {
                assert_eq!(password, "pw");
                assert!(otpauth_url.is_empty(), "blank stays blank — no 2FA");
            }
            _ => panic!("an explicit pair must be tested as supplied"),
        }
        // ...and a blank secret is not a parse error downstream.
        assert_eq!(
            a2fa_core::totp::extract_secret_optional("").unwrap(),
            None
        );
    }

    #[test]
    fn host_test_credentials_bad_otpauth_returns_ok_false() {
        let state = Arc::new(Mutex::new(State::with_tunnels(vec![])));
        // Use a well-formed otpauth:// URL that is MISSING the `secret=` param.
        // extract_secret must return Err before any I/O is attempted.
        let v = host_test_credentials(
            &state,
            &json!({"host": "k6", "password": "x",
                    "otpauth_url": "otpauth://totp/Example:user?issuer=Example"}),
            None,
        )
        .unwrap();
        assert_eq!(v["ok"], false);
        assert!(v["reason"].as_str().unwrap().contains("invalid otpauth"));
    }

    /// The registry-routed variant must also validate the URL before any I/O
    /// (and accept the registry without touching it on the error path).
    #[test]
    fn host_test_credentials_with_registry_validates_first() {
        let state = Arc::new(Mutex::new(State::with_tunnels(vec![])));
        let v = host_test_credentials(
            &state,
            &json!({"host": "", "password": "x",
                    "otpauth_url": "otpauth://totp/x?secret=JBSWY3DPEHPK3PXP"}),
            Some(OtpRegistry::new()),
        )
        .unwrap();
        assert_eq!(v["ok"], false);
        assert_eq!(v["reason"], "host required");
    }

    #[test]
    fn host_test_credentials_empty_host_returns_ok_false() {
        let state = Arc::new(Mutex::new(State::with_tunnels(vec![])));
        let v = host_test_credentials(
            &state,
            &json!({"host": "", "password": "x",
                    "otpauth_url": "otpauth://totp/x?secret=JBSWY3DPEHPK3PXP"}),
            None,
        )
        .unwrap();
        assert_eq!(v["ok"], false);
    }

    // host_totp — verify the param-validation paths WITHOUT touching the real
    // Keychain (host-not-found and missing-host-param both return before any
    // Keychain read). The TOTP math itself is covered by the core
    // totp_now_detailed tests in a2fa-core.
    #[test]
    fn host_totp_not_found_returns_not_found() {
        let state = Arc::new(Mutex::new(State::with_tunnels(vec![])));
        let err = host_totp(&state, &json!({"host": "ghost"})).unwrap_err();
        assert!(matches!(err, Error::NotFound(_)));
    }

    #[test]
    fn host_totp_missing_host_param_returns_bad_params() {
        let state = Arc::new(Mutex::new(State::with_tunnels(vec![])));
        let err = host_totp(&state, &json!({})).unwrap_err();
        assert!(matches!(err, Error::BadParams(_)));
    }

    // -----------------------------------------------------------------------
    // host_credentials / host_reveal_credentials / host_set_credentials
    //
    // These return BEFORE any Keychain access on every path asserted here
    // (unknown host, bad name, nothing-to-change, invalid secret), so the tests
    // run headlessly — no system credential store involved.
    // -----------------------------------------------------------------------

    // -----------------------------------------------------------------------
    // host_remove
    // -----------------------------------------------------------------------

    /// REGRESSION: removing a host used to leave its sshfs mounts mounted while
    /// tearing down the master they ride — a wedged mount that hangs Finder,
    /// with the host row gone so nothing in the app could unmount it. Removal
    /// must report the host gone AND leave no mount directory behind.
    #[test]
    fn host_remove_leaves_no_mount_directory_behind() {
        let host = "a2fa-test-mountleak";
        let state = make_state_with_host(host, false);
        // Simulate the leftover directory a mount leaves under ~/Mounts/<host>.
        let dir = mounts_root().join(host);
        let _ = std::fs::create_dir_all(&dir);

        let v = host_remove(&state, &json!({"host": host}), None).unwrap();
        assert_eq!(v["ok"], true);
        assert!(
            !dir.exists(),
            "removal must clean up ~/Mounts/<host>, found {dir:?}"
        );
    }

    // ---- jump-host pins survive a host removal -------------------------

    #[test]
    fn removing_a_host_drops_it_from_a_multi_host_pin() {
        let c = Some(vec!["a".to_string(), "b".to_string(), "c".to_string()]);
        assert_eq!(
            jump_candidates_without(&c, "b"),
            Some(vec!["a".to_string(), "c".to_string()])
        );
    }

    /// REGRESSION: a tunnel pinned ONLY to the removed host would wait forever
    /// for a jump host that can never appear — it never even attempts a start,
    /// so the recovery-failure auto-stop never fires either. An emptied pin must
    /// become None ("any ready host").
    #[test]
    fn removing_the_only_pinned_host_falls_back_to_any_host() {
        let c = Some(vec!["login1".to_string()]);
        assert_eq!(jump_candidates_without(&c, "login1"), None);
    }

    #[test]
    fn removing_an_unrelated_host_leaves_pins_untouched() {
        let c = Some(vec!["a".to_string()]);
        assert_eq!(jump_candidates_without(&c, "zzz"), c);
        assert_eq!(jump_candidates_without(&None, "a"), None);
    }

    #[test]
    fn host_remove_not_found_returns_not_found() {
        let state = Arc::new(Mutex::new(State::with_tunnels(vec![])));
        let err = host_remove(&state, &json!({"host": "ghost"}), None).unwrap_err();
        assert!(matches!(err, Error::NotFound(_)));
    }

    #[test]
    fn host_remove_rejects_unsafe_host_name() {
        let state = make_state_with_host("../../etc", false);
        let err = host_remove(&state, &json!({"host": "../../etc"}), None).unwrap_err();
        assert!(matches!(err, Error::BadParams(_)));
    }

    #[test]
    fn host_remove_missing_host_param_returns_bad_params() {
        let state = Arc::new(Mutex::new(State::with_tunnels(vec![])));
        let err = host_remove(&state, &json!({}), None).unwrap_err();
        assert!(matches!(err, Error::BadParams(_)));
    }

    /// The host must be GONE from State — list_hosts is what the UI renders, so
    /// a host left behind reads as "remove didn't work". The Keychain delete is
    /// attempted on a worker and may fail in a test environment; removal must
    /// still complete (that's the documented precedence).
    #[test]
    fn host_remove_drops_the_host_from_state() {
        let state = make_state_with_host("a2fa-test-removeme", true);
        // Second host stays put — removal must be surgical.
        crate::lock_state(&state).hosts.push(Host {
            host: "a2fa-test-keepme".into(),
            status: "Idle".into(),
            active: false,
            is_master_ready: false,
            pool_index: 0,
            pool_alive: 0,
            is_mounted: false,
            last_msg: String::new(),
        });

        let v = host_remove(&state, &json!({"host": "a2fa-test-removeme"}), None).unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["host"], "a2fa-test-removeme");
        assert!(v["credentials_deleted"].is_boolean());

        let guard = crate::lock_state(&state);
        assert!(
            !guard.hosts.iter().any(|h| h.host == "a2fa-test-removeme"),
            "removed host must be gone from State"
        );
        assert!(
            guard.hosts.iter().any(|h| h.host == "a2fa-test-keepme"),
            "removal must not touch other hosts"
        );
    }

    #[test]
    fn host_credentials_not_found_returns_not_found() {
        let state = Arc::new(Mutex::new(State::with_tunnels(vec![])));
        let err = host_credentials(&state, &json!({"host": "ghost"})).unwrap_err();
        assert!(matches!(err, Error::NotFound(_)));
    }

    #[test]
    fn host_credentials_missing_host_param_returns_bad_params() {
        let state = Arc::new(Mutex::new(State::with_tunnels(vec![])));
        let err = host_credentials(&state, &json!({})).unwrap_err();
        assert!(matches!(err, Error::BadParams(_)));
    }

    /// The host name is a Keychain account key — an unsafe name must be rejected
    /// as BadParams before anything reads the store.
    #[test]
    fn host_credentials_rejects_unsafe_host_name() {
        let state = make_state_with_host("../../etc", false);
        let err = host_credentials(&state, &json!({"host": "../../etc"})).unwrap_err();
        assert!(matches!(err, Error::BadParams(_)));
    }

    #[test]
    fn host_reveal_credentials_not_found_returns_not_found() {
        let state = Arc::new(Mutex::new(State::with_tunnels(vec![])));
        let err = host_reveal_credentials(&state, &json!({"host": "ghost"})).unwrap_err();
        assert!(matches!(err, Error::NotFound(_)));
    }

    #[test]
    fn host_reveal_credentials_rejects_unsafe_host_name() {
        let state = make_state_with_host("-oProxyCommand=x", false);
        let err = host_reveal_credentials(&state, &json!({"host": "-oProxyCommand=x"})).unwrap_err();
        assert!(matches!(err, Error::BadParams(_)));
    }

    #[test]
    fn host_set_credentials_not_found_returns_not_found() {
        let state = Arc::new(Mutex::new(State::with_tunnels(vec![])));
        let err =
            host_set_credentials(&state, &json!({"host": "ghost", "password": "x"})).unwrap_err();
        assert!(matches!(err, Error::NotFound(_)));
    }

    /// No `password` and no `otpauth_url` → nothing to do. Must be a loud
    /// BadParams, not a silent no-op that the UI reports as "saved".
    #[test]
    fn host_set_credentials_requires_at_least_one_field() {
        let state = make_state_with_host("k6", false);
        let err = host_set_credentials(&state, &json!({"host": "k6"})).unwrap_err();
        assert!(
            matches!(err, Error::BadParams(ref m) if m.contains("nothing to change")),
            "got {err:?}"
        );
    }

    /// An unparseable 2FA secret must be rejected BEFORE the Keychain write —
    /// storing it would leave the host unable to log in, failing ~30s later
    /// inside a login worker instead of here.
    #[test]
    fn host_set_credentials_rejects_invalid_otpauth_before_writing() {
        let state = make_state_with_host("k6", false);
        let err = host_set_credentials(
            &state,
            &json!({"host": "k6", "otpauth_url": "otpauth://totp/Example:user?issuer=Example"}),
        )
        .unwrap_err();
        assert!(
            matches!(err, Error::BadParams(ref m) if m.contains("invalid otpauth")),
            "got {err:?}"
        );
    }

    /// An EMPTY string is a different intent from an omitted field: omitting
    /// keeps the current value, so an empty value is a mistake worth surfacing
    /// rather than silently wiping the stored credential.
    #[test]
    fn host_set_credentials_rejects_empty_values() {
        let state = make_state_with_host("k6", false);
        let err =
            host_set_credentials(&state, &json!({"host": "k6", "password": ""})).unwrap_err();
        assert!(
            matches!(err, Error::BadParams(ref m) if m.contains("password is empty")),
            "got {err:?}"
        );
        // The rejection must also point at the deliberate way to drop a secret,
        // so it isn't a dead end for a host that no longer uses 2FA.
        let err =
            host_set_credentials(&state, &json!({"host": "k6", "otpauth_url": "  "})).unwrap_err();
        assert!(
            matches!(err, Error::BadParams(ref m)
                     if m.contains("otpauth_url is empty") && m.contains("clear_otp_secret")),
            "got {err:?}"
        );
    }

    /// Removing 2FA from an existing host is the mirror image of adding a host
    /// with the secret left blank. It must be asked for EXPLICITLY: a blank
    /// `otpauth_url` still errors (above), and the two ways of saying it can't
    /// be combined, since that request has no single meaning.
    #[test]
    fn host_set_credentials_clear_flag_is_explicit_and_exclusive() {
        let state = make_state_with_host("k6", false);
        // Both at once → refused before any Keychain I/O.
        let err = host_set_credentials(
            &state,
            &json!({"host": "k6", "clear_otp_secret": true,
                    "otpauth_url": "otpauth://totp/x?secret=JBSWY3DPEHPK3PXP"}),
        )
        .unwrap_err();
        assert!(
            matches!(err, Error::BadParams(ref m) if m.contains("not both")),
            "got {err:?}"
        );
        // A non-boolean flag is rejected, never coerced — this one deletes a
        // stored secret, so "yes" must not silently read as false (or true).
        let err = host_set_credentials(
            &state,
            &json!({"host": "k6", "clear_otp_secret": "yes"}),
        )
        .unwrap_err();
        assert!(
            matches!(err, Error::BadParams(ref m) if m.contains("must be a boolean")),
            "got {err:?}"
        );
        // clear_otp_secret: false is not a change on its own.
        let err =
            host_set_credentials(&state, &json!({"host": "k6", "clear_otp_secret": false}))
                .unwrap_err();
        assert!(
            matches!(err, Error::BadParams(ref m) if m.contains("nothing to change")),
            "got {err:?}"
        );
    }

    #[test]
    fn opt_bool_defaults_to_false_and_rejects_other_types() {
        assert!(!opt_bool(&json!({}), "flag").unwrap());
        assert!(!opt_bool(&json!({"flag": null}), "flag").unwrap());
        assert!(opt_bool(&json!({"flag": true}), "flag").unwrap());
        assert!(!opt_bool(&json!({"flag": false}), "flag").unwrap());
        for bad in [json!({"flag": 1}), json!({"flag": "true"}), json!({"flag": []})] {
            assert!(opt_bool(&bad, "flag").is_err(), "{bad}");
        }
    }

    /// host_test_credentials with NO secrets supplied falls back to the stored
    /// credentials. We can't exercise a real Keychain here, but the host guards
    /// must still run FIRST — an unsafe name must never reach the fallback (it
    /// becomes a Keychain account key and ssh argv).
    #[test]
    fn host_test_credentials_validates_host_before_stored_fallback() {
        let state = Arc::new(Mutex::new(State::with_tunnels(vec![])));
        let v = host_test_credentials(&state, &json!({"host": "-oProxyCommand=x"}), None).unwrap();
        assert_eq!(v["ok"], false);
        assert!(v["reason"].as_str().unwrap().contains("invalid host name"));

        let v = host_test_credentials(&state, &json!({}), None).unwrap();
        assert_eq!(v["ok"], false);
        assert_eq!(v["reason"], "host required");
    }

    /// Supplying EITHER secret must never silently mix in stored values — a
    /// half-supplied test must fail on the missing piece, not appear to pass
    /// with stored creds. Since an empty otpauth now legitimately means "no
    /// 2FA", the missing half can no longer be inferred, so the call is
    /// refused BEFORE any ssh I/O rather than testing something else.
    #[test]
    fn host_test_credentials_partial_params_do_not_use_stored_creds() {
        let state = Arc::new(Mutex::new(State::with_tunnels(vec![])));
        for params in [
            json!({"host": "k6", "password": "x"}),
            json!({"host": "k6", "otpauth_url": "otpauth://totp/x?secret=JBSWY3DPEHPK3PXP"}),
        ] {
            assert!(creds_under_test(&params).is_err(), "{params}");
            let v = host_test_credentials(&state, &params, None).unwrap();
            assert_eq!(v["ok"], false, "{params}");
            let reason = v["reason"].as_str().unwrap();
            assert!(
                reason.contains("send both") && reason.contains("no 2FA"),
                "must name the fix, got {reason:?}"
            );
        }
    }

    // host_mount_toggle — can't run sshfs in tests; verify error on
    // non-existent host or sshfs-not-installed path.
    // ---- multi-mount layout ---------------------------------------------

    /// Each (host, folder) gets its OWN mount point, which is what allows more
    /// than one folder per host to be mounted at the same time.
    #[test]
    fn mount_points_are_distinct_per_folder() {
        let a = mount_point_for("k6", "/scratch");
        let b = mount_point_for("k6", "/work");
        assert_ne!(a, b, "two folders must not share a mount point");
        // The name carries a readable prefix plus a uniqueness hash (see
        // mounts::slug_for) — assert the shape, not a literal name.
        let an = a.file_name().unwrap().to_string_lossy().into_owned();
        let bn = b.file_name().unwrap().to_string_lossy().into_owned();
        assert!(an.starts_with("scratch-"), "got {an:?}");
        assert!(bn.starts_with("work-"), "got {bn:?}");
        assert!(a.parent().unwrap().ends_with("k6"));
    }

    #[test]
    fn mount_point_for_root_is_named_not_empty() {
        let p = mount_point_for("k6", "/");
        assert!(p.ends_with("k6/root"), "got {p:?}");
    }

    /// Non-ASCII folders must get distinct mount points too — they all
    /// collapsed to the same one before the slug carried a hash.
    #[test]
    fn non_ascii_folders_get_distinct_mount_points() {
        let a = mount_point_for("k6", "/数据");
        let b = mount_point_for("k6", "/项目");
        assert_ne!(a, b);
        assert_ne!(a, mount_point_for("k6", "/"));
    }

    /// Different hosts must never collide even on the same remote path.
    #[test]
    fn mount_points_are_distinct_per_host() {
        assert_ne!(mount_point_for("k6", "/data"), mount_point_for("b8", "/data"));
    }

    /// A remote path with path separators or spaces must not escape its
    /// directory — the slug becomes ONE filesystem component.
    #[test]
    fn mount_point_cannot_escape_the_host_directory() {
        let p = mount_point_for("k6", "/../../etc/passwd");
        let rel = p.strip_prefix(mounts_root().join("k6")).unwrap();
        assert_eq!(rel.components().count(), 1, "slug must be a single component: {p:?}");
    }

    /// An explicit mount_point outside our mounts root must be refused —
    /// otherwise a client could ask the daemon to unmount an arbitrary volume.
    #[test]
    fn mount_toggle_refuses_a_mount_point_outside_the_mounts_root() {
        let host = "a2fa-test-outsideroot";
        let state = make_state_with_host(host, true);
        let err = host_mount_toggle(
            &state,
            &json!({"host": host, "mount_point": "/Volumes/SomeoneElse"}),
        )
        .unwrap_err();
        assert!(
            matches!(err, Error::BadParams(ref m) if m.contains("mounts folder")),
            "got {err:?}"
        );
        // …and the mount latch must not be left claimed by the rejection.
        assert!(!mount_in_flight().lock().unwrap().contains(host));
    }

    /// REGRESSION: an explicit mount_point that is no longer mounted must be a
    /// no-op, NOT a mount. The toggle would otherwise fall through and mount the
    /// default remote path ("/") at that directory — mounting something nobody
    /// asked for. Reachable as a race: the UI lists a mount, it goes away, the
    /// user clicks it.
    #[test]
    fn unmounting_an_already_unmounted_point_is_a_noop_not_a_mount() {
        let state = make_state_with_host("a2fa-test-noop", true);
        let mp = mounts_root().join("a2fa-test-noop").join("nothing-here");
        let v = host_mount_toggle(
            &state,
            &json!({"host": "a2fa-test-noop", "mount_point": mp.to_string_lossy()}),
        )
        .unwrap();
        assert_eq!(v["mounted"], false);
        assert_eq!(v["note"], "already unmounted");
        assert!(!mp.exists(), "must not have created or mounted anything");
    }

    /// REGRESSION (found by running the suite on Linux, where sshfs is often
    /// absent): resolving the sshfs binary used to be the FIRST thing
    /// `host_mount_toggle` did, so on a machine without sshfs even an UNMOUNT
    /// failed with "install sshfs" — leaving a mount the user could not remove.
    /// Unmounting needs only fusermount/umount, so the lookup must be lazy.
    #[test]
    fn unmount_paths_do_not_require_sshfs_to_be_installed() {
        let host = "a2fa-test-nosshfs";
        let state = make_state_with_host(host, true);
        let mp = mounts_root().join(host).join("nothing-here");
        // Nothing is mounted there, so this takes the "already unmounted"
        // early return — which must be reached whether or not sshfs exists.
        let v = host_mount_toggle(
            &state,
            &json!({"host": host, "mount_point": mp.to_string_lossy()}),
        )
        .expect("an unmount request must not depend on sshfs being installed");
        assert_eq!(v["mounted"], false);
        assert_eq!(v["note"], "already unmounted");
        assert!(!mp.exists(), "must not have created or mounted anything");
    }

    /// REGRESSION (found by fuzzing the live IPC surface): a wrong-typed
    /// OPTIONAL parameter was silently read as "absent", which for parameters
    /// that select WHAT to act on turned a malformed request into a different
    /// action — `remote_path: 123` mounted "/" instead, and a wrong-typed
    /// `mount_point` toggled a mount instead of addressing an existing one.
    #[test]
    fn wrong_typed_optional_params_are_refused_not_ignored() {
        // A host name unique to this test: `mount_in_flight` is process-global,
        // so sharing a name with a parallel test made this flake.
        let host = "a2fa-test-badtypes";
        let state = make_state_with_host(host, true);
        for (method, params) in [
            ("toggle-path", json!({"host": host, "remote_path": 123})),
            ("toggle-point", json!({"host": host, "mount_point": 42})),
        ] {
            let err = host_mount_toggle(&state, &params).unwrap_err();
            assert!(
                matches!(err, Error::BadParams(ref m) if m.contains("must be a string")),
                "{method}: got {err:?}"
            );
        }
        let err = host_mount_repair(&state, &json!({"host": host, "mount_point": {"a": 1}}))
            .unwrap_err();
        assert!(matches!(err, Error::BadParams(_)), "got {err:?}");
        let err = host_mounts(&state, &json!({"host": 999})).unwrap_err();
        assert!(matches!(err, Error::BadParams(_)), "got {err:?}");
        let err = host_set_credentials(&state, &json!({"host": host, "password": 5}))
            .unwrap_err();
        assert!(matches!(err, Error::BadParams(ref m) if m.contains("must be a string")));
    }

    #[test]
    fn opt_str_accepts_absent_null_and_strings() {
        let p = json!({"a": "x", "b": null});
        assert_eq!(opt_str(&p, "a").unwrap(), Some("x"));
        assert_eq!(opt_str(&p, "b").unwrap(), None);
        assert_eq!(opt_str(&p, "missing").unwrap(), None);
    }

    #[test]
    fn host_mounts_rejects_an_unsafe_host_filter() {
        let state = Arc::new(Mutex::new(State::with_tunnels(vec![])));
        let err = host_mounts(&state, &json!({"host": "../../etc"})).unwrap_err();
        assert!(matches!(err, Error::BadParams(_)));
    }

    /// Nothing mounted → a clear "nothing to do", not an error or a silent OK.
    #[test]
    fn mount_repair_with_nothing_mounted_reports_so() {
        let state = make_state_with_host("a2fa-test-nomounts", false);
        let v = host_mount_repair(&state, &json!({"host": "a2fa-test-nomounts"})).unwrap();
        assert_eq!(v["repaired"], 0);
        assert!(v["reason"].as_str().unwrap().contains("nothing is mounted"));
    }

    #[test]
    fn mount_repair_rejects_unsafe_host_name() {
        let state = make_state_with_host("../../etc", false);
        let err = host_mount_repair(&state, &json!({"host": "../../etc"})).unwrap_err();
        assert!(matches!(err, Error::BadParams(_)));
    }

    #[test]
    fn host_mount_toggle_not_found_returns_error() {
        let state = Arc::new(Mutex::new(State::with_tunnels(vec![])));
        let err = host_mount_toggle(&state, &json!({"host": "ghost"})).unwrap_err();
        assert!(matches!(err, Error::NotFound(_)));
    }

    // ---- mount remote-path validation ---------------------------------

    #[test]
    fn remote_path_must_be_absolute() {
        assert!(validate_remote_path("/").is_ok());
        assert!(validate_remote_path("/scratch/alice/project").is_ok());
        assert!(validate_remote_path("scratch/project").is_err(), "relative");
        assert!(validate_remote_path("~/project").is_err(), "tilde is not expanded");
        assert!(validate_remote_path("").is_err(), "empty");
    }

    /// A newline in the path would corrupt the sshfs argument and surface as a
    /// baffling mount error instead of a clear one here.
    #[test]
    fn remote_path_rejects_control_characters() {
        assert!(validate_remote_path("/a\nb").is_err());
        assert!(validate_remote_path("/a\tb").is_err());
    }

    /// An unsafe remote path must be refused BEFORE the in-flight latch or any
    /// sshfs spawn — otherwise a bad value could wedge the host's mount latch.
    #[test]
    fn host_mount_toggle_rejects_bad_remote_path() {
        let state = make_state_with_host("k6", true);
        let err = host_mount_toggle(
            &state,
            &json!({"host": "k6", "remote_path": "relative/path"}),
        )
        .unwrap_err();
        assert!(matches!(err, Error::BadParams(_)), "got {err:?}");
        // The latch must be free — a rejected path must not leave the host busy.
        assert!(
            !mount_in_flight().lock().unwrap().contains("k6"),
            "a rejected path must not hold the mount latch"
        );
    }

    #[test]
    fn host_mount_toggle_unsafe_host_name_returns_error() {
        let state = Arc::new(Mutex::new(State::with_tunnels(vec![])));
        // We need to add the host first so it's "found" but has an unsafe name.
        // (In practice host_add validates names; this tests the mount guard.)
        {
            crate::lock_state(&state).hosts.push(Host {
                host: "../../etc".into(),
                status: "Idle".into(),
                active: false,
                is_master_ready: false,
                pool_index: 0,
                pool_alive: 0,
                is_mounted: false,
                last_msg: "".into(),
            });
        }
        let err = host_mount_toggle(&state, &json!({"host": "../../etc"})).unwrap_err();
        assert!(matches!(err, Error::BadParams(_)));
    }
}
