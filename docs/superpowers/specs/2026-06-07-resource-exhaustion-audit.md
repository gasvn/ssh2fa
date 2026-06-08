# Resource-Exhaustion / Machine-Hang Bug-Class Audit & Fixes

**Date:** 2026-06-07
**Trigger:** the Rust daemon hung the whole machine (load 20+). Root cause was a runaway
unbounded spawn; the same *class* had also bitten earlier. A multi-agent audit (91 agents)
swept the whole codebase for the class. **All confirmed instances are fixed** on branch
`rust-rewrite` (not deployed — code + tests only).

## The bug class

**"A trigger spawns blocking work without (a) claiming an in-flight/state token first,
(b) a hard timeout, and (c) reaping the child on every exit path."** Three sub-forms:
1. **spawn-without-in-flight-guard** — a loop/handler spawns a thread/process every
   trigger while the previous one is still running → pile-up → exhaustion.
2. **kill-without-wait / no-reap** — a child is killed but never `wait()`ed → zombie + fd leak.
3. **block-on-shared-path-without-timeout** — blocking ssh/keychain on an orchestration or
   handler thread with no deadline → freeze.

Notable: the **Python version was correct here** (per-index locks, synchronous bodies,
timeouts). The class was a **Rust-port regression** — the maintenance loops got guards, but
the IPC handlers and the pty-login path did not inherit them.

## Confirmed findings & fixes (all done)

| # | Issue | Fix | Commit |
|---|---|---|---|
| pre | Heartbeat spawned a login worker every 3s for a Dead slot (no in-flight guard) → the original machine hang | `HostManagers.starting` + `try_begin_start` + RAII `StartGuard` on all 5 master-start sites | `ba175d4` |
| C1/C2 | squeue ssh unbounded + run inline on the maintenance loop thread → loop freeze | `ConnectTimeout`/`BatchMode` + 15s kill-on-deadline + dispatch off-loop (throttle claimed first) | `e1049c7` |
| 3 (P0) | `run_login` killed ssh child but never `wait()`ed → zombie + pty-fd leak EVERY login | RAII `ChildReaper`: `wait()`-only on success (master daemonized via ControlPersist), `kill()+wait()` on failure; + circuit-breaker on system/totp `Err` | `3135bcd` |
| 1 (P1) | IPC `tunnel_start` spawned `ssh -L` per call (only `Alive` deduped) | idempotent on `Alive\|Starting` (atomic under one State lock); `store_child` reaps prior | `83f4a0b` |
| 2,6,7 (P1/P3) | `subscribe_events` unbounded threads/Senders; uncapped connections; unbounded emit channel | subscribe-once per conn + `MAX_SUBSCRIBERS=64` + `MAX_CONNS=128` (RAII) + `sync_channel(1024)` + non-blocking `try_send` emit + 5s write timeout | `f2ea01a` |
| 5 (P2) | `ssh -O exit` / `cleanup_stale_socket` unbounded (run before every login + under map lock) | `run_ssh_bounded` chokepoint (5s kill-on-deadline) for check/exit; `teardown_all` drops the map lock across I/O | `368d7d9` |
| 4 (P1) | `wake_recover` no throttle; 2 Mac monitors fire it overlapping | daemon `WakeRecoverGuard` (in-flight + 12s debounce, RAII clear) + Swift debounce | `4a97465` |
| 8,9 (P3) | retained `ssh -N` stderr pipe could stall; `host_totp` Keychain on handler thread | stderr → `Stdio::null()`; `host_totp` Keychain read on a bounded (5s) worker thread | `2933cef` |

## The invariant that closes the whole class (going forward)

**Every blocking external spawn (ssh master / ssh -L / ssh -O / pty login / Keychain) must:**
1. **claim a per-resource in-flight token before spawning** (HashSet/AtomicBool/`Starting`
   state), released via RAII on every path;
2. **enforce a hard timeout** (kill-on-deadline), never an unbounded `.output()/.status()`;
3. **`kill()+wait()`-reap the child on every exit path** (RAII), never kill-without-wait;
4. **never run on an orchestration or IPC-handler thread** — dispatch to a short-lived worker;
5. **bound every collection** (subscribers, connections, channels, registries).

