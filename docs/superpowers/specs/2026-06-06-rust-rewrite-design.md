# Rust Rewrite — Complete Design (one-step, feature-complete)

**Status:** Design approved, pending implementation plan
**Date:** 2026-06-06
**Goal:** Replace the Python backend (daemon + CLI + TUI) with a Rust implementation that is feature-complete with today's behavior, ships as small static binaries, and lets the Swift Mac app embed the daemon directly (no bundled CPython). The product ships with **zero Python**.

This is a **one-step** design: the final architecture is fixed here and implemented to full feature parity in one coherent effort. There is **no transitional / phased / half-Python-half-Rust shipped state**. Python is kept during development only as a correctness oracle for testing, then deleted.

## Why / scope

- The Python daemon is functionally fine and light at runtime (~19 MB, ~1% CPU), but making the Mac app self-contained requires bundling CPython (~68 MB). A Rust daemon is a single ~5-10 MB static binary with no runtime to bundle.
- Rust's ownership / `Send`+`Sync` guarantees eliminate, at compile time, the class of concurrency bugs this codebase has hit (dict-iteration races, check-then-act races).
- Scope: rewrite **daemon, CLI, and TUI** in Rust. The Swift Mac app is unchanged (it is a socket client; it only swaps the embedded daemon binary).

Non-goals:
- No new features. Exact behavioral parity with the current Python (the 28 IPC methods + their semantics).
- No Linux/Windows in this spec (the design keeps platform-specific code isolated so a later port is possible, but only macOS is targeted now).
- No protocol changes — the IPC wire format stays byte-compatible so the Swift app needs no changes.

## Workspace / crate layout

A single Cargo workspace. Domain logic lives in one library crate; three thin binaries depend on it.

```
auto2fa-rs/
  Cargo.toml                      # [workspace]
  crates/
    a2fa-core/                    # library — all domain logic, unit-testable
      src/
        lib.rs
        proto.rs                  # IPC types: Method/Event/ErrCode, Request/Response/Event (serde)
        model.rs                  # Host, Tunnel, state enums; HostName/Port newtypes
        config.rs                 # config_dir(), passwords.json + tunnels.json load/save (serde, atomic write)
        creds.rs                  # macOS Keychain (keyring crate) + schema migration
        totp.rs                   # TOTP generation (totp-rs)
        ssh.rs                    # ControlMaster orchestration + pty auth (portable-pty)  ← hardest
        tunnels.rs                # ssh -L forwards, port probe, Slurm squeue discovery, post-connect hooks
        engine.rs                 # State, the scheduler/tick loop, cooldown/backoff/heartbeat/wake-recover
        error.rs                  # thiserror domain error → ErrCode mapping
    ssh2fa-daemon/                  # bin — unix-socket server, flock single-instance, wires engine + proto
      src/main.rs
    a2fa-cli/                     # bin — thin socket client; the 28 methods as subcommands (clap)
      src/main.rs
    a2fa-tui/                     # bin — ratatui terminal UI (socket client)
      src/main.rs
```

`proto.rs` lives in `a2fa-core` as a module (not a separate crate) — splitting it out is only worth it if client compile times bite, which we do not assume up front (YAGNI).

### Key crates
- `serde` + `serde_json` — protocol + config.
- `portable-pty` — spawn ssh in a pty and drive the password/OTP prompts.
- `keyring` — macOS Keychain (generic password items), matching today's service/account scheme.
- `totp-rs` — TOTP.
- `clap` — CLI argument parsing.
- `ratatui` + `crossterm` — TUI.
- `thiserror` (library errors) + `anyhow` (binary `main`s).
- `log` + `simplelog` — file logging to `/tmp/ssh2fa_daemon.log` (parity with today).
- `fs2` — advisory file lock (`try_lock_exclusive`) for the single-instance guard.
- `regex` — squeue/prompt parsing.

Tooling baseline: `rustfmt` + `clippy` (CI runs `clippy -- -D warnings`). `cargo-deny`/`proptest`/full CI are optional add-ons, not required for parity.

## Concurrency & state model

Synchronous threads — **no async/tokio** (the daemon manages a handful of hosts/tunnels; the work is blocking subprocess/pty I/O, not high-fan-out non-blocking I/O).

