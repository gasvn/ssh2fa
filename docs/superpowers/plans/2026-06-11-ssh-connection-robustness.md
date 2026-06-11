# SSH Connection Robustness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop the daemon from falsely condemning healthy SSH masters and killing the user's live sessions in a re-2FA loop, by replacing the blocking `ssh -O check` hot-path probe with a cheap non-blocking liveness probe, adding hysteresis, and making teardown unable to kill a live master — then collapsing the 2-slot pool/symlink scheme to a single stable master per host.

**Architecture:** Split liveness judgement into two layers — network death stays the ssh master's own job (`ServerAliveInterval`), and the daemon only asks the cheap question "is a master listening at this path?" via a non-blocking unix-domain `connect()`. A master is reconnected only after N consecutive confident "dead" answers, and never killed while it is listening (adopt instead). Stage 1 ships this behavior on the existing 2-slot structure (urgent, deployable). Stage 2 removes the pool/rotation/symlink so each host has one stable socket.

**Tech Stack:** Rust (`a2fa-core`, `a2fa-daemon`, `a2fa-cli`, `a2fa-tui`), `libc` (already a dep), Swift/SwiftUI (`auto2fa-mac`). Build: `cargo test` / `cargo build`; deploy: `auto2fa-mac/package-app.sh`.

**Spec:** `docs/superpowers/specs/2026-06-11-ssh-connection-robustness-design.md`

---

## File Map

| File | Responsibility | Stage |
|---|---|---|
| `auto2fa-rs/crates/a2fa-core/src/ssh/control.rs` | new `MasterLiveness` + `master_probe`; live-master kill guards; (S2) remove symlink helpers + `-N` path | 1, 2 |
| `auto2fa-rs/crates/a2fa-core/src/ssh/master.rs` | `consecutive_probe_failures` + `note_probe_result` + `probe_to_check` + `PROBE_FAILURE_THRESHOLD`; (S2) de-pool `PoolState`, drop rotation | 1, 2 |
| `auto2fa-rs/crates/a2fa-daemon/src/managers.rs` | hot path uses `master_probe`; adopt-before-restart in worker; (S2) collapse `next_action` arms | 1, 2 |
| `auto2fa-rs/crates/a2fa-daemon/src/workers.rs` | (S2) de-index pool writes | 2 |
| `auto2fa-rs/crates/a2fa-daemon/src/handlers/hosts.rs` | (S2) de-index; remove rotate handler; boot janitor | 2 |
| `auto2fa-rs/crates/a2fa-cli/src/main.rs`, `a2fa-tui/src/views/hosts.rs` | (S2) single-connection glyph | 2 |
| `auto2fa-mac/Auto2FA/Models/Host.swift`, `Views/Components/HostRow.swift`, `FriendlyText.swift` | (S2) single connection indicator | 2 |

---

# STAGE 1 — De-thrash (urgent, deployable on its own)

Stage 1 keeps the 2-slot structure untouched; it only changes *how liveness is judged* and *that teardown can't kill a live master*. After Stage 1, rebuild + redeploy + verify the thrash is gone before starting Stage 2.

---

### Task 1: `MasterLiveness` + non-blocking `master_probe`

**Files:**
- Modify: `auto2fa-rs/crates/a2fa-core/src/ssh/control.rs` (add enum + const + fn; near the other control-channel helpers)
- Test: same file's `#[cfg(test)] mod tests` (add at the end)

- [ ] **Step 1: Write the failing test**

Add to the test module at the bottom of `control.rs`:

```rust
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
    fn probe_dead_when_socket_lingers_without_listener() {
        // std's UnixListener does NOT unlink on drop, so the file lingers with
        // no listener — exactly the "master died, socket left behind" case.
        let dir = std::env::temp_dir().join(format!("a2fa-probe-stale-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let sock = dir.join("stale.sock");
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).unwrap();
        drop(listener); // fd closed, file remains
        assert_eq!(master_probe(&sock), MasterLiveness::Dead);
        let _ = std::fs::remove_file(&sock);
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cd auto2fa-rs && cargo test -p a2fa-core probe_ 2>&1 | tail -20`
Expected: FAIL to compile — `cannot find type MasterLiveness` / `cannot find function master_probe`.

- [ ] **Step 3: Implement `MasterLiveness` + `master_probe`**

Add near the top of `control.rs` (after the existing `const`s, ~line 31):

```rust
/// How long to wait for a non-blocking unix-domain `connect()` to settle
/// (only the rare listen-backlog-full case ever reaches this).
const PROBE_CONNECT_TIMEOUT: Duration = Duration::from_millis(200);

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
```

Add the function alongside `master_check` (~after line 427):

