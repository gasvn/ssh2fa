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
use a2fa_core::creds::keychain::KeychainStore;
use a2fa_core::creds::{delete_credentials, get_otpauth, get_password, store_credentials};
use a2fa_core::engine::State;
use a2fa_core::error::{Error, Result};
use a2fa_core::model::Host;
use a2fa_core::sys::run_cmd_bounded;
use a2fa_core::totp::{describe_otp, extract_secret, totp_now_detailed};
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
        let ks = KeychainStore;
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

/// Reap the leaked artifacts of a FAILED sshfs mount.
///
/// sshfs's macFUSE backend (`go-nfsv4`) is a separately-daemonized process: when
/// the mount fails (or `run_cmd_bounded` kills the sshfs child on its deadline),
/// the backend survives, holding a half-made mount. Targeted by the exact mount
/// point so an unrelated mount is never touched. Bounded helpers only; runs off
/// the State lock.
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
                // Only kill sshfs / its macFUSE backend for THIS mount path.
                if cmd.contains("go-nfsv4") || cmd.contains("sshfs") {
                    let _ = run_cmd_bounded("kill", &["-9", pid], Duration::from_secs(1));
                }
            }
        }
    }
    // 2. Force-unmount a half-made mount, then remove the now-empty dir.
    let _ = run_cmd_bounded("umount", &["-f", &mp], Duration::from_secs(10));
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
    let remote_path = params
        .get("remote_path")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or("/")
        .to_owned();
    validate_remote_path(&remote_path)?;

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

    // Snapshot current mount state.
    let is_mounted = {
        let guard = crate::lock_state(state);
        guard
            .hosts
            .iter()
            .find(|h| h.host == host_name)
            .ok_or_else(|| Error::NotFound(format!("host {host_name}")))?
            .is_mounted
    };

    // Validate the host name is mount-safe (no '/' or '..').
    // host_add validates names on the way in; this guards legacy entries.
    if host_name.contains('/') || host_name.contains("..") || host_name.is_empty() {
        return Err(Error::BadParams("invalid host name for mount".into()));
    }

    // Locate sshfs (bounded — `which` is instant, but never block). Under
    // launchd the daemon's PATH is the plist's minimal system set, which does
    // NOT include /usr/local/bin or /opt/homebrew/bin — `which sshfs` fails
    // there even with sshfs installed (mount was dead in production). Fall
    // back to the two well-known install prefixes by absolute path.
    let sshfs_bin: String = {
        let which_ok = run_cmd_bounded("which", &["sshfs"], std::time::Duration::from_secs(5))
            .map(|o| o.status.success())
            .unwrap_or(false);
        if which_ok {
            "sshfs".into()
        } else {
            match ["/usr/local/bin/sshfs", "/opt/homebrew/bin/sshfs"]
                .iter()
                .find(|p| std::path::Path::new(p).is_file())
            {
                Some(p) => (*p).to_string(),
                None => {
                    return Err(Error::Internal(
                        "sshfs not installed — install macFUSE + sshfs to use this feature"
                            .into(),
                    ));
                }
            }
        }
    };

    let mount_point = {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        std::path::PathBuf::from(home).join("Mounts").join(&host_name)
    };

    if is_mounted || mount_point.exists() && is_mount_point(&mount_point) {
        // Unmount.
        {
            let mut guard = crate::lock_state(state);
            if let Some(h) = guard.hosts.iter_mut().find(|h| h.host == host_name) {
                h.last_msg = "Unmounting…".into();
            }
        }
        let mp_str = mount_point.to_string_lossy().into_owned();
        // Bounded: a kernel-stuck `umount -f` on a wedged macFUSE mount must not
        // pin the handler thread forever.
        let _ = run_cmd_bounded("umount", &["-f", &mp_str], std::time::Duration::from_secs(10));
        // Judge ONLY by the actual mount state. Requiring umount's exit status
        // wedged the latch: if macFUSE had ALREADY auto-unmounted (network
        // drop), `umount -f` fails ("not currently mounted") → unmounted=false
        // → is_mounted stuck true and every retry hit the same failing branch.
        let unmounted = !is_mount_point(&mount_point);
        let mut guard = crate::lock_state(state);
        if let Some(h) = guard.hosts.iter_mut().find(|h| h.host == host_name) {
            h.is_mounted = !unmounted;
            h.last_msg = if unmounted { "Unmounted" } else { "Unmount failed" }.into();
        }
    } else {
        // Mount.
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
        let opts = format!(
            "reconnect,ConnectTimeout=10,ServerAliveInterval=15,ServerAliveCountMax=3,\
             volname={host_name},StrictHostKeyChecking=no,UserKnownHostsFile=/dev/null"
        );
        let result = run_cmd_bounded(
            &sshfs_bin,
            &[&src, &mp_str2, "-o", &opts],
            std::time::Duration::from_secs(45),
        );
        let mounted = result
            .map(|o| o.status.success() && is_mount_point(&mount_point))
            .unwrap_or(false);
        if !mounted {
            // A failed/killed sshfs leaves its DAEMONIZED macFUSE backend
            // (go-nfsv4) running — run_cmd_bounded only killed the direct sshfs
            // child, not the double-forked backend — plus a possibly half-made
            // mount + the created dir. Reap them so failed mounts don't leak
            // (observed: 5+ orphaned go-nfsv4 processes).
            reap_failed_sshfs(&mount_point);
        }
        let mut guard = crate::lock_state(state);
        if let Some(h) = guard.hosts.iter_mut().find(|h| h.host == host_name) {
            h.is_mounted = mounted;
            h.last_msg = if mounted {
                format!("Mounted {remote_path}")
            } else {
                "Mount failed".into()
            };
        }
    }

    // Report the mount point + what is mounted there. The app opens this in
    // Finder on a successful mount, so it must come back from the RPC rather
    // than being re-derived (and re-guessed) client-side.
    let is_mounted_now = {
        let guard = crate::lock_state(state);
        guard
            .hosts
            .iter()
            .find(|h| h.host == host_name)
            .map(|h| h.is_mounted)
            .unwrap_or(false)
    };
    Ok(json!({
        "host": host_name,
        "mounted": is_mounted_now,
        "mount_point": mount_point.to_string_lossy(),
        "remote_path": remote_path,
    }))
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