Chokepoints now enforcing this: `try_begin_start`/`StartGuard` (master starts),
`run_ssh_bounded` (control-channel ssh), `ChildReaper` (pty login), the squeue 15s+off-loop
path, `WakeRecoverGuard`, and the IPC server caps. **Future ssh/spawn code should route
through these patterns, not re-implement bare `Command::output()`.**

## Verification

Full workspace, single-threaded: **a2fa-core 122, a2fa-daemon 140, cli 15, tui 36** — 0 failures.
Release compiles. Nothing deployed. Audit: 16 confirmed candidates → 9 distinct issues fixed;
64 sites checked & cleared (incl. the Python daemon, the Swift timers, the already-guarded
maintenance loops).

---

## Round 2 (deep stability sweep) + Rounds 3-4 (fixing the fixes) — 2026-06-07

A second, deeper multi-agent sweep (the user asked twice for "仔细仔细检查") verified the
round-1 fixes AND widened to the full daemon-stability class (panics, deadlocks, lock-ordering,
poison, block-forever). It found **24 issues — including 3 regressions that round-1 introduced**:

- **CRITICAL (mine):** the round-1 `ChildReaper` `wait()`-on-success **blocked forever on every
  login** (interactive argv + `ControlPersist`: the foreground ssh client never exits) → slot
  wedged + thread/child/pty leak per login. Worse than the bug it replaced.
- **HIGH (mine):** the in-flight guard `.expect()`d on `Builder::spawn` → a transient EAGAIN
  **panicked the heartbeat thread for the daemon's life** + leaked the token (5 sites).
- **MED (mine):** the `tunnel_start` latch `.expect()`d on spawn → tunnel stuck `Starting` forever.

Plus a systemic gap: **no `catch_unwind` + non-poison-tolerant loop locks** (one panic under
`Mutex<State>` poisons it → cascading death), and **`.expect()`/`?` on spawns / unbounded
`wait()`s** turning transient errors into crashes.

**Round-2 fixes** (`037c0cc` 1f0d6b3 5bc2d0f 73eb8eb 61effc1) addressed all 24 + the 3
regressions. A **round-2 verification** then found round-2 *itself* left 4 residuals (the
host_totp fix repeated the bare-`thread::spawn` latch-leak; ChildReaper detach left one zombie
per login; server line-read was post-hoc-bounded + truncating). **Round-3** (`6be0c1e`) fixed
those: ChildReaper now **takes the child → drops pty fds (SIGHUP backgrounds the master) →
reaps on a detached thread** (no block, no zombie, master survives); host_totp releases its
latch on spawn-Err; server read is truly bounded + non-truncating. **Round-4** (`bc47af8`)
converted the last 2 production bare `thread::spawn` to `Builder` — **production now has ZERO
bare spawns.**

A final read-only verification returned **GO**: all checks PASS, the ChildReaper's two prior
failure modes are structurally impossible (take-before-drop + detached wait), and no round-2/3/4
fix broke normal operation (login, master pool, tunnels, events all verified intact).

### Invariant — EXTENDED

To the original (in-flight guard + hard timeout + reap-on-every-path + off-loop + bounded
collections), the audits added two rules that the fixes now follow everywhere:
6. **Never `.expect()`/`?` a thread spawn; never `wait()` without a deadline (or on a detached
   thread).** Every production spawn is `Builder::spawn` with the `Err` handled (release token /
   reset status / fall back) — zero bare `thread::spawn` in production.
7. **No panic while holding a lock; every long-lived loop is poison-tolerant.** Dispatch is
   `catch_unwind`-wrapped (removes the poison source); both core loops use `lock_state()`
   (`into_inner()` recovery) + per-tick `catch_unwind` (survive + continue).

### Status
Full workspace single-threaded: a2fa-core 125, a2fa-daemon 143, cli 15, tui 36 — 0 failures;
Python compiles; release compiles. `rust-rewrite` ~72 commits ahead of main. **Not deployed.**
Verdict: **GO** — no known reachable hang/crash/leak/deadlock; normal operation verified intact.

---

## Live bring-up findings — Keychain prompt storm + fd limit — 2026-06-07 (evening)

The signed daemon was deployed and run live. The user reported "countless Keychain
prompts — this is not normal." Systematic debugging (no guessing) of the live process:

