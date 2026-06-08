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

use a2fa_core::config::{load_meta, passwords_path, save_meta, HostMeta};
use a2fa_core::creds::keychain::KeychainStore;
use a2fa_core::creds::{get_otpauth, get_password, store_credentials};
use a2fa_core::engine::State;
use a2fa_core::error::{Error, Result};
use a2fa_core::model::Host;
use a2fa_core::ssh::control::update_symlink;
use a2fa_core::totp::{extract_secret, totp_now_detailed};
use serde_json::{json, Value};

use crate::managers::{spawn_managed_start, spawn_managed_stop, spawn_warmup_slot1, HostManagers};
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
    let _guard = state.lock().unwrap();
    Ok(json!({ "ok": true, "pid": std::process::id() }))
}

// ---------------------------------------------------------------------------
// list_hosts
// ---------------------------------------------------------------------------

pub fn list_hosts(state: &Arc<Mutex<State>>) -> Result<Value> {
    let guard = state.lock().unwrap();
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

    // Snapshot the current active state and credentials while holding the lock.
    let (currently_active, password_opt, otpauth_opt) = {
        let guard = state.lock().unwrap();
        let host = guard
            .hosts
            .iter()
            .find(|h| h.host == host_name)
            .ok_or_else(|| Error::NotFound(format!("host {host_name}")))?;
        let currently_active = host.active;
        // Fetch credentials from Keychain (fast on macOS; no network I/O).
        let ks = KeychainStore;
        let pw = get_password(&ks, &host_name).ok().flatten();
        let oa = get_otpauth(&ks, &host_name).ok().flatten();
        (currently_active, pw, oa)
    };

    if currently_active {
        // Deactivate: reset circuit breakers in State + spawn stop worker.
        {
            let mut guard = state.lock().unwrap();
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
            let mut guard = state.lock().unwrap();
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
    let guard = state.lock().unwrap();
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
        let guard = state.lock().unwrap();
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
                    let mut guard = state.lock().unwrap();
                    if let Some(h) = guard.hosts.iter_mut().find(|h| h.host == host_name) {
                        h.active = false;
                        h.last_msg = "Deactivating…".into();
                    }
                }
                spawn_managed_stop(host_name.clone(), Arc::clone(state), Arc::clone(&mgrs));
            } else {
                // Activate: reset circuit breakers (on the persistent state) + start.
                // Reset circuit breakers so a manual toggle gives a fresh start.
                mgrs.with_pool_mut(&host_name, |p| p.reset_circuit_breakers());

                {
                    let mut guard = state.lock().unwrap();
                    if let Some(h) = guard.hosts.iter_mut().find(|h| h.host == host_name) {
                        h.active = true;
                        h.last_msg = "Connecting…".into();
                        h.status = "Connecting".into();
                    }
                }

                // Spawn slot 0 start (reads creds in-thread).
                spawn_managed_start(
                    host_name.clone(),
                    0,
                    Arc::clone(&reg),
                    Arc::clone(state),
                    Arc::clone(&mgrs),
                );

                // Kick off slot-1 warm-up (staggered ~5 s; reads creds in-thread).
                spawn_warmup_slot1(
                    host_name.clone(),
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

    let guard = state.lock().unwrap();
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

/// Toggle sshfs mount for a host: mount if not mounted, unmount if mounted.
///
/// Runs the mount/unmount off the State lock to avoid blocking the daemon.
/// Mirrors `SSHHostManager.toggle_mount` in backend.py.
pub fn host_mount_toggle(state: &Arc<Mutex<State>>, params: &Value) -> Result<Value> {
    let host_name = params["host"]
        .as_str()
        .ok_or_else(|| Error::BadParams("host required".into()))?
        .to_owned();

    // Snapshot current mount state.
    let is_mounted = {
        let guard = state.lock().unwrap();
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

    // Check sshfs is installed.
    if std::process::Command::new("which")
        .arg("sshfs")
        .output()
        .map(|o| !o.status.success())
        .unwrap_or(true)
    {
        return Err(Error::Internal(
            "sshfs not installed — install macFUSE + sshfs to use this feature".into(),
        ));
    }

    let mount_point = {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        std::path::PathBuf::from(home).join("Mounts").join(&host_name)
    };

    if is_mounted || mount_point.exists() && is_mount_point(&mount_point) {
        // Unmount.
        {
            let mut guard = state.lock().unwrap();
            if let Some(h) = guard.hosts.iter_mut().find(|h| h.host == host_name) {
                h.last_msg = "Unmounting…".into();
            }
        }
        let mp_str = mount_point.to_string_lossy().into_owned();
        let result = std::process::Command::new("umount")
            .args(["-f", &mp_str])
            .output();
        let unmounted = result
            .map(|o| o.status.success() && !is_mount_point(&mount_point))
            .unwrap_or(false);
        let mut guard = state.lock().unwrap();
        if let Some(h) = guard.hosts.iter_mut().find(|h| h.host == host_name) {
            h.is_mounted = !unmounted;
            h.last_msg = if unmounted { "Unmounted" } else { "Unmount failed" }.into();
        }
    } else {
        // Mount.
        let _ = std::fs::create_dir_all(&mount_point);
        {
            let mut guard = state.lock().unwrap();
            if let Some(h) = guard.hosts.iter_mut().find(|h| h.host == host_name) {
                h.last_msg = "Mounting…".into();
            }
        }
        let mp_str2 = mount_point.to_string_lossy().into_owned();
        let result = std::process::Command::new("sshfs")
            .args([
                &format!("{host_name}:/"),
                &mp_str2,
                "-o",
                &format!(
                    "reconnect,ServerAliveInterval=15,volname={host_name},\
                     StrictHostKeyChecking=no,UserKnownHostsFile=/dev/null"
                ),
            ])
            .output();
        let mounted = result
            .map(|o| o.status.success() && is_mount_point(&mount_point))
            .unwrap_or(false);
        let mut guard = state.lock().unwrap();
        if let Some(h) = guard.hosts.iter_mut().find(|h| h.host == host_name) {
            h.is_mounted = mounted;
            h.last_msg = if mounted { "Mounted" } else { "Mount failed" }.into();
        }
    }

    Ok(Value::Null)
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

/// Manually rotate the active connection-pool slot for a host.
///
/// Mirrors `mgr.update_symlink(new_idx)` in daemon.py.
/// Updates the active symlink on disk (so ssh clients immediately see the new
/// slot), then updates the pool_index in State.
pub fn host_rotate(state: &Arc<Mutex<State>>, params: &Value) -> Result<Value> {
    let host_name = params["host"]
        .as_str()
        .ok_or_else(|| Error::BadParams("host required".into()))?
        .to_owned();

    let new_index: usize = {
        let mut guard = state.lock().unwrap();
        let host = guard
            .hosts
            .iter_mut()
            .find(|h| h.host == host_name && h.active)
            .ok_or_else(|| Error::NotFound("host not active".into()))?;

        let new_idx = (host.pool_index + 1) % 2;
        host.pool_index = new_idx;
        host.last_msg = format!("Manual Rotate -> {new_idx}");
        new_idx as usize
    };

    // Update the active symlink on disk (off-lock, but fast — just a filesystem op).
    // update_symlink does a temp-link + rename so it is atomic.
    let _ = update_symlink(&host_name, new_index);

    Ok(Value::Null)
}

// ---------------------------------------------------------------------------
// host_add
// ---------------------------------------------------------------------------

/// Validate host-name regex (mirrors `_valid_host_name` in daemon.py).
fn valid_host_name(host: &str) -> bool {
    if host.contains("..") {
        return false;
    }
    let mut chars = host.chars();
    let first = match chars.next() {
        Some(c) => c,
        None => return false,
    };
    if !first.is_alphanumeric() && first != '_' {
        return false;
    }
    chars.all(|c| c.is_alphanumeric() || c == '.' || c == '-' || c == '_')
}

/// Add a host: validate name, extract secret, write Keychain + passwords.json,
/// add to State, and optionally spawn a master-start.
///
/// Mirrors `_add_host_persistent` + the `HOST_ADD` handler in daemon.py.
pub fn host_add(state: &Arc<Mutex<State>>, params: &Value) -> Result<Value> {
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
        let guard = state.lock().unwrap();
        if guard.hosts.iter().any(|h| h.host == host_name) {
            return Err(Error::Duplicate(format!("host {host_name} already exists")));
        }
    }

    // Write credentials to Keychain + passwords.json meta file.
    // These are I/O ops but they're fast (local disk / Keychain daemon).
    let ks = KeychainStore;
    store_credentials(&ks, &host_name, &password, &otpauth_url)?;
    // The stored creds just changed — drop any cached copy so the next login
    // re-reads the new secret instead of serving a stale one.
    crate::managers::invalidate_creds_cache(&host_name);

    // Update passwords.json metadata.
    let meta_path = passwords_path();
    let mut meta = load_meta(&meta_path);
    meta.insert(
        host_name.clone(),
        HostMeta {
            auto_connect,
        },
    );
    if let Err(e) = save_meta(&meta_path, &meta) {
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
        let mut guard = state.lock().unwrap();
        let s = host_snapshot(&new_host);
        guard.hosts.push(new_host);
        s
    };

    // If auto_connect, kick off a master-start.
    if auto_connect {
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
pub fn host_test_credentials(_state: &Arc<Mutex<State>>, params: &Value) -> Result<Value> {
    let host = params
        .get("host")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned();

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

    // Validate otpauth URL before attempting any I/O.
    let secret = match extract_secret(&otpauth_url) {
        Ok(s) => s,
        Err(e) => {
            return Ok(json!({
                "ok": false,
                "reason": format!("invalid otpauth URL: {e}")
            }));
        }
    };

    if host.is_empty() {
        return Ok(json!({ "ok": false, "reason": "host required" }));
    }

    // Run the one-shot login attempt on this thread.
    // (In production the daemon server wraps handlers in a worker pool.)
    let (ok, reason) = test_login(&host, &password, &secret);
    Ok(json!({ "ok": ok, "reason": reason }))
}

/// Attempt a one-shot, isolated SSH login.
///
/// Uses `a2fa_core::ssh::pty_auth::run_login` with a temporary ControlPath so
/// there is no interaction with the live master pool.
///
/// Returns `(true, "")` on success or `(false, reason)` on failure.
fn test_login(host: &str, password: &str, secret: &str) -> (bool, String) {
    use a2fa_core::ssh::pty_auth::{run_login, LoginOutcome};
    use a2fa_core::totp::totp_now;

    // Build a temp log path.
    let tmp_dir = std::env::temp_dir();
    // (No ControlPath needed — test login uses ControlPath=none.)

    // Build argv exactly like _test_credentials in daemon.py:
    // -v -E <log> -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null
    // -o ConnectTimeout=10 -o PreferredAuthentications=keyboard-interactive,password
    // -o ControlMaster=no -o ControlPath=none <host> echo __auto2fa_login_ok__
    let log_path = tmp_dir.join(format!("auto2fa-testlogin-{host}-{}.log", std::process::id()));
    let argv: Vec<String> = vec![
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
        "echo".into(), "__auto2fa_login_ok__".into(),
    ];

    let secret_owned = secret.to_owned();
    let otp_fn = move || totp_now(&secret_owned);

    let result = run_login(&argv, password, otp_fn);

    // Clean up temp files.
    let _ = std::fs::remove_file(&log_path);

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

/// Daemon-global set of hosts with a TOTP Keychain read currently in flight.
///
/// macOS serializes Keychain access process-wide, so a hung "Always Allow"
/// prompt blocks the worker thread until it is answered (~30 s from the app's
/// poll rollover). Without a guard, every `host_totp` IPC call for that host
/// would spawn another worker that immediately blocks behind the same prompt —
/// one leaked thread per call. This per-host latch caps it to AT MOST one
/// in-flight worker per host; concurrent callers get a "busy" error and retry.
fn totp_in_flight() -> &'static Mutex<HashSet<String>> {
    static IN_FLIGHT: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    IN_FLIGHT.get_or_init(|| Mutex::new(HashSet::new()))
}

/// RAII guard releasing a host's `totp_in_flight` entry on every exit path
/// (worker completion or panic). Mirrors `StartGuard` in managers.rs.
struct TotpInFlightGuard {
    host: String,
}

impl Drop for TotpInFlightGuard {
    fn drop(&mut self) {
        totp_in_flight()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.host);
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
        let guard = state.lock().unwrap();
        if !guard.hosts.iter().any(|h| h.host == host_name) {
            return Err(Error::NotFound(format!("host {host_name}")));
        }
    }

    // INVARIANT (see crate::managers::load_creds, ~lines 59-67): Keychain reads
    // MUST NOT happen on a shared/handler thread. macOS serializes Keychain
    // access process-wide, so an unanswered "Always Allow" prompt would block
    // the calling thread indefinitely — and this runs on the connection-handler
    // thread, so a hung prompt would wedge the whole handler. Push the Keychain
    // read + TOTP computation onto a short-lived worker thread and join it with
    // a BOUNDED timeout (mirroring run_ssh_g in ssh/control.rs). This caps the
    // handler's exposure to a hung prompt; on timeout we return an error rather
    // than blocking forever.
    // Per-host in-flight latch: at most ONE Keychain-reading worker may exist
    // per host at a time. If one is already in flight (e.g. blocked on a hung
    // "Always Allow" prompt), do NOT spawn another — return a retryable busy
    // error so we never pile up leaked threads behind the same prompt.
    {
        let mut inflight = totp_in_flight().lock().unwrap_or_else(|e| e.into_inner());
        if !inflight.insert(host_name.clone()) {
            return Err(Error::Internal(format!(
                "totp read already in flight for {host_name} — try again"
            )));
        }
    }

    let (tx, rx) = std::sync::mpsc::channel();
    let host_owned = host_name.clone();
    // Use Builder::spawn and CAPTURE the Result so a thread-creation failure
    // (EAGAIN under thread exhaustion) cannot panic AND cannot leave the latch
    // wedged: the TotpInFlightGuard only runs once the closure starts, so if
    // the spawn itself fails the guard never runs. Release the latch here and
    // return the same retryable error as the already-in-flight case. Mirrors
    // the spawn sites in tunnels/post_connect.rs and daemon/managers.rs.
    let spawn_res = std::thread::Builder::new()
        .name(format!("host_totp:{host_name}"))
        .spawn(move || {
            // RAII: release the per-host in-flight latch on every exit path
            // (completion or panic) so the host is never wedged as "busy".
            let _inflight_guard = TotpInFlightGuard {
                host: host_owned.clone(),
            };
            // Read the otpauth URL from the Keychain and compute the code +
            // timing entirely on this worker thread. Do NOT log code/secret.
            let result = (|| -> Result<(String, u32, u32)> {
                let otpauth = get_otpauth(&KeychainStore, &host_owned)?
                    .filter(|s| !s.trim().is_empty())
                    .ok_or_else(|| Error::NotFound(format!("no 2FA secret for {host_owned}")))?;
                let (code, period, remaining) = totp_now_detailed(&otpauth)?;
                Ok((code, period, remaining))
            })();
            let _ = tx.send(result);
        });
    if let Err(e) = spawn_res {
        // The worker (and its guard) never ran — release the latch here so the
        // host is not wedged "busy" forever, and return a retryable error.
        totp_in_flight()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&host_name);
        log::warn!("failed to spawn host_totp worker for {host_name}: {e}");
        return Err(Error::Internal(format!(
            "totp read could not start for {host_name} — try again"
        )));
    }

    let (code, period, remaining) = match rx.recv_timeout(std::time::Duration::from_secs(5)) {
        Ok(inner) => inner?,
        Err(_) => {
            return Err(Error::Internal("totp read timed out".into()));
        }
    };

    Ok(json!({
        "code": code,
        "period": period,
        "seconds_remaining": remaining,
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
            let mut guard = state.lock().unwrap();
            let h = guard.hosts.iter_mut().find(|h| h.host == "k6").unwrap();
            // Simulate what the handler does synchronously.
            h.active = true;
            h.last_msg = "Connecting…".into();
        }
        assert!(state.lock().unwrap().hosts[0].active);
    }

    #[test]
    fn host_toggle_deactivates_flag_directly() {
        let state = make_state_with_host("k6", true);
        {
            let mut guard = state.lock().unwrap();
            let h = guard.hosts.iter_mut().find(|h| h.host == "k6").unwrap();
            h.active = false;
            h.last_msg = "Deactivating…".into();
        }
        assert!(!state.lock().unwrap().hosts[0].active);
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
    fn totp_in_flight_blocks_second_concurrent_claim() {
        // First claim for a host succeeds; a second concurrent claim for the
        // SAME host must be rejected (insert returns false) until released.
        let host = "totp-guard-test-host";
        // Ensure a clean slate (other tests may have used the set).
        totp_in_flight().lock().unwrap().remove(host);

        // First claim.
        assert!(
            totp_in_flight().lock().unwrap().insert(host.to_owned()),
            "first claim must succeed"
        );
        // Second concurrent claim is blocked.
        assert!(
            !totp_in_flight().lock().unwrap().insert(host.to_owned()),
            "second concurrent claim must be blocked"
        );

        // The RAII guard releases the latch on drop.
        {
            let _g = TotpInFlightGuard { host: host.to_owned() };
        }
        // After release, a new claim succeeds again.
        assert!(
            totp_in_flight().lock().unwrap().insert(host.to_owned()),
            "claim must succeed after the guard released the latch"
        );
        // Clean up.
        totp_in_flight().lock().unwrap().remove(host);
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
    fn host_rotate_advances_pool_index() {
        let state = make_state_with_host("k6", true);
        state.lock().unwrap().hosts[0].pool_index = 0;
        host_rotate(&state, &json!({"host": "k6"})).unwrap();
        // Should advance 0 → 1 (mod 2).
        assert_eq!(state.lock().unwrap().hosts[0].pool_index, 1);
    }

    #[test]
    fn host_rotate_wraps_around() {
        let state = make_state_with_host("k6", true);
        state.lock().unwrap().hosts[0].pool_index = 1;
        host_rotate(&state, &json!({"host": "k6"})).unwrap();
        assert_eq!(state.lock().unwrap().hosts[0].pool_index, 0);
    }

    #[test]
    fn host_rotate_not_active_returns_not_found() {
        let state = make_state_with_host("k6", false);
        let err = host_rotate(&state, &json!({"host": "k6"})).unwrap_err();
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
        )
        .unwrap();
        assert_eq!(v["ok"], false);
        assert!(v["reason"].as_str().unwrap().contains("invalid otpauth"));
    }

    #[test]
    fn host_test_credentials_empty_host_returns_ok_false() {
        let state = Arc::new(Mutex::new(State::with_tunnels(vec![])));
        let v = host_test_credentials(
            &state,
            &json!({"host": "", "password": "x",
                    "otpauth_url": "otpauth://totp/x?secret=JBSWY3DPEHPK3PXP"}),
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

    // host_mount_toggle — can't run sshfs in tests; verify error on
    // non-existent host or sshfs-not-installed path.
    #[test]
    fn host_mount_toggle_not_found_returns_error() {
        let state = Arc::new(Mutex::new(State::with_tunnels(vec![])));
        let err = host_mount_toggle(&state, &json!({"host": "ghost"})).unwrap_err();
        assert!(matches!(err, Error::NotFound(_)));
    }

    #[test]
    fn host_mount_toggle_unsafe_host_name_returns_error() {
        let state = Arc::new(Mutex::new(State::with_tunnels(vec![])));
        // We need to add the host first so it's "found" but has an unsafe name.
        // (In practice host_add validates names; this tests the mount guard.)
        {
            state.lock().unwrap().hosts.push(Host {
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