```rust
/// Cheap, fork-free, non-blocking liveness probe for a ControlMaster socket.
///
/// Does a single non-blocking unix-domain `connect()` to `control_path`:
/// - connect succeeds        → [`MasterLiveness::Alive`] (a master is listening;
///   the kernel completes the connect against the listening socket even if the
///   master's user-space event loop is momentarily busy)
/// - `ECONNREFUSED`          → [`MasterLiveness::Dead`] (file exists, no listener)
/// - `ENOENT`/absent file    → [`MasterLiveness::Dead`] (no master at all)
/// - anything else / EAGAIN  → [`MasterLiveness::Inconclusive`]
///
/// Never spawns a process and never blocks on the network — a unix connect is a
/// local operation. This replaces `master_check` (`ssh -O check`, which forks a
/// process and can hang for seconds on a stale connection) on the heartbeat hot
/// path. Honors the no-wedge-on-the-heartbeat invariant.
pub fn master_probe(control_path: &Path) -> MasterLiveness {
    use std::os::unix::ffi::OsStrExt;

    let path_bytes = control_path.as_os_str().as_bytes();

    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    // sun_path must be NUL-terminated; need room for the trailing NUL.
    if path_bytes.len() >= std::mem::size_of_val(&addr.sun_path) {
        return MasterLiveness::Inconclusive; // path too long to address
    }
    for (i, b) in path_bytes.iter().enumerate() {
        addr.sun_path[i] = *b as libc::c_char;
    }

    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if fd < 0 {
        return MasterLiveness::Inconclusive;
    }
    // RAII-ish: ensure close on every return path.
    struct Fd(libc::c_int);
    impl Drop for Fd {
        fn drop(&mut self) {
            unsafe { libc::close(self.0) };
        }
    }
    let _guard = Fd(fd);

    // Non-blocking so a pathological backlog-full master can't wedge us.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL, 0);
        if flags >= 0 {
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
    }

    let addr_len = std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t;
    let rc = unsafe {
        libc::connect(
            fd,
            &addr as *const libc::sockaddr_un as *const libc::sockaddr,
            addr_len,
        )
    };
    if rc == 0 {
        return MasterLiveness::Alive;
    }
    match std::io::Error::last_os_error().raw_os_error() {
        Some(libc::ECONNREFUSED) => MasterLiveness::Dead,
        Some(libc::ENOENT) => MasterLiveness::Dead,
        Some(libc::EINPROGRESS) | Some(libc::EAGAIN) => {
            // Rare (backlog full): poll for writability, then read SO_ERROR.
            let mut pfd = libc::pollfd { fd, events: libc::POLLOUT, revents: 0 };
            let ms = PROBE_CONNECT_TIMEOUT.as_millis() as libc::c_int;
            let pr = unsafe { libc::poll(&mut pfd, 1, ms) };
            if pr <= 0 {
                return MasterLiveness::Inconclusive; // timeout / poll error
            }
            let mut err: libc::c_int = 0;
            let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
            let gr = unsafe {
                libc::getsockopt(
                    fd,
                    libc::SOL_SOCKET,
                    libc::SO_ERROR,
                    &mut err as *mut libc::c_int as *mut libc::c_void,
                    &mut len,
                )
            };
            if gr != 0 {
                return MasterLiveness::Inconclusive;
            }
            match err {
                0 => MasterLiveness::Alive,
                e if e == libc::ECONNREFUSED || e == libc::ENOENT => MasterLiveness::Dead,
                _ => MasterLiveness::Inconclusive,
            }
        }
        _ => MasterLiveness::Inconclusive,
    }
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cd auto2fa-rs && cargo test -p a2fa-core probe_ 2>&1 | tail -20`
Expected: PASS — `probe_alive_when_listener_present`, `probe_dead_when_socket_file_absent`, `probe_dead_when_socket_lingers_without_listener` all green.

- [ ] **Step 5: Commit**

```bash
git add auto2fa-rs/crates/a2fa-core/src/ssh/control.rs
git commit -m "feat(rust): cheap non-blocking master_probe (replaces ssh -O check on the hot path)"
```

---

### Task 2: Hysteresis state — `consecutive_probe_failures` + `probe_to_check`

**Files:**
- Modify: `auto2fa-rs/crates/a2fa-core/src/ssh/master.rs` (add field, const, methods, pure mapper)
- Test: same file's `#[cfg(test)] mod tests`

- [ ] **Step 1: Write the failing test**

Add to the test module in `master.rs`:

```rust
#[test]
fn probe_to_check_maps_with_hysteresis() {
    use crate::ssh::control::MasterLiveness::*;
    // Alive → always Some(true)
    assert_eq!(probe_to_check(Alive, 0, 3), Some(true));
    assert_eq!(probe_to_check(Alive, 9, 3), Some(true));
    // Inconclusive → never an answer
    assert_eq!(probe_to_check(Inconclusive, 5, 3), None);
    // Dead → None until the failure count reaches the threshold
    assert_eq!(probe_to_check(Dead, 1, 3), None);
    assert_eq!(probe_to_check(Dead, 2, 3), None);
    assert_eq!(probe_to_check(Dead, 3, 3), Some(false));
    assert_eq!(probe_to_check(Dead, 4, 3), Some(false));
}

#[test]
fn note_probe_result_counts_only_confident_deaths() {
    use crate::ssh::control::MasterLiveness::*;
    let mut p = PoolState::new("k6");
    p.note_probe_result(0, Dead);
    p.note_probe_result(0, Dead);
    assert_eq!(p.consecutive_probe_failures[0], 2);
    // Inconclusive does not move the counter.
    p.note_probe_result(0, Inconclusive);
    assert_eq!(p.consecutive_probe_failures[0], 2);
    // A single Alive resets it.
    p.note_probe_result(0, Alive);
    assert_eq!(p.consecutive_probe_failures[0], 0);
    // Slots are independent.
    p.note_probe_result(1, Dead);
    assert_eq!(p.consecutive_probe_failures[1], 1);
    assert_eq!(p.consecutive_probe_failures[0], 0);
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cd auto2fa-rs && cargo test -p a2fa-core probe_to_check note_probe_result 2>&1 | tail -20`
Expected: FAIL to compile — no field `consecutive_probe_failures`, no fn `note_probe_result` / `probe_to_check`.

