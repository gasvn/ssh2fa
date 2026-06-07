# P1: Self-contained Mac app (bundle the daemon, share to others)

**Status:** Design approved, pending implementation
**Date:** 2026-06-06
**Part of:** the "make auto2fa general" effort (P1 of P0–P3). Builds on P0 (self-bootstrapping installer).

## Problem

Today auto2fa only works on the developer's machine: the Swift app discovers an
external repo + `.venv` (via `~/.auto2fa/project-dir.txt` / `python-path.txt`)
and spawns `python -m auto2fa.daemon` from there. To hand the app to someone
else they would have to clone the repo, have a working Python, and run the
installer. There are also personal assumptions baked into defaults (a
`~/logs/auto2fa_dev` fallback path, example node names like `holygpu08`).

We want a **self-contained `Auto2FA.app`**: the recipient drags it to
`/Applications`, opens it, and it works — no Python, no repo, no manual setup.

## Goal

`Auto2FA.app` ships with its own Python daemon embedded. On first launch the app
registers a bundled launchd agent (`SMAppService.agent`) that starts the
embedded daemon at login. The app connects to the daemon over the existing
socket. No external Python or repo is required for an end user. The developer's
from-source workflow (P0) keeps working unchanged.

Distribution: ad-hoc signed (no Apple Developer ID). Sharing instructions tell
the recipient to clear the Gatekeeper quarantine (right-click → Open, or
`xattr -dr com.apple.quarantine Auto2FA.app`).

Non-goals:
- No Developer ID signing / notarization (documented workaround instead).
- No Linux/Windows (P3 covers Linux for the CLI/daemon; the GUI is macOS-only).
- No Slurm changes (P2).
- The from-source dev path (P0 installer) is unchanged, not removed.

## Architecture: dual path (bundled for users, source for devs)

```
Auto2FA.app/
  Contents/
    MacOS/Auto2FA                                   Swift app (unchanged entry)
    Resources/daemon/auto2fa-daemon                 PyInstaller --onedir output:
    Resources/daemon/_internal/...                  embedded CPython + deps
    Library/LaunchAgents/com.auto2fa.daemon.plist   SMAppService agent plist
```

- **Bundled path (end user):** at launch the app detects it is a bundled build
  (the embedded daemon exists at `Contents/Resources/daemon/auto2fa-daemon`) and
  registers the agent via `SMAppService.agent(plistName: "com.auto2fa.daemon.plist").register()`.
  launchd starts the embedded daemon at login and keeps it alive. The app only
  *connects* to the socket — it does NOT spawn the daemon itself.
- **Source path (developer):** when there is no embedded daemon (running the
  dev build), behavior is exactly today's: `DaemonProcess` resolves the repo via
  `~/.auto2fa/project-dir.txt` and spawns `python -m auto2fa.daemon` using the
  pinned interpreter (P0). `SMAppService` is not used in this path.

The daemon's existing single-instance flock guard (added earlier) means even if
both paths somehow fire, only one daemon binds the socket.

### Why SMAppService bundled agent

It is the standard modern macOS pattern for a menu-bar app with a login-time
background helper. The plist lives *inside* the bundle and SMAppService resolves
it relative to the registered app, so moving/renaming the app does not silently
break auto-start (unlike a hand-written absolute-path LaunchAgent). It reuses
the same framework the app already uses for its `SMAppService.mainApp` login
item.

### The agent plist

`Contents/Library/LaunchAgents/com.auto2fa.daemon.plist`, using `BundleProgram`
(a bundle-relative path), NOT an absolute `ProgramArguments` path:

```xml
<key>Label</key>            <string>com.auto2fa.daemon</string>
<key>BundleProgram</key>    <string>Contents/Resources/daemon/auto2fa-daemon</string>
<key>RunAtLoad</key>        <true/>
<key>KeepAlive</key>        <dict><key>SuccessfulExit</key><false/></dict>
```

(No `SSH_CONFIG_PATH` needed — `credentials.config_dir()` falls back to `~/.ssh`.
`StandardOut/ErrorPath` may point at `/tmp/auto2fa_daemon.log` as today.)

## Bundling approach

**PyInstaller `--onedir`** builds a standalone `auto2fa-daemon` (embedded CPython
+ deps) from the `auto2fa-daemon` console entry point. Output `dist/auto2fa-daemon/`
is copied into `Contents/Resources/daemon/`.

