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
| Per-tunnel event ring buffer (≤200) | `tunnels.py:_record` | `a2fa-daemon/tunnel_runtime.rs` (runtime-only, not core model); `tunnel_events` handler returns real events |
| Daemon log rotation at startup (gzip >10MB) | `daemon.py:38` | `a2fa-daemon/log_rotation.rs` (flate2) |
| `cleanup_orphans` on boot (reap stray `ssh -N -J -L`) | `tunnels.py:944` | `a2fa-core/tunnels/cleanup.rs`; called in `server::run` |
| Real `wake_recover` + `reset_all` (master rebuild) | `daemon.py:911-1055` | `a2fa-daemon/handlers/system.rs` + `managers::spawn_master_rebuild`/`rebuild_masters` |
| Keychain migration v1→v2 on boot + one-time `.pre-keychain-backup` | `credentials.py:200-330` | `a2fa-core/creds/migrate.rs::migrate_passwords_file_if_needed`; wired in `server::run` before `State::new` |
| Graceful shutdown (SIGINT/SIGTERM teardown) | `daemon.py:1200` | `a2fa-daemon/server.rs` signal thread + `HostManagers::teardown_all` + `TunnelRuntime::kill_all_children` (signal-hook) |

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

## Remaining: cutover (T16)

Technically unblocked — the daemon is now autonomous and at parity. Cutover requires:
1. The FAS-RC account cooled from prior rate-limiting.
2. A clean user-supervised live e2e: run the Rust daemon, watch it auto-connect all
   hosts + tunnels, verify the Mac app, then retire Python.

Do **not** run live cluster logins while the account may be rate-limited.