- [ ] **Step 3: Implement the field, const, methods, and mapper**

In `master.rs`, add the constant near the other thresholds (~line 45):

```rust
/// Consecutive confident "dead" probes before a Ready master is condemned.
/// One transient probe failure must never trigger a reconnect.
pub const PROBE_FAILURE_THRESHOLD: u32 = 3;
```

Add the field to `PoolState` (after `flap_backoff_until`, ~line 96):

```rust
    /// Consecutive confident `Dead` probe results per slot (hysteresis). Reset
    /// to 0 on any `Alive`; untouched by `Inconclusive`.
    pub consecutive_probe_failures: [u32; POOL_SIZE],
```

Initialize it in `PoolState::new` (in the struct literal, alongside the others):

```rust
            consecutive_probe_failures: [0; POOL_SIZE],
```

Add the method on `impl PoolState` (near `note_slot_alive`):

```rust
    /// Fold one probe result into the per-slot hysteresis counter.
    pub fn note_probe_result(&mut self, index: usize, liveness: crate::ssh::control::MasterLiveness) {
        use crate::ssh::control::MasterLiveness::*;
        if index >= POOL_SIZE {
            return;
        }
        match liveness {
            Alive => self.consecutive_probe_failures[index] = 0,
            Dead => self.consecutive_probe_failures[index] += 1,
            Inconclusive => {} // no confident answer — leave the counter alone
        }
    }
```

