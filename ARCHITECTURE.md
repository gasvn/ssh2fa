# SSH2FA — Architecture

This document describes the codebase as it is shipped on the `feat/tunnels`
branch. It is intended to help future contributors (human or LLM) navigate
quickly.

## 1. Goal

SSH2FA is a Textual TUI dashboard that:

1. Maintains persistent SSH ControlMaster connections to login hosts that
   require TOTP (an HPC center, etc.), so `ssh host` from any other terminal is
   instant and 2FA-free.
2. Layers SLURM-aware **two-level port forwards** (login host → compute node)
   on top of those connections, with a node picker, persistence, and
   auto-recovery when nodes disappear.
3. Optionally mounts the remote filesystem via sshfs.

## 2. Layout

```
auto2fa_dev/
├── auto2fa/
│   ├── __init__.py
│   ├── backend.py        # SSHHostManager — pool of 2 masters per host
│   ├── tunnels.py        # TunnelManager — port forwards + node discovery
│   └── main.py           # Textual App, screens, key handlers
├── tests/
│   ├── test_backend_hooks.py
│   ├── test_pooling_logic.py
│   └── test_tunnels.py   # 51 tests covering the tunnel manager
├── docs/
│   └── superpowers/
│       ├── specs/        # design specs (latest: 2026-05-22-tunnels-design.md)
│       └── plans/        # implementation plans
├── setup.py
├── requirements.txt      # textual >= 0.40, rich, pyotp, pexpect, dotenv
├── install_deps.sh       # macFUSE-T + sshfs installer
├── ssh_config_template
└── README.md
```

## 3. Three modules, three concerns

### A. `backend.py` — `SSHHostManager`

One instance per login host. Daemon `threading.Thread`. Owns a pool of two
SSH ControlMaster connections so `MaxSessions` exhaustion never breaks the
user's IDE.

Key state:
- `self.active` — user toggled on/off (Space in dashboard)
- `self.running` — thread lifecycle
- `self.pool` — `{0: pexpect_child, 1: pexpect_child}`
- `self.pool_status` — `{0: "Ready"|"Failed"|"Dead", ...}`
- `self.active_index` — which pool slot the symlink points to
- `self.is_mounted` — sshfs mount state (dedicated bit, not derived from log
  messages)

Key methods:
- `manage_pool_loop()` — heartbeat every 3s (cheap local `ssh -O check`),
  remote probe every 5s, rotate to the other pool slot when active is full
- `start_master(index)` — pexpect-driven login that handles password + TOTP
  prompts and parks `ssh -N -M` as the master
- `cleanup_all()` / `cleanup_stale_connection()` — kill stray children, drop
  sockets, remove symlinks
- `is_master_ready()` — read-only API used by `TunnelManager` to pick a jump
- `mount_host()` / `unmount_host()` / `toggle_mount()` — sshfs lifecycle,
  debounced to avoid mount+unmount races on rapid M presses

All subprocess calls inside the manager have timeouts; the desktop
notification helper runs on its own daemon thread so a wedged
`osascript` can't stall the pool loop.

### B. `tunnels.py` — `TunnelManager`

Owns every port forward. Tunnels are top-level (not nested per-host) — any
connected login host can serve as the jump.

State per tunnel (`TunnelState` dataclass): name, local_port, remote_port,
jump_candidates (or None = "any"), last_node, last_user, auto_start, plus
runtime fields (status, active_jump, child, last_msg).

Persistence: `$SSH_CONFIG_PATH/tunnels.json`, written atomically
(`tmp + os.replace`), serialised by `_save_lock`.

Concurrency:
- **Per-tunnel `_lifecycle_lock`** (one Lock per tunnel name, lazy-created)
  so concurrent `start`/`stop` on *different* tunnels run in parallel —
  one tunnel's 10s port probe doesn't block another's.
- `tick()` runs from a background thread driven by `Auto2FAApp` and is the
  sole automatic state machine (auto-start on grace period, dead-child
  respawn, jump-failover, SLURM stale detection, all with idempotent
  semantics so they cooperate with UI-driven actions).