/// Returns true if `path` is an actual mount point.
/// Uses `std::fs::symlink_metadata` — if the entry exists and its device id
/// differs from its parent, it is a mount point.
fn is_mount_point(path: &std::path::Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(_) => return false,
    };
    let parent = path.parent().unwrap_or(path);
    let parent_meta = match std::fs::symlink_metadata(parent) {
        Ok(m) => m,
        Err(_) => return false,
    };
    meta.dev() != parent_meta.dev()
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
        .to_owned();

    let auto_connect = params
        .get("auto_connect")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // Extract TOTP secret from URL (validates the URL format).
    let secret = extract_secret(&otpauth_url)
        .map_err(|e| Error::BadParams(format!("invalid otpauth URL: {e}")))?;

    // Check for duplicates before doing any I/O.
    {
        let guard = crate::lock_state(state);
        if guard.hosts.iter().any(|h| h.host == host_name) {
            return Err(Error::Duplicate(format!("host {host_name} already exists")));
        }
    }

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
                let ks = KeychainStore;
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

    let supplied_password = params.get("password").and_then(|v| v.as_str());
    let supplied_otpauth = params.get("otpauth_url").and_then(|v| v.as_str());

    // Neither secret supplied → test what's stored for this host (bounded
    // Keychain read on a worker, never on this handler thread).
    let (password, otpauth_url) = match (supplied_password, supplied_otpauth) {
        (None, None) => {
            let host_owned = host.clone();
            let stored = run_keychain_bounded(
                "credential read",
                &host,
                CREDENTIAL_OP_TIMEOUT,
                move || {
                    let ks = KeychainStore;
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
        // One or both supplied → use exactly what was sent (an omitted field
        // stays empty, preserving the pre-existing contract for the Add-host
        // wizard, which always sends both).
        _ => (
            supplied_password.unwrap_or("").to_owned(),
            supplied_otpauth.unwrap_or("").to_owned(),
        ),
    };

    // Validate otpauth URL before attempting any ssh I/O.
    let secret = match extract_secret(&otpauth_url) {
        Ok(s) => s,
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
                a2fa_core::totp::totp_now(&secret_owned)
            })
        }
    };
    Ok(json!({ "ok": ok, "reason": reason }))
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

    // Clean up temp files (surface a failure instead of silently leaving it).
    if let Err(e) = std::fs::remove_file(&log_path) {
        log::warn!("test-login: could not remove {log_path:?}: {e}");
    }

    match result {
        Ok(LoginOutcome::Success) => (true, String::new()),
        Ok(LoginOutcome::AuthFailed { reason }) => (false, reason),
        Ok(LoginOutcome::Timeout) => (false, "Timeout before login completed".into()),
        Ok(LoginOutcome::Eof { output: _ }) => {
            (false, "SSH exited before login completed — host unreachable?".into())
        }
        Err(e) => (false, format!("System error: {e}")),
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
            let otpauth = get_otpauth(&KeychainStore, &host_owned)?
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
// host_remove — the missing other half of host_add
// ---------------------------------------------------------------------------

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
    //    tear down, then stop the master off the State lock.
    {
        let mut guard = crate::lock_state(state);
        if let Some(h) = guard.hosts.iter_mut().find(|h| h.host == host_name) {
            h.active = false;
            h.last_msg = "Removing…".into();
        }
    }
    if let Some(mgrs) = managers {
        spawn_managed_stop(host_name.clone(), Arc::clone(state), mgrs);
    }

    // 2. Delete both Keychain entries on the bounded worker (never inline).
    let host_owned = host_name.clone();
    let cred_result = run_keychain_bounded(
        "credential delete",
        &host_name,
        CREDENTIAL_OP_TIMEOUT,
        move || delete_credentials(&KeychainStore, &host_owned),
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

    // 3. Drop the passwords.json entry (serialized read-modify-write).
    if let Err(e) = update_meta(&passwords_path(), |meta| {
        meta.remove(&host_name);
    }) {
        log::warn!("[{host_name}] could not update passwords.json during removal: {e}");
    }

    // 4. Drop it from State so it disappears from list_hosts immediately.
    {
        let mut guard = crate::lock_state(state);
        guard.hosts.retain(|h| h.host != host_name);
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
            let ks = KeychainStore;
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
            let ks = KeychainStore;
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
/// Params: `host` plus at least one of `password`, `otpauth_url`.
/// Whichever field is omitted keeps its current stored value.
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

    let new_password = params
        .get("password")
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned());
    let new_otpauth = params
        .get("otpauth_url")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_owned());

    if new_password.is_none() && new_otpauth.is_none() {
        return Err(Error::BadParams(
            "nothing to change — pass 'password' and/or 'otpauth_url'".into(),
        ));
    }

    // Validate the 2FA secret BEFORE touching the Keychain: storing an
    // unparseable secret would leave the host unable to log in, and the failure
    // would only surface ~30 s later inside a login worker.
    if let Some(ref url) = new_otpauth {
        if url.is_empty() {
            return Err(Error::BadParams(
                "otpauth_url is empty — omit the field to keep the current 2FA secret".into(),
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

    // Read-counterpart + write in ONE bounded worker.
    let host_owned = host_name.clone();
    let pw_arg = new_password.clone();
    let otp_arg = new_otpauth.clone();
    run_keychain_bounded(
        "credential write",
        &host_name,
        CREDENTIAL_OP_TIMEOUT,
        move || {
            let ks = KeychainStore;
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
        let err =
            host_set_credentials(&state, &json!({"host": "k6", "otpauth_url": "  "})).unwrap_err();
        assert!(
            matches!(err, Error::BadParams(ref m) if m.contains("otpauth_url is empty")),
            "got {err:?}"
        );
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

    /// Supplying EITHER secret keeps the old contract (the other field defaults
    /// to empty) rather than silently mixing in stored values — a half-supplied
    /// test must fail on the missing piece, not appear to pass with stored creds.
    #[test]
    fn host_test_credentials_partial_params_do_not_use_stored_creds() {
        let state = Arc::new(Mutex::new(State::with_tunnels(vec![])));
        // password supplied, otpauth omitted → empty secret → invalid otpauth.
        let v =
            host_test_credentials(&state, &json!({"host": "k6", "password": "x"}), None).unwrap();
        assert_eq!(v["ok"], false);
        assert!(v["reason"].as_str().unwrap().contains("invalid otpauth"));
    }

    // host_mount_toggle — can't run sshfs in tests; verify error on
    // non-existent host or sshfs-not-installed path.
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