Add the pure free function (module level, near `next_action`'s peers — but it lives in `master.rs` so the heartbeat can call it):

```rust
/// Map a probe result + current failure count to the legacy `Option<bool>`
/// "check_alive" that `next_action` consumes — applying hysteresis so a Ready
/// slot is only reported dead (`Some(false)`) after `threshold` consecutive
/// confident `Dead` probes. `Alive` → `Some(true)`; everything inconclusive or
/// below threshold → `None` (which `next_action` treats as "no restart").
pub fn probe_to_check(
    liveness: crate::ssh::control::MasterLiveness,
    consecutive_failures: u32,
    threshold: u32,
) -> Option<bool> {
    use crate::ssh::control::MasterLiveness::*;
    match liveness {
        Alive => Some(true),
        Inconclusive => None,
        Dead => {
            if consecutive_failures >= threshold {
                Some(false)
            } else {
                None
            }
        }
    }
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cd auto2fa-rs && cargo test -p a2fa-core probe_to_check note_probe_result 2>&1 | tail -20`
Expected: PASS. Also run the full core suite to confirm nothing else broke:
Run: `cd auto2fa-rs && cargo test -p a2fa-core 2>&1 | tail -15`
Expected: all green (the existing `PoolState::new` callers still compile because the new field has a literal initializer).

- [ ] **Step 5: Commit**

```bash
git add auto2fa-rs/crates/a2fa-core/src/ssh/master.rs
git commit -m "feat(rust): per-slot probe hysteresis (consecutive_probe_failures + probe_to_check)"
```

---

### Task 3: Heartbeat uses `master_probe` + hysteresis (the core behavior swap)

**Files:**
- Modify: `auto2fa-rs/crates/a2fa-daemon/src/managers.rs:1163-1179` (the per-slot probe block in `tick_host`)
- Test: covered by Task 2's pure tests + the existing `next_action_*` tests (unchanged); behavior verified at deploy.

- [ ] **Step 1: Replace the `master_check` call with the probe + hysteresis mapping**

In `tick_host`, replace this block (currently ~1164-1179):

```rust
    // --- Per-slot heartbeat: run ssh -O check (off-lock) ---
    for slot in 0..POOL_SIZE {
        let path = pool.pool_path(slot);

        // Only bother checking slots that have ever been started.
        let check_result: Option<bool> = match pool.slot_status[slot] {
            SlotStatus::Init => None, // never started — skip live check
            _ => Some(master_check(&path, host_name)),
        };
```

with:

```rust
    // --- Per-slot heartbeat: cheap non-blocking liveness probe (off-lock) ---
    for slot in 0..POOL_SIZE {
        let path = pool.pool_path(slot);

        // Only bother probing slots that have ever been started.
        let check_result: Option<bool> = match pool.slot_status[slot] {
            SlotStatus::Init => None, // never started — skip live check
            _ => {
                let liveness = a2fa_core::ssh::control::master_probe(&path);
                // Fold into the per-slot hysteresis counter, then derive the
                // legacy check_alive with the threshold applied. A single Dead
                // probe yields None (no restart); only PROBE_FAILURE_THRESHOLD
                // consecutive Dead probes yield Some(false).
                let failures = managers
                    .with_pool_mut(host_name, |p| {
                        p.note_probe_result(slot, liveness);
                        p.consecutive_probe_failures[slot]
                    })
                    .unwrap_or(0);
                a2fa_core::ssh::master::probe_to_check(
                    liveness,
                    failures,
                    a2fa_core::ssh::master::PROBE_FAILURE_THRESHOLD,
                )
            }
        };
```

Note: the existing `master_check` import at the top of `managers.rs` may now be unused — if `cargo build` warns `unused import`, remove `master_check` from the `use` line (keep other imports).

- [ ] **Step 2: Build and run the full daemon + core suite**

Run: `cd auto2fa-rs && cargo test -p a2fa-core -p a2fa-daemon 2>&1 | tail -25`
Expected: compiles clean; all tests green. The existing `next_action_ready_slot_with_failed_check_gives_restart` still passes (it calls `next_action` directly with `Some(false)`, which is unchanged — the hysteresis lives upstream in `tick_host`).

- [ ] **Step 3: Build the release daemon binary**

Run: `cd auto2fa-rs && cargo build --release -p a2fa-daemon 2>&1 | tail -5`
Expected: `Finished release` with no errors.

- [ ] **Step 4: Commit**

```bash
git add auto2fa-rs/crates/a2fa-daemon/src/managers.rs
git commit -m "feat(rust): heartbeat hot path uses master_probe + hysteresis instead of ssh -O check"
```

---

### Task 4: Adopt-before-restart — the no-kill safety gate in the restart worker

**Files:**
- Modify: `auto2fa-rs/crates/a2fa-daemon/src/managers.rs` — inside the `hb-restart` worker closure (~after the throttle + still-active re-check, before `load_creds`/`start_master`, ~line 1264)
- Test: behavior verified at deploy (the worker is an integration path); the pure adopt decision is already covered by `next_action_dead_slot_with_passing_check_adopts_instead_of_restart`.

- [ ] **Step 1: Insert the re-probe + adopt gate**

In the `hb-restart` worker, immediately after the `still_active` check passes and BEFORE `let (password, secret) = load_creds(&host_owned);`, insert:

```rust
                        // Adopt-before-restart: re-probe RIGHT before the
                        // destructive restart. If a master is listening now (it
                        // came back, or never actually died — the probe storm /
                        // a transient blip), DO NOT kill it and DO NOT burn a
                        // 2FA login. Adopt it back to Ready. This is the gate
                        // that makes a false condemnation non-destructive.
                        {
                            let path = managers2.snapshot(&host_owned).pool_path(slot);
                            if a2fa_core::ssh::control::master_probe(&path)
                                == a2fa_core::ssh::control::MasterLiveness::Alive
                            {
                                info!("[{host_owned}] hb-restart: master ALIVE on re-probe — adopting (no kill, no 2FA)");
                                managers2.with_pool_mut(&host_owned, |p| {
                                    p.slot_status[slot] = SlotStatus::Ready;
                                    p.consecutive_probe_failures[slot] = 0;
                                    p.mark_slot_ready(slot);
                                });
                                let alive = managers2
                                    .with_pool(&host_owned, |p| {
                                        p.slot_status
                                            .iter()
                                            .filter(|s| **s == SlotStatus::Ready)
                                            .count() as u8
                                    })
                                    .unwrap_or(1);
                                heal_host_state(&state2, &host_owned, slot, alive);
                                return; // StartGuard drops → releases the in-flight token
                            }
                        }
```

(`heal_host_state` is the existing helper at `managers.rs:1116`; `StartGuard` releases on the early `return` exactly as on the deactivation early-return below it.)

- [ ] **Step 2: Build and test**

Run: `cd auto2fa-rs && cargo test -p a2fa-daemon 2>&1 | tail -15`
Expected: compiles clean; all tests green.

- [ ] **Step 3: Commit**

```bash
git add auto2fa-rs/crates/a2fa-daemon/src/managers.rs
git commit -m "feat(rust): adopt-before-restart gate — never kill+reauth a master that is alive on re-probe"
```

---

### Task 5: Teardown guard — `cleanup_stale_socket` / `kill_orphaned_master` refuse to touch a live master

**Files:**
- Modify: `auto2fa-rs/crates/a2fa-core/src/ssh/control.rs` — top of `cleanup_stale_socket` (~463) and `kill_orphaned_master` (~499)
- Test: same file — a unit test asserting cleanup is a no-op when a listener is present.

- [ ] **Step 1: Write the failing test**

Add to the `probe_tests` module in `control.rs`:

```rust
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
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cd auto2fa-rs && cargo test -p a2fa-core cleanup_is_noop 2>&1 | tail -20`
Expected: FAIL — current `cleanup_stale_socket` removes the socket file unconditionally, so `sock.exists()` is false.

- [ ] **Step 3: Add the live-master guard**

At the very top of `cleanup_stale_socket` (before the polite `ssh -O exit`), add:

```rust
    // SAFETY GATE: never disturb a master that is currently listening. A live
    // listener means real client sessions may be multiplexed on it — removing
    // its socket or killing it would drop the user's sessions. The reconnect
    // path only reaches cleanup when we've decided the master is gone; this is
    // belt-and-suspenders against a master that recovered in between.
    if master_probe(path) == MasterLiveness::Alive {
        warn!("[{host}] cleanup_stale_socket: master is ALIVE on {} — refusing to clean", path.display());
        return;
    }
```

At the top of `kill_orphaned_master` (control.rs ~511, the `master_owner_pid` is separate — target the `pub fn kill_orphaned_master` that does the `[mux]` pgrep+kill; if its exact name/signature differs, add the same guard at its entry), add:

```rust
    // SAFETY GATE: a listening master is serving clients — do not kill it.
    if master_probe(path) == MasterLiveness::Alive {
        return;
    }
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cd auto2fa-rs && cargo test -p a2fa-core cleanup_is_noop 2>&1 | tail -20`
Expected: PASS. Then full core suite:
Run: `cd auto2fa-rs && cargo test -p a2fa-core 2>&1 | tail -15`
Expected: all green.

- [ ] **Step 5: Commit**

```bash
git add auto2fa-rs/crates/a2fa-core/src/ssh/control.rs
git commit -m "fix(rust): teardown refuses to remove/kill a master that is alive on probe"
```

---

### Task 6: Build, deploy Stage 1, and verify the thrash is gone

**Files:** none (build + deploy + observe)

- [ ] **Step 1: Full workspace test + release build**

Run: `cd auto2fa-rs && cargo test 2>&1 | tail -20 && cargo build --release 2>&1 | tail -5`
Expected: all tests green; `Finished release`.

- [ ] **Step 2: Bump the daemon bundle version so the installer ships the new binary**

The daemon version gate lives in the build/packaging. Verify the packaged daemon version differs from the installed one (otherwise `package-app.sh` no-ops):
Run: `cat ~/.auto2fa/.daemon-bundle-version`
Then build the packaged app:
Run: `cd auto2fa-mac && ./package-app.sh 2>&1 | tail -30`
Expected: builds a universal app + daemon, signs, and installs to `/Applications/Auto2FA.app`. If it reports "same version, skipping daemon install", bump the version per the script's mechanism and re-run.

- [ ] **Step 3: Confirm the new daemon is running**

Run: `launchctl list | grep auto2fa && ps aux | grep -m1 '[a]2fa-daemon'`
Expected: `com.auto2fa.daemon` has a PID and a NON-negative last-exit status (not `-9`); the daemon binary mtime is fresh.

- [ ] **Step 4: Verify the thrash is gone (observe the live log for ~2 minutes)**

Run: `sleep 120; echo '--- recent ---'; tail -200 /tmp/auto2fa_daemon.log | grep -cE 'needs restart \(status=Ready|killed (DUPLICATE|orphaned)|marked Dead but master is ALIVE|pgrep exceeded 2s'`
Expected: **0** (or near-0) — no Ready-slot condemnations, no master kills, no adopt-after-false-death, no pgrep saturation. Also:
Run: `tail -200 /tmp/auto2fa_daemon.log | grep -cE 'sending password|OTP submitted'`
Expected: near-0 in steady state (no constant re-2FA). Hosts should read Connected in the menu bar / `a2fa-cli`.

- [ ] **Step 5: Tag the Stage 1 checkpoint**

```bash
git tag robustness-stage1
echo "Stage 1 deployed and verified. Proceed to Stage 2."
```

---

# STAGE 2 — Single stable master (structural simplification)

Only start Stage 2 after Stage 1 is verified live. Stage 2 removes the 2-slot pool, the active symlink, and rotation, so each host has one stable socket `cm-auto2fa-<host>`. The user's `~/.ssh/config` is unchanged (that path was a symlink; now it's the real socket).