- Known gotcha: `keyring` loads its macOS backend dynamically; PyInstaller needs
  it declared as a hidden import (`--collect-submodules keyring` /
  `--hidden-import keyring.backends.macOS`). The daemon also pulls `pexpect`,
  `pyotp`, `python-dotenv` (all import cleanly under PyInstaller). `textual` is
  NOT needed by the daemon (it is the TUI), so it should be excluded to keep the
  bundle small.
- Alternatives considered: a relocatable python-build-standalone CPython +
  site-packages (more manual/larger); py2app (awkward for a Swift host). Rejected
  in favor of PyInstaller onedir.

## Components / changes

1. **PyInstaller build** — a spec or a `build.sh` step that produces
   `dist/auto2fa-daemon/` with the hidden imports above, excluding textual.
2. **Build pipeline** — `build.sh` runs the PyInstaller step before/after
   `xcodebuild` and ensures the onedir output lands in
   `Contents/Resources/daemon/` and the agent plist lands in
   `Contents/Library/LaunchAgents/`. `project.yml` declares these as bundled
   resources / a Copy Files phase. The daemon build is skippable for pure-Swift
   dev iteration (a flag), so day-to-day source dev does not pay the PyInstaller
   cost.
3. **Swift `DaemonProcess`** — add `isBundledBuild` detection (embedded daemon
   present). Bundled → register the SMAppService agent and connect; do not spawn.
   Source → today's spawn path. Keep the flock/socket-responds safety.
4. **Swift agent registration** — a small wrapper (mirroring `LoginItem.swift`)
   around `SMAppService.agent(plistName:)` with register/status/error handling.
5. **De-personalization** — remove the `~/logs/auto2fa_dev` default in
   `DaemonProcess.discoverProjectDir()` (dev path now relies on
   `project-dir.txt`, which P0 always writes); replace example node names
   (`holygpu08`, etc.) in `cli.py` help and any docs with generic placeholders;
   sweep for other personal paths/host names in shipped code.
6. **Distribution docs** — README section: how to build the shareable app, and
   the recipient's Gatekeeper step (right-click → Open, or
   `xattr -dr com.apple.quarantine Auto2FA.app`).

## Verification model (different from P0)

P1 is build-tooling + Swift + packaging — largely NOT unit-testable. Verification
is end-to-end:

1. **Bundled daemon runs in a clean env:** after PyInstaller build,
   `env -i HOME=$HOME PATH=/usr/bin:/bin Contents/Resources/daemon/auto2fa-daemon`
   starts, binds the socket, and `list_hosts` returns data. This proves the
   embed has no missing imports (esp. keyring).
2. **Bundled app e2e:** build `Auto2FA.app`; from a clean shell confirm the app
   registers the agent (`launchctl print gui/$UID/com.auto2fa.daemon` or
   `SMAppService` status = enabled), the daemon starts, and the app's
   `list_hosts` returns hosts.
3. **Source path unbroken:** a dev build (no embedded daemon) still spawns from
   `project-dir.txt` and serves data — confirm P0's `auto2fa install` flow and
   the app's dev spawn still work.
4. **De-personalization:** `grep -r` over shipped code finds no `/Users/<name>`,
   no personal node names, no `~/logs/auto2fa_dev` default.

What little IS unit-testable: any new pure Swift/Python helper (e.g. the
bundled-vs-source detection logic if extracted to a testable function) and a
`grep`-based de-personalization assertion in the Python test suite.

## Decomposition (two phases, one spec)

- **P1a — bundle the daemon:** PyInstaller build + embed in `.app` + build
  pipeline. Done when the embedded `auto2fa-daemon` runs in a clean env from
  inside the built bundle.
- **P1b — Swift integration + polish:** SMAppService agent registration +
  bundled/source detection in `DaemonProcess`; de-personalization; Gatekeeper
  docs. Done when a freshly built `Auto2FA.app`, launched on a clean account,
  auto-starts its daemon and shows hosts — with the dev path still working.

## Risks / open points

- **ad-hoc SMAppService agent registration** may need testing — SMAppService
  agents are most reliable with a stable signing identity. If ad-hoc
  registration is rejected at runtime, fall back to the P0-style hand-written
  LaunchAgent pointing at the bundled daemon (still self-contained, just less
  idiomatic). This fallback is the contingency, decided during P1b e2e.
- **PyInstaller + keyring** hidden-import completeness is the most likely build
  snag; the clean-env smoke test (verification step 1) catches it early.
- Bundle size grows by the embedded CPython (~tens of MB); acceptable.
