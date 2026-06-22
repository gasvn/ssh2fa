# Rust Rewrite — Feature-Parity Completion Record

**Date:** 2026-06-06
**Branch:** `rust-rewrite`
**Status:** Tier 1 + Tier 2 + Tier 3 complete. Cutover (T16) remains, gated on a clean cluster window + user-supervised live e2e.

This records the gap-closing work done after the initial Rust rewrite, bringing the
Rust daemon/CLI/TUI to feature parity with the Python implementation.

## Tier 1 — autonomous management (done earlier)

- Persistent `HostManagers` registry; boot auto-start of active hosts.
- Heartbeat / auto-reconnect loop (dead master → restart, slot-1 warmup, rotation).
- Tunnel maintenance: `wants_alive` auto-recovery, child-died/ghost-alive detection,
  `ssh -L` child registry + kill-on-stop, squeue/stale detection, boot auto-start.
- Persistent cooldown / circuit breakers.
- Lowercase wire event names (`host_status_changed`, `tunnel_status_changed`,
  `notification`) — fixes a subscription-breaking casing bug.

## Tier 2 — robustness / parity

| Feature | Python ref | Rust location |
|---|---|---|
| `expand_first_node` (SLURM range `holygpu[01-03]`→`holygpu01`) | `tunnels.py:42` | `a2fa-core/tunnels/discovery.rs`; applied in daemon `tunnel_set_node` |
| Per-tunnel event ring buffer (≤200) | `tunnels.py:_record` | `ssh2fa-daemon/tunnel_runtime.rs` (runtime-only, not core model); `tunnel_events` handler returns real events |
| Daemon log rotation at startup (gzip >10MB) | `daemon.py:38` | `ssh2fa-daemon/log_rotation.rs` (flate2) |
| `cleanup_orphans` on boot (reap stray `ssh -N -J -L`) | `tunnels.py:944` | `a2fa-core/tunnels/cleanup.rs`; called in `server::run` |
| Real `wake_recover` + `reset_all` (master rebuild) | `daemon.py:911-1055` | `ssh2fa-daemon/handlers/system.rs` + `managers::spawn_master_rebuild`/`rebuild_masters` |
| Keychain migration v1→v2 on boot + one-time `.pre-keychain-backup` | `credentials.py:200-330` | `a2fa-core/creds/migrate.rs::migrate_passwords_file_if_needed`; wired in `server::run` before `State::new` |
| Graceful shutdown (SIGINT/SIGTERM teardown) | `daemon.py:1200` | `ssh2fa-daemon/server.rs` signal thread + `HostManagers::teardown_all` + `TunnelRuntime::kill_all_children` (signal-hook) |

### Design divergences (intentional, documented in code)

- **`wake_recover` retry:** Python schedules a one-shot asyncio backoff
  (`WAKE_RETRY_DELAYS = 10/20/30/60/120s`). The Rust port instead sets
  `wants_alive = true` on tunnels needing restart and lets the always-on
  maintenance auto-recovery loop revive them once their master is ready — strictly
  more robust (never gives up) and avoids duplicating the maintenance loop.
- **Notification events:** the Python daemon defines `Event.NOTIFICATION` in
  `ipc.py` but **never emits it** — notifications are produced client-side (Swift
  app + TUI react to `tunnel_status_changed`). The Rust daemon mirrors this exactly
  (the `Event::Notification` variant exists but is unemitted). Notification UX lives
  in the TUI (Tier 3) and the Swift app.

### Lock discipline

The `wake_recover` master probe (`ssh -O check`, 5s) snapshots the active slot
index under a brief map lock then probes off-lock — never holds the `HostManagers`
map mutex across blocking ssh I/O (would stall the heartbeat loop). Same rule the
rest of the daemon follows for `Mutex<State>`.

## Tier 3 — TUI parity

- Delete tunnel: `d` → confirm modal → `tunnel_remove`.
- Squeue-backed node picker: `Enter` on a tunnel → live `discover_nodes` list
  (nav, `c` custom entry, `r` refresh) → `tunnel_set_node`.