> **Note on granularity:** Stage 2 is largely mechanical removal across a 600-line state machine. Each task below names the exact symbols to remove/change and includes the non-obvious new code. The implementer should let `cargo build` errors drive the de-indexing (remove a field → fix each compile error), committing per task.

---

### Task 7: Collapse the ControlPath scheme to a single stable socket

**Files:**
- Modify: `auto2fa-rs/crates/a2fa-core/src/ssh/control.rs:223-304` (path + symlink helpers)
- Modify: `auto2fa-rs/crates/a2fa-core/src/ssh/master.rs:218-221` (`pool_path`)

- [ ] **Step 1: Make `control_path` return the stable base (drop the `-<index>` suffix)**

Change `control_path` (control.rs:223) so the socket path no longer appends `-{index}`:

```rust
/// Return the stable ControlPath for `host` (single master, no pool index).
/// This is the path the user's `ControlPath ~/.ssh/cm-auto2fa-%h` resolves to,
/// and the master binds it directly (no symlink indirection).
pub fn control_path(host: &str, _index: usize) -> PathBuf {
    resolve_control_base(host)
}
```

(Keeping the `_index` parameter avoids touching every caller in this task; callers are de-indexed in Task 8/9. `active_symlink_path` and `control_path` now return the same path.)

- [ ] **Step 2: Make symlink helpers no-ops / removable**

`update_symlink` (control.rs:244) is no longer needed for a single master. Replace its body so it does nothing and returns `true` (callers removed in Task 9), and add a deprecation note:

```rust
/// DEPRECATED (single-master): there is no symlink to rotate anymore — the
/// master binds the stable path directly. Retained as a no-op until callers are
/// removed in Task 9, then deleted.
pub fn update_symlink(_host: &str, _index: usize) -> bool {
    true
}
```

Leave `symlink_target_index`, `active_symlink_path`, `remove_symlink`, `parse_trailing_index` in place for now (boot janitor in Task 12 uses `remove_symlink`); they are removed when their last caller goes.

- [ ] **Step 3: Build (expect warnings, no errors)**

Run: `cd auto2fa-rs && cargo build -p a2fa-core 2>&1 | tail -10`
Expected: compiles; possibly `unused` warnings (fine for now).

- [ ] **Step 4: Run core tests; fix the path test if present**

Run: `cd auto2fa-rs && cargo test -p a2fa-core 2>&1 | tail -20`
Expected: green. If a test asserts the `-{index}` suffix (search `control_path` in tests), update it to expect the base path.

- [ ] **Step 5: Commit**

```bash
git add auto2fa-rs/crates/a2fa-core/src/ssh/control.rs
git commit -m "refactor(rust): single stable ControlPath (drop -N suffix; symlink rotation no-op)"
```