- **One `Mutex<State>`** holds the whole registry (hosts, tunnels, subscribers). It is held **only for fast critical sections** — read the fields you need, drop the guard, then run blocking ssh/pty work, then briefly re-lock to write results back. **The lock is never held across ssh I/O.** This gives data-race safety (Rust won't let you touch the data unlocked) with no lock-ordering deadlock (a single lock) and no actor/channel framework.
- **Worker threads**: one per host manager (mirrors the proven Python structure) for the master-connection lifecycle; tunnels start/stop on short-lived worker threads. The poll/tick loop runs on its own thread (every 0.5 s, same as today).
- **OTP serialization**: hosts that share a TOTP secret must not submit codes concurrently (a real, already-fixed behavior). A per-secret `Mutex` (keyed by secret) serializes OTP submission across hosts — held only around the submit window.
- **IPC server**: a listener thread accepts connections; one thread per connection. Event subscribers are `StreamWriter`-equivalents stored in `State`; the tick loop emits change events to them.
- **Single instance**: `fs2` exclusive lock on `~/.ssh2fa/lock` at startup; if held, exit cleanly (parity with the flock guard added to the Python daemon).

## IPC protocol contract (the linchpin — byte-compatible)

Transport: line-delimited JSON over a Unix domain socket at `~/.ssh2fa/ssh2fa.sock`, `chmod 0600`. Request: `{"id","method","params"}`. Response: `{"id","result"}` or `{"id","error":{"code","message"}}`. Event (to subscribers): `{"event","data"}`. Invalid UTF-8 / bad JSON → `invalid_request` error, connection stays alive.

**Methods (28), implemented to parity:**
`ping`, `list_hosts`, `list_tunnels`, `host_toggle`, `host_mount_toggle`, `host_rotate`, `host_add`, `host_test_credentials`, `tunnel_add`, `tunnel_remove`, `tunnel_toggle`, `tunnel_start`, `tunnel_stop`, `tunnel_set_node`, `tunnel_set_autostart`, `tunnel_set_jump_candidates`, `tunnel_set_post_connect`, `tunnel_set_tags`, `tunnel_set_url_path`, `tunnel_rename`, `tunnels_batch`, `discover_nodes`, `port_suggest`, `wake_recover`, `reset_all`, `log_tail`, `tunnel_events`, `subscribe_events`.

**Events (3):** `host_status_changed`, `tunnel_status_changed`, `notification`.

**Error codes (8):** `invalid_request`, `unknown_method`, `bad_params`, `not_found`, `port_in_use`, `duplicate`, `discovery_failed`, `internal`.

The exact request params, response shapes, and snapshot field names are captured from the current `auto2fa/ipc.py` + `auto2fa/daemon.py` handlers and frozen in `proto.rs` (and asserted by the conformance harness, below).

## Domain model

- `HostName` — newtype; constructor rejects `/`, `..`, leading dot, empty (kills the path-traversal class at the type level). Used as Keychain account, ssh alias, `/tmp` log name, `~/Mounts/<host>`.
- `Port` — newtype; constructor enforces 1024–65535.
- `Host` — alias, status, active, master pool (`ControlMaster` 0/1), `is_master_ready`, `is_mounted`, cooldown/backoff timestamps, OTP secret ref.
- `Tunnel` — name, local/remote `Port`, jump candidates, last_node/user, status (idle/starting/alive/failed/port_busy/stale), active_jump, auto_start, post_connect_cmd, tags, url_path, wants_alive, uptime accounting (`total_uptime_sec` + `alive_since`), last_alive_at.
- Status snapshots match the Python `_host_snapshot`/`_tunnel_snapshot` field names. **Change detection for events uses a stable-field key that excludes live-computed `total_uptime_sec`** (parity with the current daemon — do not regress the "event every 0.5 s" bug).

## SSH core (the hard part)

Keep the system `ssh` + `ControlMaster` (so terminal `ssh host` stays instant and password-less). Per host: a pool of up to 2 masters (`-M -S <controlpath>`), `ssh -O check` / `-O exit` for control, a symlink that points the active control path at the live pool member (parity with today's rotation).

Authentication (the pexpect replacement): spawn ssh in a pty via `portable-pty`, run an expect loop — read until a password prompt regex → write the Keychain password; read until the OTP/verification-code prompt → write a fresh TOTP. Reuse the per-secret OTP lock + the "fresh code / wait for next window" logic (don't replay a code within its 30 s window). Honor cooldown after consecutive login failures and probe back-off, exactly as today. Capture the pty output to extract failure reasons (e.g. "Permission denied").

Heartbeat: periodically `ssh -O check` the active master; tolerate transient failures (the widened keepalive / forgive-momentary-stall behavior already tuned in Python); rotate/rebuild on real death.

## Tunnels

- `tunnel_add`: validate name (`HostName`-style) + ports (`Port`), refuse a port already bound on 127.0.0.1, persist atomically; global add-lock so two concurrent adds can't both pass the duplicate check.
- start: spawn `ssh -N -J <jump> -L local:localhost:remote …`; probe `127.0.0.1:local` until it accepts or times out; on success mark alive, persist `wants_alive`, run the post-connect hook (threaded, no double-spawn); on failure terminate the child and mark failed. A probe that *raises* must still terminate the child (parity with the leak fix).
- node discovery: `squeue -h -o '%i|%P|%j|%T|%M|%R'` on the jump host, parsed with regex; `DiscoveryError` → `discovery_failed`. Manual `tunnel_set_node` always works without discovery.
- wake-recover & auto-recover: re-probe and rebuild tunnels the user wanted alive after sleep/flap; snapshot the dict before iterating (no "changed size during iteration").
- rename: atomic under the add-lock; migrate the per-resource bookkeeping.

## Engine / tick loop

A 0.5 s tick (own thread) that: runs per-tunnel maintenance (probe/recover) off-lock, computes host & tunnel snapshots, and emits `*_status_changed` events only when the **stable-field key** changes. Periodic log-rotation check. Graceful shutdown sets a stop flag, joins workers with a deadline, and tears down masters/tunnels.

## Daemon binary

`ssh2fa-daemon`: acquire the single-instance lock; remove a stale socket; bind + `chmod 0600`; load config (Keychain + json) honoring `config_dir()` fallback to `~/.ssh`; start host workers + tick loop; serve IPC. Logs to `/tmp/ssh2fa_daemon.log`.

## CLI binary

`a2fa-cli` (installed as `auto2fa`): `clap` subcommands mirroring today (`list`, `hosts`, `tunnels`, `start`, `stop`, `toggle`, `node`, `wake`, `logs`, `raw`, plus the rest of the 28 as needed). No-arg launches the TUI (parity). Connects to the socket, sends one request, prints the result; clean errors on `OSError`/timeout/malformed JSON (parity with the hardened Python CLI).

## TUI binary

`a2fa-tui`: `ratatui` + `crossterm`. Hosts table + tunnels table, status colors, start/stop/toggle keybindings, node picker, add-host / new-tunnel flows, log viewer — feature parity with the Textual TUI. It is a socket client (subscribes to events, renders snapshots). **Status indicators are static or low-frequency** (do not reintroduce a perpetual-animation CPU drain).

## Build & distribution

- `cargo build --release` → three static binaries. Universal: build `aarch64-apple-darwin` + `x86_64-apple-darwin`, `lipo` together.
- The Swift app embeds `ssh2fa-daemon` (replaces the PyInstaller/CPython bundle — the 68 MB problem is gone). The `SMAppService` agent / launch path from the P1 design points at the embedded Rust binary.
- `auto2fa` (CLI) + `auto2fa-tui` ship as small standalone binaries.
- The P0 installer is reworked to not depend on Python (it becomes trivial: drop the binaries in place + register the agent).

## Error handling, logging, types

- `a2fa-core` defines a `thiserror` `Error` enum; a `to_errcode()` maps each variant to one of the 8 IPC `ErrCode`s. Binaries use `anyhow` in `main`. No `unwrap()`/`expect()` on reachable runtime paths.
- `log` facade + `simplelog` writing to the daemon log file.
- Newtypes (`HostName`, `Port`) make illegal values unrepresentable at the boundary.

## Feature-parity checklist

Implemented and verified against the Python oracle:
1. All 28 IPC methods, exact params/response/error-code semantics.
2. ControlMaster pool (size 2), rotation, symlink active-path, `-O check/exit`.
3. Interactive password + TOTP auth via pty; fresh-code/no-replay; per-secret OTP serialization.
4. Cooldown after N consecutive failures; probe back-off; toggle clears cooldowns.
5. Heartbeat with transient-failure tolerance; master rebuild on real death.
6. Tunnels: add/remove/start/stop/toggle/rename, port validation + in-use check, atomic persist, post-connect hooks, tags, url_path, autostart, jump candidates, batch ops.
7. Slurm `squeue` node discovery + manual node set.
8. wake-recover / auto-recover; `wants_alive` persisted across daemon restart.
9. sshfs mount toggle (`~/Mounts/<host>`, host-name validated).
10. `reset_all`, `port_suggest`, `host_test_credentials`, `discover_nodes`, `log_tail`, `tunnel_events`.
11. Event emission with stable-field change detection (no uptime-driven event storm).
12. Single-instance flock; socket perms 0600; `config_dir()` → `~/.ssh` fallback.
13. No personal defaults / paths / node names anywhere (de-personalized).

## Testing & optimization (during implementation)

- **Unit tests** per module (config round-trip, TOTP vectors, change-key stability, HostName/Port validation, squeue parsing, state-machine transitions via small tests).
- **Conformance harness**: a dev-only test that drives the same request sequences against both the Python daemon and the Rust daemon over the socket and asserts equal responses/snapshots. The Python daemon is the oracle (deleted after parity is reached).
- **Integration**: against a real jump host (the maintainer's clusters) and the live Swift app as a client — end-to-end connect / tunnel / mount.
- **Optimization** (last): binary size (`strip`, `lto`, `opt-level="z"` if needed), idle RSS, startup time.

## Cutover (end of implementation, single switch)

When the Rust daemon passes the conformance harness + Swift app e2e and the CLI/TUI reach parity: point the Swift app's embedded daemon + the install path at the Rust binaries, delete the Python package (`auto2fa/`), the PyInstaller packaging, and the Python tests. One commit flips the product to Rust.

## Risks

- **pty auth (ssh password+OTP)** is the highest-risk port — prototype `a2fa-core::ssh` first against a real host before building the rest on top.
- **ControlMaster** semantics (symlink rotation, `-O` control) must match exactly or "instant `ssh host`" breaks — covered by the integration test.
- **Keychain** access from a non-GUI launchd context can prompt/388 — verify the bundled daemon reads existing items (parity with current behavior).
- Scope is large (~5000 lines Rust incl. a TUI); the implementation plan sequences modules so each is testable as it lands, but the deliverable is the complete system.