**Evidence gathered (read-only first):** single daemon, correctly signed with the stable
Apple Development identity, **0 ssh procs / 25 fds** at snapshot. The cumulative log
(`/tmp/auto2fa_daemon.log`, **appended across 29 daemon restarts today**) showed ~8k
`spawning ssh master` with ~6k `dup of fd failed` + ~2k `Too many open files`. Splitting
by restart boundary: **all 2039 fd-exhaustion failures and the bulk of the cred reads were
in the 28 earlier (pre-round-3, buggy) instances; ZERO after the current binary's restart**
(only 6 cred reads). So the historical fd exhaustion was the already-fixed pty leak from
earlier builds — not the current code.

**Disproved the obvious hypothesis with a probe.** lsof showed leaked-looking pty pairs, so
the suspicion was a residual pty-fd leak in `run_login`'s success path. A 3-variant
`fd_leak_probe` (fast-exit child; lingering child; **ControlPersist-shaped grandchild** that
inherits fds while the foreground exits) held the daemon fd count **flat across 30 cycles**;
the master fd is **O_CLOEXEC**. The current pty teardown does NOT leak. (Probe removed after.)

**Actual root causes of the prompt storm:**
1. `load_creds` re-read the Keychain on **every** login attempt (no cache). A host whose
   login keeps failing re-reads every ~3s; macOS re-prompts whenever the binary's signature
   isn't authorized → flood. Worsened by 29 restarts + redeploys today.
2. **Signing identifier churn** — signing temp files (`/tmp/a2fa-live`) gives codesign a
   filename-derived identifier (`a2fa-live`); the Keychain ACL is keyed on the designated
   requirement, so a changed identifier re-prompts even with the same stable cert.
3. **launchd soft RLIMIT_NOFILE = 256** — too low; once hit, every spawn fails.

**Fixes (commit on `rust-rewrite`, tested):**
- `managers.rs`: process-lifetime credential cache (read Keychain ≤ once/host/lifetime; only
  complete creds cached; `invalidate_creds_cache` from `host_add`). Poison-tolerant.
- `sys.rs` (new): `raise_fd_limit()` lifts soft NOFILE toward 8192 (capped at hard/kernel),
  called first in `main()`.
- Deploy procedure: sign with **pinned** `--identifier com.auto2fa.daemon`; optional plist
  `SoftResourceLimits NumberOfFiles`.

Tests: a2fa-core **127** (+2 sys), a2fa-daemon **147** (+4 creds), cli 15, tui 36 — 0 failures.
The bug-class invariant is unchanged; this adds **"resolve each external secret once and
cache it"** + **"raise the process fd limit at startup"** as standing rules.

---

## Round 5 — 140-agent crash/hang audit (zero machine-hang tolerated) — 2026-06-07 (night)

User mandate: "cannot take any risk of crashing the system again." A 140-agent
adversarially-verified Workflow swept 10 hang/crash dimensions (unbounded-spawn, fd-leak,
blocking-no-timeout, panic/poison, deadlock/lock-order, unbounded-growth, busy-spin,
fork-amplification, launchd-crashloop, system-wide-resource), with **3 skeptics per finding**
(reachability / already-guarded / real machine impact) and a completeness critic + 2nd round.
**25 raw → 18 confirmed (round 1); critic found 6 gaps → 16 raw → 3 confirmed (round 2);
21 total confirmed, 4 system-threatening.** Every finding re-verified against current code
before fixing.

Fixed (commits on `rust-rewrite`):
| Severity | Finding | Fix |
|---|---|---|
| CRIT/sys | host_mount_toggle: sshfs/umount/which unbounded `.output()` on handler thread, no ConnectTimeout, no in-flight latch → macFUSE half-mount can freeze Finder/Spotlight machine-wide; repeated toggles stack sshfs subtrees | ConnectTimeout=10 in sshfs opts + `run_cmd_bounded` (kill-on-deadline) + per-host `MountInFlightGuard` |
| HIGH/sys | pty_auth login `buf` grows unbounded ≤60s (heap bomb + O(n²) regex) | `cap_transcript` 256 KiB trailing window |
| HIGH/sys | heartbeat + tunnel-maintenance loop-thread spawns `.expect()` on main thread → EAGAIN-after-boot-storm crashloop | log-and-continue (degrade, never crash); same for tick spawn (was `?`→exit) |
| CRIT | startup Keychain v1→v2 migration unbounded on main thread before accept → locked-Keychain boot wedge/crashloop | run on worker w/ 15s join timeout |
| HIGH | cleanup_orphans pgrep/ps/kill unbounded on boot thread before accept | `run_cmd_bounded` (2s/2s/1s) |
| HIGH | State Mutex held across `save_tunnels()` fsync (7 handler sites + maintenance + workers) → whole-daemon wedge under disk/FS pressure | snapshot off-lock (`persist_tunnels` / clone-then-save) |
| HIGH | ssh master `-v` → unbounded `/tmp` logs (8 MB/slot, never rotated) → slow /tmp-full machine stall | drop `-v`; truncate per-slot log each start |
| HIGH | subscribers::register raw `.lock().unwrap()` outside catch_unwind → poison = permanent event-sub outage | `lock_state`; + 9 raw unwraps in tunnel_maintenance |
| MED | main.rs panics if log file can't open → fast crashloop | stderr fallback, continue |
| MED | rotated `.gz` logs never pruned | keep newest 3 |
| LOW | kill_all_children holds runtime lock across child.wait() | drain under lock, kill+reap off-lock |