---

### Task 8: De-pool `PoolState` — one master, no rotation

**Files:**
- Modify: `auto2fa-rs/crates/a2fa-core/src/ssh/master.rs`

- [ ] **Step 1: Remove rotation fields/methods and de-array the per-slot state**

In `PoolState`:
- Remove fields: `active_index`, `last_rotate`, `probe_backoff_until`.
- Keep `slot_status`, `slot_ready_since`, `consecutive_probe_failures` but change them from `[T; POOL_SIZE]` arrays to single values (`SlotStatus`, `Option<Instant>`, `u32`). Rename the accessors to drop the index parameter, OR keep `POOL_SIZE = 1` and the array form to minimize churn.

**Decision (minimize churn, keep tests stable):** set `pub const POOL_SIZE: usize = 1;` and keep the array shape. Then:
- Remove `try_rotate` entirely.
- Remove `in_probe_backoff` and `ROTATION_PING_PONG_WINDOW` / `PROBE_BACKOFF` consts and `probe_backoff_until` field.
- In `reset_circuit_breakers`, drop the `probe_backoff_until = None` line.

- [ ] **Step 2: Build and let errors guide removal**

Run: `cd auto2fa-rs && cargo build -p a2fa-core 2>&1 | tail -30`
Expected: errors at each removed symbol's use site. Remove those uses (they are all rotation paths). Re-run until clean.

- [ ] **Step 3: Run core tests; delete rotation-only tests**

Run: `cd auto2fa-rs && cargo test -p a2fa-core 2>&1 | tail -20`
Expected: green after deleting tests that exercise `try_rotate`/ping-pong/`in_probe_backoff` (search `rotate`, `ping_pong`, `probe_backoff` in `master.rs` tests).

- [ ] **Step 4: Commit**

```bash
git add auto2fa-rs/crates/a2fa-core/src/ssh/master.rs
git commit -m "refactor(rust): POOL_SIZE=1, remove rotation/ping-pong state from PoolState"
```

---

### Task 9: Simplify `next_action` and `tick_host` (no WarmSlot1 / Rotate)

**Files:**
- Modify: `auto2fa-rs/crates/a2fa-daemon/src/managers.rs`

- [ ] **Step 1: Remove the `WarmSlot1` and `Rotate` arms**

In `next_action` (managers.rs:256):
- Delete the slot-1 warm-up block (271-275) and the rotation block (287-297).
- Delete the `WarmSlot1` and `Rotate` variants from `enum MaintenanceAction` (and their handler arms in `tick_host`, ~1366+ and the post-restart `try_rotate` call ~1344-1352).
- The `Init` arm of `needs_restart` (`SlotStatus::Init => slot == 0`) stays (still restarts a stuck Init on the single slot 0).
- Remove `!pool.in_probe_backoff()` from the `needs_restart` guard (the method is gone); keep `!pool.in_flap_backoff()`.

Resulting `next_action` arms: `Skip` (inactive / cooldown / flap-backoff via the restart guard) | `AdoptAlive` (down-but-probe-true) | `Restart` | `Healthy`.

- [ ] **Step 2: Remove the post-restart rotation block in the worker**

In the `hb-restart` worker, delete the `if slot == active_index { … try_rotate … }` block (~1344-1352) and any `active_index` references (the host has one slot).

- [ ] **Step 3: Build, let errors guide cleanup, test**

Run: `cd auto2fa-rs && cargo test -p a2fa-daemon 2>&1 | tail -25`
Expected: compiles; delete `next_action_*` tests that assert WarmSlot1/Rotate (search `WarmSlot1`, `Rotate`, `rotate_when` in `managers.rs` tests); keep/▶ the Skip/Restart/AdoptAlive/Healthy tests. All green.

- [ ] **Step 4: Commit**

```bash
git add auto2fa-rs/crates/a2fa-daemon/src/managers.rs
git commit -m "refactor(rust): single-master next_action (drop WarmSlot1/Rotate arms)"
```

---

### Task 10: De-index daemon State writes + remove the rotate IPC handler

**Files:**
- Modify: `auto2fa-rs/crates/a2fa-daemon/src/workers.rs:205-251`
- Modify: `auto2fa-rs/crates/a2fa-daemon/src/handlers/hosts.rs` (rotate handler ~520-582; `pool_index`/`pool_alive` writes)

- [ ] **Step 1: Pin pool fields to single-master values**

Everywhere the daemon sets `pool_index`/`pool_alive` (workers.rs:210-211,251; hosts.rs:42-44,582,706-708,1048-1049): set `pool_index = 0` always and `pool_alive = if is_master_ready { 1 } else { 0 }`. Keep the IPC JSON keys (`hosts.rs:42-44`) so existing clients still decode.

- [ ] **Step 2: Remove the manual rotate handler**

Delete the rotate command handler (hosts.rs:520-582) and its IPC registration/dispatch entry (search the method name, e.g. `rotate`, in the handler dispatch table). If a client sends it, the daemon returns a "not supported" error (or the command is removed from the client in Task 11).

- [ ] **Step 3: Build + test**

Run: `cd auto2fa-rs && cargo test -p a2fa-daemon 2>&1 | tail -20`
Expected: green.

- [ ] **Step 4: Commit**