Discovery (`NodeDiscovery`):
- `discover(host_mgr)` runs
  `ssh -o ControlPath=<master> <host> "squeue -h -o '%i|%P|%j|%T|%M|%R' -u $USER"`
  through the existing ControlMaster, so it inherits 2FA-free auth and is
  near-instant once the host is connected.
- `parse(stdout)` is pure and unit-tested.
- `expand_first_node(nodelist)` handles SLURM bracket ranges like
  `gpunode[01-03]`.

Lifecycle (`start`):
1. Acquire per-tunnel lock.
2. Short-circuit if already alive/starting, no last_node, no jump
   candidate, or local port not bindable.
3. `pexpect.spawn("ssh", ["-N", "-J", jump, "-L", "<lp>:localhost:<rp>",
   "-o", "ExitOnForwardFailure=yes", ..., "<user>@<node>"])` —
   spawn errors are caught and surface as status=`failed`.
4. Probe `127.0.0.1:local_port` for up to 10s. Success → `alive`.
   Timeout → terminate child, extract a short failure reason from
   pexpect.before, status=`failed`.

`shutdown()` uses `lock.acquire(timeout=0.5)` per tunnel so app exit doesn't
hang waiting for a 10s probe.

### C. `main.py` — `Auto2FAApp` (Textual)

Single-screen layout:
```
┌ Header (live summary: hosts X/Y connected · tunnels A/B alive · ? for help)
├ HOSTS section title
├ HostTable    (DataTable subclass; bindings: Space/M/R)
├ TUNNELS section title (current focus highlighted)
├ TunnelTable  (DataTable subclass; bindings: Space/T/D/Y)
└ Footer
```

Per-table `BINDINGS` instead of App-level ones so single-letter keys
(t, d, m, r, y, space) never conflict with `Input` widgets in modals.

Modal screens:
- `NewTunnelScreen` — Name + Local port + Submit/Cancel buttons (Ctrl+S)
- `NodePickerScreen` — squeue listing with pre-highlighted previous node,
  R refresh, C custom-entry escape hatch. squeue runs on a worker thread
  so the modal opens instantly with a "Loading…" state.
- `CustomNodeScreen` — Node + User (Enter on node moves to user; only
  user-field Enter submits, so paste with trailing newline doesn't fire
  prematurely)
- `ConfirmScreen` — Y/N for delete
- `HelpScreen` — ? key opens it; binds escape/q/? to dismiss

Every modal also binds `q→cancel` so the App's quit binding can't be
triggered accidentally from inside a dialog.

Background drivers:
- `_tick_loop` (background thread) calls `tunnel_mgr.tick()` every 500ms.
  Hard-skipped via `is_set` check on shutdown.
- `_tick_ui` (Textual `set_interval`, 1s) refreshes the two tables and the
  header subtitle. Skipped while any modal is on the screen stack so input
  fields stay snappy. Fingerprint-checks state to avoid unnecessary
  rebuilds.

Worker threads for any operation that might block more than ~50ms:
- `action_toggle_tunnel` → spawns worker so the up-to-10s probe doesn't
  freeze the UI. Debounced by `_toggle_in_flight` set.
- `action_pick_node` → spawns worker for `set_node + start`.
- `action_delete_tunnel` → spawns worker for `stop + remove`.
- `action_mount_host` → spawns worker for `toggle_mount`.
- `_fallback_clipboard` → pbcopy/xclip on its own thread with 2s timeout.