New helper: `a2fa_core::sys::run_cmd_bounded` (generic kill-on-deadline + reap) — the non-ssh
sibling of `run_ssh_bounded`, now a chokepoint for which/umount/sshfs/pgrep/ps/kill.

**Accepted residuals (NOT machine-hang vectors, deferred):** #13 host_add Keychain write inline
on handler thread (user-initiated, one conn, bounded by MAX_CONNS); #21 cleanup_orphans can't
reap removed-port tunnels / orphan ControlMasters (functional gap, not a hang).

INVARIANT EXTENDED: **non-ssh external commands also go through a bounded chokepoint**
(`run_cmd_bounded`), **the boot/main thread does no unbounded blocking before the accept loop**
(migration + cleanup on bounded workers), and **loop-thread spawns degrade, never `.expect`/`?`
to process exit** (no launchd crashloop).

Tests: a2fa-core **133**, a2fa-daemon **150** (+ mount latch, cap_transcript, run_cmd_bounded,
log-prune tests), cli 15, tui 36 — 0 failures; clippy no new errors. Not yet deployed.

### Round 5b — fix-verification pass (do the fixes regress?) — 2026-06-07

A 25-agent adversarial pass reviewed the Round-5 fixes for self-introduced regressions (one
reviewer per fix unit hunting new-hang / incomplete-fix / normal-op-regression, then 3 skeptics
per suspected regression: reachable / actually-worse-than-prefix / real severity). **The run hit
the session token limit and 6 verify agents died**, so the verdict is PARTIAL — but it caught
the two that matter:

1. **CONFIRMED regression — migration worker race (medium).** The bounded migration worker
   called `save_meta` itself; on the 15s-timeout path it was abandoned but stayed alive and
   could `save_meta` passwords.json AFTER the accept loop opened, racing a concurrent `host_add`
   `save_meta` (non-atomic RMW) → last-writer-wins lost-update of host metadata (only reachable
   on a first-ever v1→v2 migration with a locked Keychain; this machine is already v2). FIXED:
   split `migrate` into `prepare_migration` (Keychain writes, returns the v2 map, no persist) +
   wrapper; the daemon worker only does Keychain writes, the BOOT thread does the single
   `save_meta` before State::new/accept. Abandoned worker never persists → no race.
2. **Latent hang — `run_cmd_bounded` post-exit read (reproduced, verify incomplete).** A
   reviewer reproduced (>8s) that `read_to_end` blocks if a daemonized grandchild holds the
   pipe; does not fire on this machine's sshfs (redirects fds on daemonize) but fragile across
   variants. FIXED defensively: drain pipes NON-BLOCKING (O_NONBLOCK, read to WouldBlock/EOF) —
   never wait for EOF.

Other fix units (pty cap, spawn-degrade, persist-off-lock, lock_state conversions, master log,
main stderr-fallback, kill_all_children, mount latch) generated few/no suspected regressions and
were judged sound. **Accepted residual (low, non-hang):** a failed/timed-out sshfs leaves a
daemonized go-nfsv4/FUSE process that cleanup_orphans (matches only `ssh -N -J … -L`) doesn't
reap — a slow resource leak under repeated failed mounts, not a machine hang.

Tests after 5b: a2fa-core **134**, a2fa-daemon **150**, cli 15, tui 36 — 0 failures; clippy clean.
