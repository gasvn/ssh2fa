# P1 Self-Contained Mac App Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship `Auto2FA.app` with its own embedded Python daemon so a recipient drags it to `/Applications`, opens it, and it works — no Python, no repo — while the developer's from-source workflow keeps working.

**Architecture:** Build the daemon into a self-contained directory with PyInstaller (`--onedir`) and embed it in the app bundle. The app runs in one of two paths: **bundled** (embedded daemon present → register an `SMAppService` agent whose plist lives inside the bundle → launchd starts the daemon at login; the app only connects) or **source** (no embedded daemon → today's spawn-from-`project-dir.txt` path). De-personalize shipped defaults and document the Gatekeeper workaround for ad-hoc-signed distribution.

**Tech Stack:** PyInstaller, Swift (`SMAppService`, `Bundle`), xcodegen/xcodebuild, launchd.

**Spec:** `docs/superpowers/specs/2026-06-06-p1-self-contained-app-design.md`

**Verification model:** P1 is build-tooling + Swift + packaging — mostly NOT unit-testable. Tasks use clean-environment smoke tests and build/launch e2e checks instead of pytest where TDD does not fit. The Python de-personalization piece (Task 5) does keep a real pytest. Some e2e steps (SMAppService registration, GUI launch) need a human to confirm the menu-bar UI; those are called out explicitly.

---

## File Structure

- Create: `packaging/daemon_entry.py` — PyInstaller entry script (`auto2fa.daemon:main`).
- Create: `packaging/build_daemon.sh` — reproducible PyInstaller build (hidden imports, excludes) → `packaging/dist/auto2fa-daemon/`.
- Create: `packaging/com.auto2fa.daemon.agent.plist` — the SMAppService agent plist (uses `BundleProgram`), copied into the app at build time.
- Modify: `.gitignore` — ignore `packaging/build/`, `packaging/dist/`, `packaging/*.spec`.
- Modify: `auto2fa-mac/build.sh` — build the daemon and embed it + the agent plist into the `.app`.
- Modify: `auto2fa-mac/project.yml` — declare the embedded daemon + agent plist as bundled files (Copy Files build phases).
- Create: `auto2fa-mac/Auto2FA/BundledDaemonAgent.swift` — `SMAppService.agent` wrapper (register/status), mirroring `LoginItem.swift`.
- Modify: `auto2fa-mac/Auto2FA/DaemonProcess.swift` — `isBundledBuild` detection; `ensureRunning()` branches bundled→register-agent vs source→spawn; remove the `~/logs/auto2fa_dev` default.
- Modify: `auto2fa/cli.py` — de-personalize help examples.
- Modify: `README.md` — distribution + Gatekeeper section.
- Test: `tests/test_depersonalization.py` — asserts shipped code has no personal paths/host names.

---

## P1a — Bundle the daemon

### Task 1: PyInstaller build of the daemon + clean-env smoke

**Files:**
- Create: `packaging/daemon_entry.py`, `packaging/build_daemon.sh`
- Modify: `.gitignore`

- [ ] **Step 1: Create the entry script**

`packaging/daemon_entry.py`:
```python
"""PyInstaller entry point for the bundled daemon. PyInstaller needs a real
script file (not a console_scripts entry), so this just calls the same main()."""
import sys

from auto2fa.daemon import main

if __name__ == "__main__":
    sys.exit(main())
```

- [ ] **Step 2: Create the build script**

`packaging/build_daemon.sh`:
```bash
#!/usr/bin/env bash
# Build the self-contained daemon with PyInstaller (--onedir).
# Output: packaging/dist/auto2fa-daemon/auto2fa-daemon (+ _internal/).
set -euo pipefail
cd "$(dirname "$0")/.."

VENV_PY=".venv/bin/python"
"$VENV_PY" -m pip install --quiet --upgrade pyinstaller

"$VENV_PY" -m PyInstaller --noconfirm --onedir --name auto2fa-daemon \
  --collect-submodules keyring \
  --hidden-import keyring.backends.macOS \
  --exclude-module textual \
  --exclude-module rich \
  --distpath packaging/dist \
  --workpath packaging/build \
  --specpath packaging \
  packaging/daemon_entry.py

echo "built: packaging/dist/auto2fa-daemon/auto2fa-daemon"
```

- [ ] **Step 3: Ignore build artifacts**

Append to `.gitignore`:
```
# PyInstaller daemon build output
packaging/build/
packaging/dist/
packaging/*.spec
```

- [ ] **Step 4: Build it**

Run: `chmod +x packaging/build_daemon.sh && ./packaging/build_daemon.sh`
Expected: ends with `built: packaging/dist/auto2fa-daemon/auto2fa-daemon` and no traceback.

- [ ] **Step 5: Clean-env smoke test (the key check — catches missing hidden imports)**

This proves the embedded binary has every dependency (esp. keyring) with NO access to the dev venv or site-packages. The running daemon already holds the singleton flock, so the embedded one should import cleanly then exit via the flock (exit 0) — what matters is the ABSENCE of ImportError/ModuleNotFoundError.

Run:
```bash
env -i HOME="$HOME" PATH="/usr/bin:/bin" \
  packaging/dist/auto2fa-daemon/auto2fa-daemon > /tmp/p1_smoke.log 2>&1 &
SMOKE=$!; sleep 3; kill -9 $SMOKE 2>/dev/null
echo "--- smoke output ---"; cat /tmp/p1_smoke.log
grep -Eq "ModuleNotFoundError|ImportError|Traceback" /tmp/p1_smoke.log \
  && echo "SMOKE FAIL: import error in bundled daemon" \
  || echo "SMOKE PASS: no import errors"
```
Expected: `SMOKE PASS: no import errors`, and the log shows daemon startup lines (e.g. `Daemon initialised` / `daemon listening` / `another auto2fa daemon already holds` from the flock). If `SMOKE FAIL`, add the missing module as a `--hidden-import` / `--collect-submodules` in `build_daemon.sh` and rebuild (Step 4) until it passes. Do NOT proceed until SMOKE PASS.

- [ ] **Step 6: Commit**

```bash
git add packaging/daemon_entry.py packaging/build_daemon.sh .gitignore
git commit -m "feat: PyInstaller build of self-contained daemon"
```

---

### Task 2: Embed the daemon + agent plist into Auto2FA.app

**Files:**
- Create: `packaging/com.auto2fa.daemon.agent.plist`
- Modify: `auto2fa-mac/build.sh`, `auto2fa-mac/project.yml`

- [ ] **Step 1: Create the SMAppService agent plist**

`packaging/com.auto2fa.daemon.agent.plist` (uses `BundleProgram`, a bundle-relative path — NOT an absolute path):
```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.auto2fa.daemon</string>
    <key>BundleProgram</key>
    <string>Contents/Resources/daemon/auto2fa-daemon</string>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
    <key>StandardOutPath</key>
    <string>/tmp/auto2fa_daemon.log</string>
    <key>StandardErrorPath</key>
    <string>/tmp/auto2fa_daemon.log</string>
</dict>
</plist>
```

- [ ] **Step 2: Add an embed step to build.sh**

In `auto2fa-mac/build.sh`, after the `APP_PATH="build/Build/Products/$CONFIG/Auto2FA.app"` line and before the `if [ $RUN_AFTER … ]` block, insert:
```bash
# Embed the self-contained daemon + its SMAppService agent plist, unless
# AUTO2FA_SKIP_DAEMON=1 (fast pure-Swift dev iteration → source path at runtime).
if [ "${AUTO2FA_SKIP_DAEMON:-0}" != "1" ]; then
  echo "→ building + embedding daemon"
  ( cd .. && ./packaging/build_daemon.sh )
  DAEMON_SRC="../packaging/dist/auto2fa-daemon"
  RES_DIR="$APP_PATH/Contents/Resources/daemon"
  AGENTS_DIR="$APP_PATH/Contents/Library/LaunchAgents"
  rm -rf "$RES_DIR" && mkdir -p "$RES_DIR" "$AGENTS_DIR"
  cp -R "$DAEMON_SRC/." "$RES_DIR/"
  cp ../packaging/com.auto2fa.daemon.agent.plist "$AGENTS_DIR/com.auto2fa.daemon.plist"
  # Re-sign ad-hoc so the embedded code is covered by the bundle signature.
  codesign --force --deep --sign - "$APP_PATH" || true
  echo "→ embedded daemon at $RES_DIR"
else
  echo "→ AUTO2FA_SKIP_DAEMON=1 — skipping daemon embed (source path)"
fi
```

- [ ] **Step 3: Note the project.yml resource boundary**

No `project.yml` change is required because the embed happens post-`xcodebuild` in `build.sh` (copying into the built `.app`). Add a comment in `project.yml` under the `Auto2FA` target documenting that `Contents/Resources/daemon` and `Contents/Library/LaunchAgents` are injected by `build.sh`, so a reader doesn't expect them in the Xcode project:
```yaml
    # NOTE: Contents/Resources/daemon/ and Contents/Library/LaunchAgents/ are
    # injected post-build by build.sh (the embedded PyInstaller daemon + its
    # SMAppService agent plist). They are intentionally not Xcode resources.
```
Place this as a comment line inside the `Auto2FA:` target block (e.g. right after `type: application`).

- [ ] **Step 4: Build the app and verify the embed**

Run: `cd auto2fa-mac && ./build.sh`
Expected: `** BUILD SUCCEEDED **` then `→ embedded daemon at …/Auto2FA.app/Contents/Resources/daemon`.

Then verify the files and that the EMBEDDED daemon (inside the built bundle) runs in a clean env:
```bash
APP=auto2fa-mac/build/Build/Products/Debug/Auto2FA.app
test -x "$APP/Contents/Resources/daemon/auto2fa-daemon" && echo "daemon present"
test -f "$APP/Contents/Library/LaunchAgents/com.auto2fa.daemon.plist" && echo "agent plist present"
env -i HOME="$HOME" PATH="/usr/bin:/bin" "$APP/Contents/Resources/daemon/auto2fa-daemon" > /tmp/p1_embed.log 2>&1 &
P=$!; sleep 3; kill -9 $P 2>/dev/null
grep -Eq "ModuleNotFoundError|ImportError|Traceback" /tmp/p1_embed.log && echo "EMBED FAIL" || echo "EMBED PASS"
```
Expected: `daemon present`, `agent plist present`, `EMBED PASS`.

- [ ] **Step 5: Commit**

```bash
cd ~/logs/auto2fa_dev
git add packaging/com.auto2fa.daemon.agent.plist auto2fa-mac/build.sh auto2fa-mac/project.yml
git commit -m "feat: embed self-contained daemon + SMAppService agent plist into Auto2FA.app"
```

---

## P1b — Swift integration + de-personalization + docs

### Task 3: BundledDaemonAgent — SMAppService.agent wrapper

**Files:**
- Create: `auto2fa-mac/Auto2FA/BundledDaemonAgent.swift`

- [ ] **Step 1: Implement the wrapper** (mirrors `LoginItem.swift`)

`auto2fa-mac/Auto2FA/BundledDaemonAgent.swift`:
```swift
import Foundation
import ServiceManagement

/// Wraps SMAppService.agent for the embedded daemon (macOS 13+). The agent's
/// launchd plist ships inside the app bundle at
/// Contents/Library/LaunchAgents/com.auto2fa.daemon.plist and points (via
/// BundleProgram) at Contents/Resources/daemon/auto2fa-daemon. Registering it
/// makes launchd start the bundled daemon at login and keep it alive — without
/// any absolute paths that break when the app moves.
enum BundledDaemonAgent {
    static let plistName = "com.auto2fa.daemon.plist"

    /// True iff this build actually embeds the daemon (a bundled distribution,
    /// not a from-source dev build).
    static var isBundled: Bool {
        guard let res = Bundle.main.resourceURL else { return false }
        let daemon = res.appendingPathComponent("daemon/auto2fa-daemon")
        return FileManager.default.isExecutableFile(atPath: daemon.path)
    }

    @available(macOS 13.0, *)
    static var isRegistered: Bool {
        SMAppService.agent(plistName: plistName).status == .enabled
    }

    /// Register the agent so launchd runs the bundled daemon. Returns nil on
    /// success, or an error message (e.g. ad-hoc signing rejected it — the
    /// caller then falls back to spawning).
    @discardableResult
    @available(macOS 13.0, *)
    static func register() -> String? {
        let svc = SMAppService.agent(plistName: plistName)
        if svc.status == .enabled { return nil }
        do {
            try svc.register()
            return nil
        } catch {
            return error.localizedDescription
        }
    }
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cd auto2fa-mac && AUTO2FA_SKIP_DAEMON=1 ./build.sh`
Expected: `** BUILD SUCCEEDED **` (skip the daemon embed for a fast compile check).

- [ ] **Step 3: Commit**

```bash
cd ~/logs/auto2fa_dev
git add auto2fa-mac/Auto2FA/BundledDaemonAgent.swift
git commit -m "feat: BundledDaemonAgent SMAppService.agent wrapper"
```

---

### Task 4: DaemonProcess — bundled vs source path

**Files:**
- Modify: `auto2fa-mac/Auto2FA/DaemonProcess.swift`

- [ ] **Step 1: Branch ensureRunning() on bundled vs source**

In `auto2fa-mac/Auto2FA/DaemonProcess.swift`, find `func ensureRunning() async -> SpawnResult {`. Immediately after its first line (the `if DaemonProcess.socketResponds()` early-return block stays as-is), add a bundled branch BEFORE the `guard let projectDir = DaemonProcess.discoverProjectDir()` line. The function currently begins:
```swift
    func ensureRunning() async -> SpawnResult {
        if DaemonProcess.socketResponds() {
            NSLog("[Auto2FA] daemon already running; not spawning")
            return .alreadyRunning
        }

        guard let projectDir = DaemonProcess.discoverProjectDir() else {
```
Insert between the socketResponds block and the `guard let projectDir`:
```swift
        // Bundled distribution: let launchd run the embedded daemon via the
        // SMAppService agent — do not spawn it ourselves. Only fall through to
        // the source-spawn path if registration fails (e.g. ad-hoc signing).
        if BundledDaemonAgent.isBundled {
            if #available(macOS 13.0, *) {
                if let err = BundledDaemonAgent.register() {
                    NSLog("[Auto2FA] agent register failed (%@) — falling back to spawn", err)
                } else {
                    // Give launchd a moment to bring the socket up.
                    for _ in 0..<75 {
                        try? await Task.sleep(nanoseconds: 200_000_000)
                        if DaemonProcess.socketResponds() { return .alreadyRunning }
                    }
                    NSLog("[Auto2FA] agent registered but socket not up in 15s — falling back")
                }
            }
        }

        guard let projectDir = DaemonProcess.discoverProjectDir() else {
```

- [ ] **Step 2: Remove the personal default project dir**

In `DaemonProcess.discoverProjectDir()`, delete the "Default dev path" block:
```swift
        // 2. Default dev path
        let defaultPath = home + "/logs/auto2fa_dev"
        if fm.fileExists(atPath: defaultPath + "/auto2fa/daemon.py") {
            return defaultPath
        }

```
so the function falls straight from the `project-dir.txt` override to `return nil`. (P0's installer always writes `project-dir.txt`, so the personal fallback is no longer needed and must not ship to other users.)

- [ ] **Step 3: Verify it compiles**

Run: `cd auto2fa-mac && AUTO2FA_SKIP_DAEMON=1 ./build.sh`
Expected: `** BUILD SUCCEEDED **`.

- [ ] **Step 4: Commit**

```bash
cd ~/logs/auto2fa_dev
git add auto2fa-mac/Auto2FA/DaemonProcess.swift
git commit -m "feat: DaemonProcess bundled (SMAppService) vs source path; drop personal default dir"
```

---

### Task 5: De-personalize the Python CLI + a guard test

**Files:**
- Modify: `auto2fa/cli.py`
- Test: `tests/test_depersonalization.py`

- [ ] **Step 1: Write the failing test**

`tests/test_depersonalization.py`:
```python
"""Guard: shipped Python code must not hardcode personal machine paths or the
maintainer's cluster node names, so the project is shareable."""
from __future__ import annotations

import os
import re
import unittest

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SHIPPED = ["auto2fa/cli.py", "auto2fa/main.py", "auto2fa/backend.py",
           "auto2fa/tunnels.py", "auto2fa/daemon.py", "auto2fa/credentials.py",
           "auto2fa/installer.py", "auto2fa/ipc.py"]


class TestNoPersonalData(unittest.TestCase):
    def _read(self, rel):
        with open(os.path.join(REPO, rel)) as f:
            return f.read()

    def test_no_user_home_paths(self):
        for rel in SHIPPED:
            self.assertNotIn("/Users/", self._read(rel),
                             f"{rel} hardcodes a /Users/ path")

    def test_no_personal_node_names(self):
        # holygpuNNN and similar are this maintainer's Slurm nodes; examples
        # must use generic placeholders.
        pat = re.compile(r"holygpu\w*", re.IGNORECASE)
        for rel in SHIPPED:
            self.assertIsNone(pat.search(self._read(rel)),
                              f"{rel} contains a personal node name")


if __name__ == "__main__":
    unittest.main()
```

- [ ] **Step 2: Run it to see which files fail**

Run: `.venv/bin/python -m pytest tests/test_depersonalization.py -q`
Expected: FAIL — `auto2fa/cli.py` contains `holygpu08` (in the module docstring/help examples).

- [ ] **Step 3: De-personalize cli.py**

In `auto2fa/cli.py`, find example text containing `holygpu08` (the module docstring near the top has a line like `auto2fa node jupyter holygpu08      # set tunnel node and start`). Replace the node name with a generic placeholder, e.g.:
```python
    auto2fa node jupyter compute-node-01   # set tunnel node and start
```
Scan the rest of `cli.py` for any other personal host names (k6/k7/k8/b8/kempner used as examples) and replace example occurrences with generic placeholders like `myhost`. Do NOT change behavior, only help/doc strings.

- [ ] **Step 4: Run the guard + full suite**

Run: `.venv/bin/python -m pytest tests/test_depersonalization.py -q`
Expected: PASS.
Run: `.venv/bin/python -m pytest tests/ -q`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add auto2fa/cli.py tests/test_depersonalization.py
git commit -m "feat: de-personalize CLI help + guard test against personal data"
```

---

### Task 6: README distribution + Gatekeeper docs

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Add a "Sharing the app" section**

In `README.md`, after the Installation section, add:
````markdown
## Sharing the prebuilt app (macOS)

Build a self-contained `Auto2FA.app` that bundles its own Python daemon — the
recipient needs no Python and no repo:

```bash
cd auto2fa-mac
./build.sh release        # embeds the daemon; output under build/Build/Products/Release/
```

Zip `Auto2FA.app` and send it. Because the app is ad-hoc signed (no Apple
Developer ID), the recipient must clear the Gatekeeper quarantine once:

- **Right-click → Open** the first time (then confirm), or
- ```bash
  xattr -dr com.apple.quarantine /Applications/Auto2FA.app
  ```

Then drag it to `/Applications` and open it. It registers a login agent that
starts the bundled daemon automatically — no `python3 install.py` needed (that
flow is for running from source).
````

- [ ] **Step 2: Commit**

```bash
git add README.md
git commit -m "docs: sharing the prebuilt app + Gatekeeper workaround"
```

---

### Task 7: End-to-end verification (bundled + source paths)

**Files:** none (verification + final commit of any fixes)

- [ ] **Step 1: Full Python suite**

Run: `.venv/bin/python -m pytest tests/ -q`
Expected: all pass.

- [ ] **Step 2: Build the bundled app**

Run: `cd auto2fa-mac && ./build.sh`
Expected: `** BUILD SUCCEEDED **` + `→ embedded daemon at …`.

- [ ] **Step 3: Bundled-path e2e (NEEDS HUMAN to confirm the menu-bar UI)**

Install + launch the bundled app:
```bash
osascript -e 'quit app "Auto2FA"' 2>/dev/null; sleep 1
rm -rf /Applications/Auto2FA.app
cp -R auto2fa-mac/build/Build/Products/Debug/Auto2FA.app /Applications/
# stop the dev LaunchAgent so we observe the bundled agent specifically
launchctl bootout gui/$(id -u)/com.auto2fa.daemon 2>/dev/null
open /Applications/Auto2FA.app
sleep 8
echo "--- agent status ---"; launchctl print gui/$(id -u)/com.auto2fa.daemon 2>&1 | grep -E "state|program" | head
echo "--- socket serving? ---"; ~/logs/auto2fa_dev/.venv/bin/auto2fa list 2>&1 | head
```
Expected: the agent shows as running and `auto2fa list` prints hosts. **Ask the human to confirm the Auto2FA menu-bar icon appears and shows the host list.**

If SMAppService registration is rejected under ad-hoc signing (agent not running, log shows a registration error): invoke the **contingency** from the spec — change `packaging/com.auto2fa.daemon.agent.plist` consumption so `build.sh` instead writes a P0-style hand-written LaunchAgent pointing at `/Applications/Auto2FA.app/Contents/Resources/daemon/auto2fa-daemon`, and have `BundledDaemonAgent` install that via the existing `installer`-style approach. Re-verify. Report this pivot rather than forcing SMAppService.

- [ ] **Step 4: Source-path still works**

Restore the dev install and confirm the from-source path is intact:
```bash
cd ~/logs/auto2fa_dev && .venv/bin/auto2fa install && .venv/bin/auto2fa list | head
```
Expected: dev LaunchAgent reloads, `auto2fa list` prints hosts. (This proves P0's source path was not broken by the bundled changes.)

- [ ] **Step 5: De-personalization sweep**

Run:
```bash
grep -rn "/Users/shgao\|holygpu\|logs/auto2fa_dev" auto2fa/ auto2fa-mac/Auto2FA/ 2>/dev/null | grep -v "//" || echo "clean"
```
Expected: no shipped-code hits (comments/docs referencing the dev path are acceptable; the assertion is no hardcoded behavior depends on them). Fix any stragglers.

- [ ] **Step 6: Commit any fixes**

```bash
git add -A && git commit -m "test: P1 e2e verification + de-personalization sweep fixes"
```
(If no fixes were needed, skip the commit.)

---

## Self-Review

**Spec coverage:**
- Embed daemon via PyInstaller onedir — Task 1. ✓
- Embed into .app + agent plist (BundleProgram) — Task 2. ✓
- SMAppService agent wrapper — Task 3. ✓
- Bundled vs source dual path in DaemonProcess; remove personal default — Task 4. ✓
- De-personalize (cli help) + guard — Task 5; broader sweep — Task 7 Step 5. ✓
- Gatekeeper/distribution docs — Task 6. ✓
- Verification model (clean-env smoke, build e2e, source-path intact) — Tasks 1,2,7. ✓
- Contingency (ad-hoc SMAppService rejected → hand-written LaunchAgent into bundle) — Task 7 Step 3. ✓
- keyring hidden-import / exclude textual — Task 1. ✓

**Placeholder scan:** Build/Swift/e2e steps give exact commands and full code. The two inherently human/exploratory points (menu-bar confirmation in Task 7 Step 3; the contingency pivot) are explicitly flagged as such, not hidden TODOs.

**Type/name consistency:** `BundledDaemonAgent` (`isBundled`, `register()`, `plistName`, `isRegistered`) used identically in Task 3 (def) and Task 4 (call). Agent label `com.auto2fa.daemon` and bundle paths `Contents/Resources/daemon/auto2fa-daemon` + `Contents/Library/LaunchAgents/com.auto2fa.daemon.plist` consistent across Tasks 2, 3, 4, 7. `AUTO2FA_SKIP_DAEMON` env flag consistent across Tasks 2, 3, 4.