- Status-transition toasts mirroring `main.py:_notify_status_transitions`.
- macOS native notifications via `osascript` (background thread, macOS-gated, swallows errors).
- Yank URL: `y` → `http://localhost:<port><url_path>` to clipboard (pbcopy/xclip/wl-copy).
- Host actions: `m` mount toggle, `r` rotate.
- Help modal: `?`.
- Keybinding parity reconciled with the Python TUI.

### Final TUI keybindings

- **Global:** `q` quit (`Ctrl+c`), `t` new tunnel (`Ctrl+n`), `?` help, `Tab` switch pane, `/` filter, `l` logs, `j/k/↑/↓` move.
- **Tunnels:** `Space` toggle, `Enter` node picker, `y` yank URL, `d` delete, `s`/`x` start/stop aliases.
- **Hosts:** `Space` toggle, `m` mount, `r` rotate, `a` add host.

## Test status

Full workspace, single-threaded: **core 107, daemon 125 (+13 integration), tui 36, cli 15** — 0 failures.
(The 2 conformance/doc tests are `#[ignore]` by design.) Clippy: autofixes applied;
11 residual stylistic warnings (too-many-args on spawn fns, intentional `from_str` naming) left as-is.

## Cutover (T16) — DONE 2026-06-06 (zero-relogin handoff)

The Rust daemon is now the live, launchd-managed daemon (`com.ssh2fa.daemon`,
ProgramArguments → `~/.ssh2fa/ssh2fa-daemon`, env `SSH_CONFIG_PATH=/Users/shgao/.ssh/`).

**Two correctness fixes made the handoff zero-relogin** (commits `f4121f0`, `351f759`):
1. `control.rs` resolves `ControlPath` via `ssh -G` (honors the user's
   `ControlPath ~/.ssh/cm-ssh2fa-%h` directive) → Rust computes the SAME socket
   paths Python used.
2. `boot_autostart` adopts already-live masters (`adopt_if_alive`) → no login when
   a master socket is already up.

**Handoff procedure used** (preserves masters; Python tears them down on SIGTERM via
`cleanup_all`, so it must be killed without its handler running):
1. Back up plist; repoint ProgramArguments → `~/.ssh2fa/ssh2fa-daemon`.
2. `kill -STOP` Python → `launchctl unload` (deregister; SIGTERM stays pending while
   frozen) → `kill -KILL` Python (handler never runs → masters survive via
   `ControlPersist`) → `launchctl load` (Rust starts, adopts).
   Daemon **restart** is likewise zero-relogin (SIGKILL it; launchd KeepAlive respawns;
   it re-adopts). NOTE: a *graceful* SIGTERM tears masters down — use SIGKILL to preserve.

**Outcome:** b8/k7/k8 adopted with **0 logins**; txgent tunnel up. Only failures were
**pre-existing** (also down under Python): `k6` login fails at keyboard-interactive
("exited early") — toggled OFF to protect the account from its repeated-failure pattern
(the same pattern that rate-limited FAS-RC before); `claw` tunnel's compute node
`holygpu8a13504` is dead (probe timeout) — repick its node or stop it.

**A live-surfaced bug was fixed** (`81c338c`): the squeue format `%i|%P|...` was passed
unquoted, so the remote shell split on `|` (`bash: %T: command not found`) — broke node
discovery + stale detection. Now single-quoted.

**Rollback** (if needed): `cp ~/Library/LaunchAgents/com.ssh2fa.daemon.plist.bak-precutover`
back over the plist, then `launchctl unload && launchctl load` it to relaunch the Python
daemon at `.venv/bin/ssh2fa-daemon`. The Python code + venv are intentionally **not
deleted** yet — keep until the Rust daemon proves stable over normal use.

**Follow-ups (non-blocking):** investigate k6's keyboard-interactive "exited early" (node
vs account vs pty timing — pty_auth itself is proven working on b8/k7/k8/kempner); repick
claw's node; once stable, merge `rust-rewrite` → `main` and remove the Python backend.
