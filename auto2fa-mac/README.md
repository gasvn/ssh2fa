# SSH2FA — Native macOS Frontend

SwiftUI + AppKit frontend for [SSH2FA](../README.md), talking to the Python
daemon over a Unix domain socket.

## Status

**Foundations only (2026-05-24).** This session ships:

- Swift app skeleton — opens in Xcode, builds against macOS 14.
- `BackendClient` (Network.framework) — async/await wrapper around the
  daemon's line-delimited JSON IPC. Subscribes to events.
- `AppState` — `@MainActor` observable mirror of daemon state.
- Read-only `HostsView` and `TunnelsView` in the main window.
- `MenuBarController` (AppKit `NSStatusItem`) — colour-coded status icon
  with a quick-action `NSMenu`.
- `NotchPresenter` wrapper around [DynamicNotchKit](https://github.com/MrKai77/DynamicNotchKit)
  — pushes a notch toast on tunnel state transitions. Falls back
  automatically to a floating panel on non-notched Macs.
- `LaunchAgent` template (`../LaunchAgents/com.ssh2fa.daemon.plist`)
  for auto-starting the daemon on login.

**Not yet implemented** (next sessions):

- New-Tunnel / Node-Picker / Confirm-Delete SwiftUI sheets
- Most user actions (start/stop/delete/yank URL/mount) — calls wired but
  not surfaced in UI yet
- Code signing + notarization
- `.app` bundle output and DMG

## Building

1. Install the Python daemon side first (from the repo root):

   ```bash
   pip install -e .
   ```

   This registers the `ssh2fa-daemon` entry point. Verify with
   `ssh2fa-daemon --help` (it accepts no args; it just runs).

2. Install the LaunchAgent so the daemon starts on login:

   ```bash
   cp ../LaunchAgents/com.ssh2fa.daemon.plist ~/Library/LaunchAgents/
   # edit the plist if your install location for ssh2fa-daemon isn't /usr/local/bin
   launchctl load ~/Library/LaunchAgents/com.ssh2fa.daemon.plist
   ```

3. Open `SSH2FA.xcodeproj` in Xcode 15+:

   ```bash
   open SSH2FA.xcodeproj
   ```

   Add the DynamicNotchKit Swift Package via *File → Add Package
   Dependencies…*:

   ```
   https://github.com/MrKai77/DynamicNotchKit
   ```

4. Set the run scheme to "SSH2FA" and build & run (⌘R).

## Project layout

```
auto2fa-mac/
├── SSH2FA.xcodeproj/             # Xcode project (generated on first build)
└── SSH2FA/
    ├── Auto2FAApp.swift           # @main; configures menu bar + main window
    ├── MenuBarController.swift    # NSStatusItem and its NSMenu
    ├── NotchPresenter.swift       # DynamicNotchKit toasts for state changes
    ├── ContentView.swift          # Main window root (TabView host/tunnels)
    ├── BackendClient.swift        # async Unix socket IPC client
    ├── AppState.swift             # @MainActor ObservableObject — daemon mirror
    ├── Models/
    │   ├── Host.swift
    │   ├── Tunnel.swift
    │   └── Job.swift
    ├── Views/
    │   ├── HostsView.swift
    │   └── TunnelsView.swift
    └── Resources/
        └── Info.plist
```

`SSH2FA.xcodeproj` is intentionally not in the repo — generate it on
first open via `swift package init` then create the Xcode project from
the package, or hand-create via Xcode → File → New → Project →
macOS App. The `SSH2FA/` directory is your sources root.

## IPC protocol

See [docs/superpowers/specs/2026-05-24-mac-app-design.md](../docs/superpowers/specs/2026-05-24-mac-app-design.md)
for the full daemon ↔ client protocol. TL;DR:

- Transport: Unix domain socket at `~/.ssh2fa/ssh2fa.sock`.
- Framing: line-delimited JSON.
- Messages: requests (with `id`/`method`/`params`), responses (`id`/`result|error`),
  events (no id, `event`/`data`).

You can poke the daemon by hand:

```bash
echo '{"id":"1","method":"list_tunnels"}' | nc -U ~/.ssh2fa/ssh2fa.sock
```