```bash
git add auto2fa-rs/crates/a2fa-daemon/src/workers.rs auto2fa-rs/crates/a2fa-daemon/src/handlers/hosts.rs
git commit -m "refactor(rust): pin pool_index=0/pool_alive<=1; remove manual rotate handler"
```

---

### Task 11: Single-connection rendering in CLI + TUI

**Files:**
- Modify: `auto2fa-rs/crates/a2fa-cli/src/main.rs:174-194`
- Modify: `auto2fa-rs/crates/a2fa-tui/src/views/hosts.rs:42-43`

- [ ] **Step 1: CLI — replace `pool={idx}/{alive}` with a connected/disconnected word**

In `a2fa-cli/src/main.rs` (~179-194), drop the `pool_index`/`pool_alive` read and print a single status derived from `is_master_ready`:

```rust
        let connected = h.get("is_master_ready").and_then(|v| v.as_bool()).unwrap_or(false);
        let state = if connected { "connected" } else { "disconnected" };
        println!("  {glyph} {host:<40} {state}  {last_msg}");
```

- [ ] **Step 2: TUI — replace the `{pool_index}/{pool_alive}` cell**

In `a2fa-tui/src/views/hosts.rs:43`, replace:

```rust
            let pool = format!("{}/{}", h.pool_index, h.pool_alive);
```

with a single-state label:

```rust
            let pool = if h.is_master_ready { "●" } else { "○" }.to_string();
```