Notifications:
- In-app `self.notify(...)` toasts for routine events.
- `_system_notify(title, msg)` posts a macOS Notification Center alert via
  `osascript` on a daemon thread (so it can't block the UI) for important
  transitions: alive → stale, alive → failed, initial-connect failures.
- `_user_stopped` set suppresses the "connection dropped" warning when the
  user pressed Space intentionally — they get a calm "⊘ X stopped" toast
  instead.

## 4. Configuration

### `passwords.json`  (in `$SSH_CONFIG_PATH`)

```json
{
  "k8": {
    "password": "...",
    "otpauthUrl": "otpauth://totp/...?secret=...",
    "autoConnect": true
  }
}
```

Each top-level key must match a `Host` alias in `~/.ssh/config`. Optional
`autoConnect: true` starts the master pool on dashboard launch.

### `tunnels.json`  (in `$SSH_CONFIG_PATH`)

```json
{
  "tunnels": {
    "jupyter": {
      "local_port": 8888,
      "remote_port": 8888,
      "jump_candidates": null,
      "last_node": "gpunode8a11103.hpc.example.edu",
      "last_user": "alice",
      "auto_start": true
    }
  }
}
```

- `jump_candidates: null` ⇒ any host in `passwords.json` may be used.
- `auto_start: true` ⇒ try to start on dashboard launch after a 3s grace
  period.
- `last_node` updates automatically when the user picks a node.

### `~/.ssh/config`

Must use `ControlMaster no` for client connections (so the IDE doesn't try
to hijack our master pool) and `ControlPath ~/.ssh/cm-ssh2fa-%h` so it
follows the per-pool symlink that `SSHHostManager` maintains.

## 5. Keys at a glance

| Section | Key | Action |
|---------|-----|--------|
| Global  | `T` | New tunnel modal (works anywhere; Input widgets consume t when typing) |
| Global  | `?` | Help overlay |
| Global  | `Q` | Quit |
| Global  | `Tab` | Switch focus between HOSTS and TUNNELS |
| Hosts   | `Space` | Start/stop the selected host |
| Hosts   | `M` | Mount/unmount sshfs (toggle, debounced) |
| Hosts   | `R` | Rotate connection pool |
| Tunnels | `Space` | Start/stop the selected tunnel |
| Tunnels | `Enter` | Pick a compute node from `squeue` |
| Tunnels | `Y` | Copy `localhost:<port>` to clipboard |
| Tunnels | `D` | Delete the selected tunnel |
| Modals  | `Esc` / `Q` | Cancel |
| Modals  | `Ctrl+S` | Submit (in form modals) |

## 6. Test surface

`pytest tests/` runs ~58 tests covering:
- `is_master_ready` semantics
- `Job` / `TunnelState` / `DiscoveryError` shapes
- `NodeDiscovery.parse` against canned squeue outputs
- `NodeDiscovery.discover` (mocked subprocess)
- `TunnelManager.load`/`save` round-trips (including malformed and
  non-dict root JSON)
- `add` validation (duplicate, port range, port in use, remote_port range)
- `remove`/`set_node`/`pick_active_jump` (default candidates, unknown
  candidate skip, etc.)
- `start` short-circuits (no node, no jump, port busy, already alive/starting)
  plus happy-path spawn + probe
- `stop`/`toggle`
- `tick` state machine: dead-child respawn (asserts both stop and start
  are called — earlier regression let dead tunnels stay green), jump
  failover, 2-miss stale detection, discovery error doesn't bump misses,
  squeue hit resets miss counter
- `cleanup_orphans` (pgrep + kill)
- `shutdown` (children killed) and the new
  `test_shutdown_does_not_block_on_held_lock`
- `expand_first_node` for SLURM bracket ranges

## 7. Common pitfalls (learned the hard way)

- **Single global lock = serial probes.** The very first version had one
  `_lifecycle_lock` for all tunnels; two starts couldn't run in parallel.
  Now each tunnel has its own lock; `_save_lock` is the only manager-wide
  serialisation.
- **Modal Q quitting the app.** App-level Q was firing even while a modal
  had focus on a non-Input widget (e.g., a Button). Every modal now binds
  `q → cancel`.
- **Optimistic refresh blowing up.** If the refresh after Space raised, the
  in-flight set kept the name forever, silently ignoring subsequent
  presses. Wrapped in try/except.
- **`start()` is a no-op on dead children.** tick() used to call start()
  directly when it detected a dead child, but start short-circuits on
  status="alive" — so the entire self-healing loop did nothing. tick now
  calls stop+start. Test was a false green because it mocked start; now
  asserts both calls.
- **Pasted hostnames with `\n` triggering Input.Submitted.** CustomNode's
  node-field Enter no longer submits; only the user-field's does. Embedded
  newlines are stripped at submit time.
