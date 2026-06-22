# SSH2FA — Native macOS App Architecture

**Date:** 2026-05-24
**Status:** In progress (this session: foundations only)

## 1. Goal

Ship SSH2FA as a real native macOS application:
- SwiftUI main window — lists hosts and tunnels, full feature parity with the TUI (create tunnel, pick compute node, copy URL, mount/unmount, etc.)
- AppKit menu bar item — always-visible status indicator + quick-action menu
- **Dynamic Notch UI** via [MrKai77/DynamicNotchKit](https://github.com/MrKai77/DynamicNotchKit) — important state transitions (tunnel connected, tunnel dropped, host failed) animate from the notch on MacBook Pros. Falls back automatically to a floating panel on non-notched Macs.
- Dock icon — clicking restores the main window
- Auto-start on login via `~/Library/LaunchAgents/com.ssh2fa.daemon.plist`
- Existing Textual TUI continues to work standalone for SSH/headless users

## 1a. Three UI surfaces

| Surface | Always visible? | Purpose |
|---|---|---|
| **Main window** | No (toggle from Dock or menu bar) | Full management — list, create, pick node, copy URL, delete |
| **Menu bar item** | Yes | At-a-glance status colour + quick dropdown |
| **Dynamic Notch** | Transient, ~3-5s on event | Push notifications for tunnel state transitions via `DynamicNotchInfo` (icon + title + description). Expands from the notch on M1/M2/M3 Pros; floating panel on Air / Intel Macs. |

DynamicNotchKit is added as a Swift Package Manager dependency
(`https://github.com/MrKai77/DynamicNotchKit`, macOS 13+). Used pattern:

```swift
let info = DynamicNotchInfo(
    icon: .init(systemName: "bolt.fill"),
    title: "Connected",
    description: "jupyter via k8 → localhost:8888"
)
await info.expand()
```

## 2. Architecture

**Two processes, one shared state owner:**

```
                                ┌─────────────────────────┐
                                │  Python daemon process  │
   Mac app                      │  (auto2fa.daemon)       │
  ──────────                    │                         │
  Swift SwiftUI ◀──IPC────────▶ │  - SSHHostManager pool  │
  AppKit menubar                │  - TunnelManager        │
                                │  - JSON-RPC server      │
   TUI (legacy)                 │    on Unix socket       │
  ──────────                    │  - Event bus            │
  Textual ───────────────┐      │                         │
  (standalone mode,      │      └─────────────────────────┘
   no daemon)            │                  ▲
                         │                  │
                         └──────────────────┘
                         (TUI in daemon-client
                          mode — future)
```

Today both `auto2fa` (TUI) and the Mac app run their own in-process managers.
After this work:

- **Daemon mode (new):** A long-running Python process owns the managers and serves them over a Unix socket. The Mac app connects to it. Started by LaunchAgent on login.
- **TUI standalone mode (current):** unchanged, for ssh/headless use.
- **TUI daemon-client mode (future):** TUI can connect to the daemon instead of running its own managers. Out of scope for this session.

A **flock** (`~/.ssh2fa/lock`) prevents the standalone TUI and the daemon from running at the same time on the same machine — they'd fight over SSH ControlMaster sockets.

## 3. IPC protocol

**Transport:** Unix domain socket at `~/.ssh2fa/ssh2fa.sock`. Local-only, file permissions 0600.

**Framing:** Line-delimited JSON. Each message is one JSON object terminated by `\n`. Easy to debug with `nc -U`.

**Message types:**

```jsonc
// Request (client → daemon)
{"id": "abc123", "method": "list_tunnels", "params": {}}

// Response (daemon → client)
{"id": "abc123", "result": [...]}
// or
{"id": "abc123", "error": {"code": "not_found", "message": "..."}}

// Event (daemon → all subscribed clients, no id)
{"event": "tunnel_status_changed",
 "data": {"name": "jupyter", "status": "alive", "active_jump": "k8"}}
```

### Methods

| Method | Params | Result | Notes |
|---|---|---|---|
| `list_hosts` | – | `[{host, status, is_master_ready, pool_index, pool_alive, is_mounted, last_msg, active}]` | snapshot |
| `list_tunnels` | – | `[{name, local_port, remote_port, last_node, last_user, jump_candidates, active_jump, status, last_msg, auto_start}]` | snapshot |
| `host_toggle` | `{host}` | `null` | flips `mgr.active` |
| `host_mount_toggle` | `{host}` | `null` | toggle sshfs |
| `host_rotate` | `{host}` | `null` | rotate pool |
| `tunnel_add` | `{name, local_port, remote_port?}` | tunnel state | raises if dup/port-in-use |
| `tunnel_remove` | `{name}` | `null` | stops + deletes |
| `tunnel_toggle` | `{name}` | `null` | start/stop |
| `tunnel_set_node` | `{name, node, user}` | `null` | persists + starts |
| `discover_nodes` | `{host}` | `[{jobid, partition, name, state, time, node}]` | runs `squeue` |
| `subscribe_events` | – | – | enables event push for this client |
| `ping` | – | `{ok: true}` | healthcheck |

### Events

| Event | Data |
|---|---|
| `host_status_changed` | `{host, status, is_master_ready, ...}` |
| `tunnel_status_changed` | `{name, status, last_msg, active_jump, ...}` |
| `notification` | `{severity: "info"\|"warning"\|"error", title, message}` |

The daemon throttles its own emission rate (≤ 4 events/s per topic) so a flapping tunnel can't spam clients.

## 4. Files added / modified

### Python (`auto2fa/`)

```
auto2fa/
├── backend.py        # unchanged
├── tunnels.py        # unchanged
├── main.py           # unchanged (TUI standalone)
├── ipc.py            # NEW: protocol constants, encode/decode helpers
├── daemon.py         # NEW: async IPC server, owns managers, emits events
└── client.py         # NEW: sync IPC client wrapper (for Mac app dev/testing)
```

New entry points in `setup.py`:
- `auto2fa` → existing TUI
- `ssh2fa-daemon` → starts the daemon (called by LaunchAgent)

### Swift (`auto2fa-mac/`)

```
auto2fa-mac/
├── SSH2FA.xcodeproj/    # Xcode project metadata
├── SSH2FA/
│   ├── Auto2FAApp.swift       # @main entry, configures menu bar + main window
│   ├── MenuBarController.swift # NSStatusItem + menu
│   ├── NotchPresenter.swift   # DynamicNotchKit wrapper for state-change toasts
│   ├── ContentView.swift      # main window root
│   ├── BackendClient.swift    # Unix socket IPC client (async/await)
│   ├── AppState.swift         # @MainActor ObservableObject; mirror of daemon state
│   ├── Models/
│   │   ├── Host.swift
│   │   ├── Tunnel.swift
│   │   └── Job.swift
│   ├── Views/
│   │   ├── HostsView.swift
│   │   ├── TunnelsView.swift
│   │   ├── NewTunnelSheet.swift    # later session
│   │   ├── NodePickerSheet.swift   # later session
│   │   └── ConfirmDeleteSheet.swift # later session
│   └── Resources/
│       ├── Info.plist
│       └── Assets.xcassets/
└── README.md
```

### LaunchAgent

```
LaunchAgents/
└── com.ssh2fa.daemon.plist     # template; user installs to ~/Library/LaunchAgents/
```

## 5. Session plan (today)

Today delivers the foundation; the Mac app will be runnable in Xcode but with limited feature surface (list views only). Modal sheets, full actions, and `.app` packaging come in follow-ups.

1. Spec (this doc)
2. `ipc.py` — protocol shapes
3. `daemon.py` — IPC server using `asyncio` + Unix socket; wraps `SSHHostManager` + `TunnelManager`; emits events on state change
4. `client.py` — sync wrapper (used by the Swift bridge during dev; final Swift talks directly via Network.framework)
5. Swift project skeleton with `BackendClient.swift` doing `URLSession`-style line-delimited reads from the Unix socket
6. SwiftUI views: `HostsView`, `TunnelsView` (read-only first cut)
7. Menu bar controller showing tunnel count + dropdown
8. `NotchPresenter` wiring DynamicNotchKit to daemon events
9. `LaunchAgents/com.ssh2fa.daemon.plist`
10. Top-level `README` updates

## 6. Out of scope (later sessions)

- New-tunnel / node-picker / confirm-delete SwiftUI sheets
- Wiring up start/stop/delete/yank actions in Swift
- Code signing + notarization
- `.app` bundling with `xcodebuild archive` + DMG
- TUI → daemon-client refactor
- Settings pane (jump candidate filter, auto-start toggles)