(Keep the variable name `pool` if it's referenced downstream, or rename to `conn` and update the use site.)

- [ ] **Step 3: Build + test both crates**

Run: `cd auto2fa-rs && cargo build -p a2fa-cli -p a2fa-tui 2>&1 | tail -10`
Expected: compiles clean.

- [ ] **Step 4: Commit**

```bash
git add auto2fa-rs/crates/a2fa-cli/src/main.rs auto2fa-rs/crates/a2fa-tui/src/views/hosts.rs
git commit -m "refactor(rust): CLI/TUI show single connection state, not pool x/y"
```

---

### Task 12: Boot janitor — sweep pre-upgrade pool sockets + symlinks + leaked masters

**Files:**
- Modify: `auto2fa-rs/crates/a2fa-daemon/src/handlers/hosts.rs` (or wherever the per-host boot/adopt path runs — search for `symlink_target_index` / boot adoption)

- [ ] **Step 1: On boot, for each host, retire the old scheme then establish the stable master**

In the boot/adoption path (where the daemon currently adopts via `symlink_target_index`), replace the adoption logic with:

```rust
    // One-time migration from the 2-slot+symlink scheme to a single stable
    // master. Order matters: probe the stable path FIRST (adopt a live master a
    // prior daemon left), and only sweep the retired -0/-1 sockets + the old
    // symlink when the stable path has no live listener.
    let base = a2fa_core::ssh::control::resolve_control_base(host);
    if a2fa_core::ssh::control::master_probe(&base)
        == a2fa_core::ssh::control::MasterLiveness::Alive
    {
        // A stable master is already up — adopt it, no 2FA.
        // (mark Ready in PoolState + State; existing adopt code path)
    } else {
        // Retire leftovers from the old scheme: the -0/-1 pool sockets, the old
        // symlink, and any leaked [mux] masters on those -N paths. These belong
        // to the retired design and have no live sessions we care about (the
        // stable path is dead). Then establish the single stable master.
        for idx in 0..2usize {
            let old = std::path::PathBuf::from(format!("{}-{idx}", base.display()));
            a2fa_core::ssh::control::cleanup_stale_socket(&old, host); // guarded: skips if a listener is somehow live
        }
        a2fa_core::ssh::control::remove_symlink(host); // remove the old symlink path if it's a symlink
        // ... establish the stable master (existing start path on the stable base) ...
    }
```

Adjust to the actual surrounding adopt/establish code. Key invariants: probe-then-act; the guarded `cleanup_stale_socket` from Task 5 still refuses to touch anything that has a live listener.

- [ ] **Step 2: Build + test**

Run: `cd auto2fa-rs && cargo test -p a2fa-daemon 2>&1 | tail -20`
Expected: green.

- [ ] **Step 3: Remove now-dead symlink helpers**

After this task, `update_symlink`/`symlink_target_index`/`active_symlink_path`/`parse_trailing_index` should have no remaining callers (search each). Delete the ones that are unused; keep `remove_symlink` (used here) and `resolve_control_base`/`control_path`.

Run: `cd auto2fa-rs && cargo build 2>&1 | tail -10` (fix unused-import/dead-code warnings).

- [ ] **Step 4: Commit**

```bash
git add auto2fa-rs/crates/a2fa-daemon/src/handlers/hosts.rs auto2fa-rs/crates/a2fa-core/src/ssh/control.rs
git commit -m "feat(rust): boot janitor migrates 2-slot+symlink scheme to single stable master"
```

---

### Task 13: Swift UI — single connection indicator

**Files:**
- Modify: `auto2fa-mac/Auto2FA/Views/Components/HostRow.swift:136-144` (`poolPips`)
- Modify: `auto2fa-mac/Auto2FA/Models/Host.swift:10-21` (keep decoding `pool_index`/`pool_alive` for back-compat; they're still emitted)
- Check: `auto2fa-mac/Auto2FA/FriendlyText.swift` ("pool" strings still translate correctly)

- [ ] **Step 1: Replace the `poolPips` "x/2" view with a single connection dot**

In `HostRow.swift`, replace `poolPips` (136-144) with a single indicator driven by the host's ready state (use the existing `is_master_ready`-derived property the row already has; if the row uses `host.poolAlive`, switch to the ready flag):

```swift
    private var connectionDot: some View {
        Image(systemName: host.isMasterReady ? "circle.fill" : "circle")
            .font(.system(size: 7))
            .foregroundStyle(host.isMasterReady ? Color.green : Color.secondary)
            .help(host.isMasterReady ? "Connected" : "Not connected")
    }
```

Update the row body (~91) to use `connectionDot` instead of `poolPips`. If `Host` has no `isMasterReady` property, add it from the `is_master_ready` JSON key (it is already in the IPC payload).

- [ ] **Step 2: Keep `Host` decoding stable**

`Host.swift` keeps `poolIndex`/`poolAlive` (still present in JSON) to avoid a decoder break, but they are no longer used in the UI. Add `isMasterReady` decoding if missing:

```swift
    let isMasterReady: Bool
    // CodingKeys: case isMasterReady = "is_master_ready"
```

- [ ] **Step 3: Build the app (build is the gate; SourceKit cross-file errors are false positives)**

Run: `cd auto2fa-mac && xcodegen generate >/dev/null 2>&1; xcodebuild -scheme Auto2FA -configuration Debug build 2>&1 | tail -15`
Expected: `** BUILD SUCCEEDED **`.

- [ ] **Step 4: Commit**

```bash
git add auto2fa-mac/Auto2FA/Views/Components/HostRow.swift auto2fa-mac/Auto2FA/Models/Host.swift
git commit -m "refactor(ui): single connection indicator (drop pool x/2 pips)"
```

---

### Task 14: Build, deploy Stage 2, and verify

**Files:** none (build + deploy + observe)

- [ ] **Step 1: Full workspace test + release build + app build**

Run: `cd auto2fa-rs && cargo test 2>&1 | tail -20 && cargo build --release 2>&1 | tail -5`
Expected: all green; `Finished release`.

- [ ] **Step 2: Package + deploy**

Run: `cd auto2fa-mac && ./package-app.sh 2>&1 | tail -30`
Expected: builds + installs the universal app + new daemon (bump the daemon bundle version if the installer reports a no-op).

- [ ] **Step 3: Verify single stable sockets + clean steady state**

Run: `ls -la ~/.ssh/ | grep cm-auto2fa; echo '---'; ps aux | grep -c '[c]m-auto2fa-.*-[01] '`
Expected: `cm-auto2fa-<host>` entries are **regular sockets** (`srw-`), NOT symlinks (`lrwx`), and there are **no** `-0`/`-1` pool sockets or `[mux]` masters on `-N` paths left.

Run: `sleep 120; tail -200 /tmp/auto2fa_daemon.log | grep -cE 'needs restart \(status=Ready|killed (DUPLICATE|orphaned)|Rotated symlink|marked Dead but master is ALIVE|pgrep exceeded 2s'`
Expected: **0**. Hosts read Connected; opening a new `ssh kempner` reuses the stable master with no 2FA prompt while it's alive.

- [ ] **Step 4: Final review + finish the branch**

Dispatch a final whole-change code review (spec compliance + quality), then use `superpowers:finishing-a-development-branch`.

```bash
git tag robustness-stage2
```

---

## Self-Review (plan vs spec)

**Spec coverage:**
- Two-layer separation (network→ssh keepalive; process→cheap probe) → Tasks 1, 3. ✓
- Cheap non-blocking probe → Task 1. ✓
- Hysteresis (threshold 3) → Tasks 2, 3. ✓
- Adopt-before-restart / never-kill-live → Tasks 4, 5. ✓
- `ssh -O check` off the hot path → Task 3 (replaced by `master_probe`). ✓
- Single stable master, no symlink/rotation → Tasks 7, 8, 9. ✓
- State/IPC back-compat + UI single indicator → Tasks 10, 11, 13. ✓
- Boot janitor for pre-upgrade leftovers → Task 12. ✓
- Retained cooldown + flap-backoff breakers → Task 8 keeps them (only rotation/ping-pong removed). ✓
- Staged rollout (de-thrash first, verified, then structural) → Stage 1 (Tasks 1-6) / Stage 2 (Tasks 7-14). ✓
- Tests for probe mapping, hysteresis, teardown safety → Tasks 1, 2, 5. ✓

**Placeholder scan:** No TBD/TODO. Mechanical-removal tasks (8, 9, 12) name exact symbols and let `cargo build` errors drive de-indexing — concrete, not vague.

**Type consistency:** `MasterLiveness {Alive,Dead,Inconclusive}` (Task 1) is used consistently in Tasks 2-5, 12. `probe_to_check(MasterLiveness, u32, u32) -> Option<bool>` and `note_probe_result(usize, MasterLiveness)` (Task 2) match their call sites (Task 3). `PROBE_FAILURE_THRESHOLD` (Task 2) used in Task 3. `consecutive_probe_failures` array (Task 2) used in Tasks 3, 4. Decision to keep `POOL_SIZE = 1` + array shape (Task 8) keeps these signatures valid into Stage 2.

**Known soft spots (acceptable):** Tasks 9/10/12 depend on exact surrounding code the implementer will see at edit time (handler dispatch table name, boot adopt block). The tasks specify the invariants and the new code; the implementer wires them to the real call sites. This is inherent to refactoring a live state machine and is called out explicitly.
