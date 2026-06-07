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
