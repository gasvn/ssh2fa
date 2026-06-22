# Touch ID Lock + Free iCloud-Drive Preference Sync — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add two opt-in macOS features — a Touch ID lock on the private windows, and free (no paid Apple account) iCloud-Drive sync of the app's UI preferences.

**Architecture:** A Foundation-only `SyncCore.swift` holds the two pure decision functions (`LockCore.shouldChallenge`, `SyncCore.resolve`) so they can be unit-tested in a **hostless** XCTest bundle (no app launch → no daemon side-effects). `BiometricLock` + `LockGate` (LocalAuthentication) gate the Dashboard and Logs `WindowGroup`s. `PreferenceSync` writes an allowlisted JSON into the iCloud Drive folder and reconciles last-writer-wins. Both features are off by default.

**Tech Stack:** Swift / SwiftUI, LocalAuthentication, NSFileCoordinator, XcodeGen, XCTest.

**Spec:** `docs/superpowers/specs/2026-06-10-touchid-lock-and-icloud-pref-sync-design.md`

**Conventions for every build/test command below** (run from repo root `/Users/shgao/logs/auto2fa_dev`):
- Regenerate project after editing `project.yml`: `cd auto2fa-mac && xcodegen generate && cd ..`
- Build: `xcodebuild -project auto2fa-mac/SSH2FA.xcodeproj -scheme SSH2FA -configuration Release -derivedDataPath /tmp/a2fa_dd build 2>&1 | grep -E "error:|BUILD"`
- Test: `xcodebuild test -project auto2fa-mac/SSH2FA.xcodeproj -scheme Auto2FATests -destination 'platform=macOS' 2>&1 | grep -E "error:|Test Suite|passed|failed"`
- SourceKit cross-file "Cannot find type" / "No such module" diagnostics are FALSE POSITIVES; `BUILD SUCCEEDED` is the gate.

---

### Task 1: Pure cores + hostless test target (TDD)

**Files:**
- Create: `auto2fa-mac/SSH2FA/SyncCore.swift`
- Create: `auto2fa-mac/Auto2FATests/SyncCoreTests.swift`
- Modify: `auto2fa-mac/project.yml`

- [ ] **Step 1: Add the test target + scheme to `project.yml`**

Append a `Auto2FATests` target under the existing `targets:` map (after the `SSH2FA:` target block, keeping `SSH2FA:` unchanged), and add a top-level `schemes:` block at the end of the file:

```yaml
  Auto2FATests:
    type: bundle.unit-test
    platform: macOS
    sources:
      # Compile the pure core DIRECTLY into the test bundle (Foundation-only, no
      # app host) so tests run headlessly — the app never launches, so its
      # daemon-install / spawn side-effects never fire during `xcodebuild test`.
      - path: Auto2FATests
      - path: SSH2FA/SyncCore.swift
    settings:
      base:
        GENERATE_INFOPLIST_FILE: YES
        PRODUCT_BUNDLE_IDENTIFIER: com.ssh2fa.tests

schemes:
  Auto2FATests:
    build:
      targets:
        Auto2FATests: [test]
    test:
      targets:
        - Auto2FATests
```

- [ ] **Step 2: Create `SSH2FA/SyncCore.swift` as a stub (so the test target compiles a file but the symbols are missing → RED)**

```swift
import Foundation
// Implemented in Step 5.
```

- [ ] **Step 3: Write the failing tests `Auto2FATests/SyncCoreTests.swift`**

```swift
import XCTest

// LockCore / SyncCore / SyncPayload are compiled into THIS test bundle via
// project.yml (sources include SSH2FA/SyncCore.swift) — same module, no import.
final class SyncCoreTests: XCTestCase {
    // MARK: LockCore.shouldChallenge
    func testLockDisabledNeverChallenges() {
        XCTAssertFalse(LockCore.shouldChallenge(enabled: false, lastAuth: nil,
            now: Date(), grace: 60))
    }
    func testLockNoPriorAuthChallenges() {
        XCTAssertTrue(LockCore.shouldChallenge(enabled: true, lastAuth: nil,
            now: Date(), grace: 60))
    }
    func testLockWithinGraceDoesNotChallenge() {
        let now = Date(timeIntervalSince1970: 1000)
        let last = Date(timeIntervalSince1970: 970)   // 30s ago, grace 60
        XCTAssertFalse(LockCore.shouldChallenge(enabled: true, lastAuth: last,
            now: now, grace: 60))
    }
    func testLockPastGraceChallenges() {
        let now = Date(timeIntervalSince1970: 1000)
        let last = Date(timeIntervalSince1970: 930)   // 70s ago, grace 60
        XCTAssertTrue(LockCore.shouldChallenge(enabled: true, lastAuth: last,
            now: now, grace: 60))
    }

    // MARK: SyncCore.resolve
    func testResolveNoRemoteWritesLocal() {
        XCTAssertEqual(SyncCore.resolve(remoteUpdatedAt: nil,
            lastAppliedRemoteAt: 0, localLastWriteAt: 0), .writeLocal)
    }
    func testResolveRemoteNewerApplies() {
        XCTAssertEqual(SyncCore.resolve(remoteUpdatedAt: 200,
            lastAppliedRemoteAt: 100, localLastWriteAt: 100), .applyRemote)
    }
    func testResolveLocalNewerWrites() {
        XCTAssertEqual(SyncCore.resolve(remoteUpdatedAt: 100,
            lastAppliedRemoteAt: 100, localLastWriteAt: 200), .writeLocal)
    }
    func testResolveAlreadyAppliedNoop() {
        XCTAssertEqual(SyncCore.resolve(remoteUpdatedAt: 100,
            lastAppliedRemoteAt: 100, localLastWriteAt: 100), .noop)
    }

    // MARK: SyncPayload round-trip
    func testPayloadRoundTrip() throws {
        let p = SyncPayload(version: 1, updatedAt: 12345.0,
            values: ["a": true, "b": false])
        let data = try JSONEncoder().encode(p)
        let back = try JSONDecoder().decode(SyncPayload.self, from: data)
        XCTAssertEqual(p, back)
    }
}
```

- [ ] **Step 4: Regenerate + run tests → expect FAIL (compile: undefined symbols)**

Run:
```bash
cd auto2fa-mac && xcodegen generate && cd ..
xcodebuild test -project auto2fa-mac/SSH2FA.xcodeproj -scheme Auto2FATests -destination 'platform=macOS' 2>&1 | grep -E "error:|Test Suite|passed|failed"
```
Expected: compile errors like "cannot find 'LockCore' in scope" (RED).

- [ ] **Step 5: Implement `SSH2FA/SyncCore.swift` (replace the stub)**

```swift
import Foundation

/// Pure decision logic for the Touch ID gate. No LocalAuthentication import so
/// it compiles headlessly into the test bundle.
enum LockCore {
    static func shouldChallenge(enabled: Bool, lastAuth: Date?, now: Date,
                                grace: TimeInterval) -> Bool {
        guard enabled else { return false }
        if let lastAuth, now.timeIntervalSince(lastAuth) < grace { return false }
        return true
    }
}

/// On-disk shape of the synced preferences file (in iCloud Drive).
struct SyncPayload: Codable, Equatable {
    var version: Int
    var updatedAt: Double           // epoch seconds (wall clock)
    var values: [String: Bool]
}

enum SyncResolution: Equatable { case applyRemote, writeLocal, noop }

/// Pure last-writer-wins reconcile. No file I/O so it unit-tests headlessly.
enum SyncCore {
    static func resolve(remoteUpdatedAt: Double?, lastAppliedRemoteAt: Double,
                        localLastWriteAt: Double) -> SyncResolution {
        guard let r = remoteUpdatedAt else { return .writeLocal }   // seed file
        if r > lastAppliedRemoteAt && r > localLastWriteAt { return .applyRemote }
        if localLastWriteAt > r { return .writeLocal }
        return .noop
    }
}
```

- [ ] **Step 6: Run tests → expect PASS**

Run:
```bash
xcodebuild test -project auto2fa-mac/SSH2FA.xcodeproj -scheme Auto2FATests -destination 'platform=macOS' 2>&1 | grep -E "Test Suite|passed|failed"
```
Expected: "Test Suite 'SyncCoreTests' passed", 9 tests, 0 failures.

- [ ] **Step 7: Commit**

```bash
git add auto2fa-mac/project.yml auto2fa-mac/SSH2FA/SyncCore.swift auto2fa-mac/Auto2FATests/SyncCoreTests.swift
git commit -m "feat(ui): pure cores for Touch ID gate + pref-sync reconcile, with hostless test target"
```

---

### Task 2: `BiometricLock` + `LockGate` + Settings toggle (compiles, not yet wired to windows)

**Files:**
- Create: `auto2fa-mac/SSH2FA/BiometricLock.swift`
- Modify: `auto2fa-mac/SSH2FA/Settings.swift` (SettingsKey + @AppStorage + Privacy section)

- [ ] **Step 1: Create `SSH2FA/BiometricLock.swift`**

```swift
import Foundation
import SwiftUI
import LocalAuthentication

/// Optional biometric gate for the app's private windows. Uses
/// `deviceOwnerAuthentication` (Touch ID with a Mac-password fallback), which
/// needs NO entitlement on this non-sandboxed app.
@MainActor
final class BiometricLock: ObservableObject {
    /// When the user last authenticated successfully — drives the grace window.
    @Published var lastSuccessfulAuth: Date?

    /// Seconds after a success during which re-opening a gated window does NOT
    /// re-prompt ("re-lock on close, with a grace period").
    static let graceInterval: TimeInterval = 60

    var enabled: Bool { UserDefaults.standard.bool(forKey: SettingsKey.requireTouchID) }

    func shouldChallengeNow() -> Bool {
        LockCore.shouldChallenge(enabled: enabled, lastAuth: lastSuccessfulAuth,
                                 now: Date(), grace: BiometricLock.graceInterval)
    }

    /// Can the device evaluate owner auth at all (biometrics OR a login password)?
    static func availability() -> (ok: Bool, reason: String?) {
        let ctx = LAContext()
        var err: NSError?
        let ok = ctx.canEvaluatePolicy(.deviceOwnerAuthentication, error: &err)
        return (ok, err?.localizedDescription)
    }

    /// Prompt for auth. A FRESH LAContext per call (reuse caches a prior result).
    func authenticate() async -> Bool {
        let ctx = LAContext()
        let ok: Bool = await withCheckedContinuation { cont in
            ctx.evaluatePolicy(.deviceOwnerAuthentication,
                               localizedReason: "Unlock SSH2FA") { success, _ in
                cont.resume(returning: success)
            }
        }
        if ok { lastSuccessfulAuth = Date() }
        return ok
    }
}

/// Wraps a window's content; when the lock is engaged it shows `LockedView` and
/// requires auth before revealing `content`. Re-evaluates on appear and when the
/// app becomes active, so an unattended open window re-locks after the grace.
struct LockGate<Content: View>: View {
    @EnvironmentObject private var lock: BiometricLock
    @Environment(\.scenePhase) private var scenePhase
    @State private var unlocked = false
    @State private var authing = false
    @ViewBuilder var content: () -> Content

    var body: some View {
        Group {
            if unlocked {
                content()
            } else {
                LockedView(authing: authing) { Task { await attempt() } }
            }
        }
        .onAppear { evaluate() }
        .onChange(of: scenePhase) { _, phase in if phase == .active { evaluate() } }
        .onChange(of: lock.lastSuccessfulAuth) { _, _ in evaluate() }
    }

    private func evaluate() {
        if !lock.shouldChallengeNow() {
            unlocked = true
        } else if !BiometricLock.availability().ok {
            // Fail OPEN — never trap the user out of their own app when neither
            // biometrics nor a login password can satisfy the policy.
            unlocked = true
        } else {
            unlocked = false
            Task { await attempt() }
        }
    }

    private func attempt() async {
        guard !authing else { return }
        authing = true
        let ok = await lock.authenticate()
        authing = false
        if ok { unlocked = true }
    }
}

struct LockedView: View {
    let authing: Bool
    let unlock: () -> Void
    var body: some View {
        VStack(spacing: 16) {
            Image(systemName: "lock.fill")
                .font(.system(size: 40)).foregroundStyle(.secondary)
            Text("SSH2FA is locked").font(.title3)
            Button(authing ? "Authenticating…" : "Unlock", action: unlock)
                .controlSize(.large)
                .disabled(authing)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding(40)
    }
}
```

- [ ] **Step 2: Add the setting key + storage + Privacy section in `Settings.swift`**

In the `SettingsKey` enum, after `static let notchDoNotDisturb = ...`, add:
```swift
    static let requireTouchID = "auto2fa.security.requireTouchID"
```

In `SettingsView`'s `@AppStorage` block (after `compactRows`), add:
```swift
    @AppStorage(SettingsKey.requireTouchID) private var requireTouchID = false
```

In the `Form`, immediately AFTER the `} header: { Text("Daemon") }` section's closing and BEFORE the `}` that closes the `Form`, add a new section:
```swift
                Section {
                    Toggle("Require Touch ID to open the dashboard", isOn: $requireTouchID)
                    Text("Locks the dashboard and log windows behind Touch ID (falls back to your Mac login password). The menu-bar icon stays visible. Re-locks ~60s after you close the window.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    if requireTouchID && !BiometricLock.availability().ok {
                        Text("⚠︎ This Mac can't evaluate Touch ID or a login password right now — the lock may not engage.")
                            .font(.caption)
                            .foregroundStyle(.orange)
                    }
                } header: { Text("Privacy & Security") }
```

- [ ] **Step 3: Build → expect SUCCESS**

Run:
```bash
xcodebuild -project auto2fa-mac/SSH2FA.xcodeproj -scheme SSH2FA -configuration Release -derivedDataPath /tmp/a2fa_dd build 2>&1 | grep -E "error:|BUILD"
```
Expected: `** BUILD SUCCEEDED **`.

- [ ] **Step 4: Commit**

```bash
git add auto2fa-mac/SSH2FA/BiometricLock.swift auto2fa-mac/SSH2FA/Settings.swift
git commit -m "feat(ui): Touch ID lock — BiometricLock + LockGate + Privacy setting"
```

---

### Task 3: Wire `LockGate` around the Dashboard + Logs windows

**Files:**
- Modify: `auto2fa-mac/SSH2FA/Auto2FAApp.swift`

- [ ] **Step 1: Own a shared `BiometricLock`**

After `@StateObject private var menuBar = MenuBarController()` add:
```swift
    @StateObject private var biometricLock = BiometricLock()
```

- [ ] **Step 2: Wrap the Dashboard window content in `LockGate`**

Replace the `WindowGroup("SSH2FA") { ... }` body so the gate wraps `ContentView`, while the `.onAppear` / `.task` (daemon bootstrap, menu-bar install) stay on the OUTER view so they run even while the window is locked:

```swift
        WindowGroup("SSH2FA") {
            LockGate {
                ContentView()
                    .environmentObject(appState)
            }
            .environmentObject(biometricLock)
            .onAppear {
                SingleInstance.enforceOrExit()
                installMenuBarOnce()
                installSleepWakeMonitor()
                installNetworkMonitor()
                installNotificationHandling()
            }
            .task {
                let spawnAllowed = UserDefaults.standard
                    .object(forKey: SettingsKey.spawnDaemonOnLaunch) as? Bool ?? true
                if spawnAllowed {
                    DaemonProcess.shared.installBundledDaemonIfNeeded()
                    let result = await DaemonProcess.shared.ensureRunning()
                    switch result {
                    case .alreadyRunning:
                        NSLog("[SSH2FA] daemon was already running")
                    case .spawned(let pid):
                        NSLog("[SSH2FA] spawned daemon, PID=\(pid)")
                    case .failed(let reason):
                        appState.connectionError = reason
                    }
                } else {
                    NSLog("[SSH2FA] spawnDaemonOnLaunch=off; assuming external daemon")
                }
                await appState.bootstrap()
            }
        }
        .defaultSize(width: 820, height: 540)
        .windowToolbarStyle(.unifiedCompact)
```

(The `.commands { ... }` block that follows stays exactly as-is.)

- [ ] **Step 3: Wrap the Logs window content in `LockGate`**

Replace the `WindowGroup("SSH2FA Logs", id: "logs") { ... }` block with:
```swift
        WindowGroup("SSH2FA Logs", id: "logs") {
            LockGate {
                LogViewerView()
                    .environmentObject(appState)
            }
            .environmentObject(biometricLock)
        }
        .defaultSize(width: 900, height: 600)
```

- [ ] **Step 4: Build → expect SUCCESS**

Run:
```bash
xcodebuild -project auto2fa-mac/SSH2FA.xcodeproj -scheme SSH2FA -configuration Release -derivedDataPath /tmp/a2fa_dd build 2>&1 | grep -E "error:|BUILD"
```
Expected: `** BUILD SUCCEEDED **`.

- [ ] **Step 5: Commit**

```bash
git add auto2fa-mac/SSH2FA/Auto2FAApp.swift
git commit -m "feat(ui): gate dashboard + logs windows behind Touch ID LockGate"
```

---

### Task 4: `PreferenceSync` + Sync setting + app wiring

**Files:**
- Create: `auto2fa-mac/SSH2FA/PreferenceSync.swift`
- Modify: `auto2fa-mac/SSH2FA/Settings.swift` (SettingsKey + @AppStorage + Sync section + availability helper)
- Modify: `auto2fa-mac/SSH2FA/Auto2FAApp.swift` (own + start it)

- [ ] **Step 1: Create `SSH2FA/PreferenceSync.swift`**

```swift
import Foundation
import AppKit

/// Free, entitlement-less preference sync across the user's Macs: writes an
/// allowlisted JSON of UI preferences into the iCloud Drive folder, which iCloud
/// syncs. NOT NSUbiquitousKeyValueStore (that needs the paid iCloud entitlement).
/// Carries ONLY UI prefs — never secrets or host/tunnel configs.
@MainActor
final class PreferenceSync {
    /// Allowlist of UserDefaults keys to sync. UI preferences only.
    static let syncedKeys: [String] = [
        SettingsKey.notchEnabled, SettingsKey.notchPersistent,
        SettingsKey.notchDoNotDisturb, SettingsKey.autoOpenBrowser,
        SettingsKey.autoRecoverOnWake, SettingsKey.spawnDaemonOnLaunch,
        SettingsKey.compactRows, SettingsKey.requireTouchID,
    ]
    // Device-local bookkeeping (NOT synced).
    private static let kLocalLastWrite = "auto2fa.sync._localLastWriteAt"
    private static let kLastAppliedRemote = "auto2fa.sync._lastAppliedRemoteAt"

    private let defaults = UserDefaults.standard
    private var isApplyingRemote = false
    private var wasEnabled = false
    private var lastWrittenSnapshot: [String: Bool] = [:]
    private var debounceTask: Task<Void, Never>?
    private var observers: [NSObjectProtocol] = []

    var enabled: Bool { defaults.bool(forKey: SettingsKey.syncPrefsViaICloud) }

    static var iCloudDir: String {
        NSHomeDirectory() + "/Library/Mobile Documents/com~apple~CloudDocs/SSH2FA"
    }
    static var fileURL: URL { URL(fileURLWithPath: iCloudDir + "/settings.json") }

    func iCloudAvailable() -> Bool {
        FileManager.default.fileExists(atPath:
            NSHomeDirectory() + "/Library/Mobile Documents/com~apple~CloudDocs")
    }

    func start() {
        wasEnabled = enabled
        let nc = NotificationCenter.default
        observers.append(nc.addObserver(forName: UserDefaults.didChangeNotification,
            object: nil, queue: .main) { [weak self] _ in
            Task { @MainActor in self?.onLocalChange() }
        })
        observers.append(nc.addObserver(forName: NSApplication.didBecomeActiveNotification,
            object: nil, queue: .main) { [weak self] _ in
            Task { @MainActor in self?.reconcile() }
        })
        reconcile()
    }

    func reconcile() {
        guard enabled, iCloudAvailable() else { return }
        let remote = readRemote()
        let applied = defaults.double(forKey: PreferenceSync.kLastAppliedRemote)
        let local = defaults.double(forKey: PreferenceSync.kLocalLastWrite)
        switch SyncCore.resolve(remoteUpdatedAt: remote?.updatedAt,
                                lastAppliedRemoteAt: applied, localLastWriteAt: local) {
        case .applyRemote: if let remote { applyRemote(remote) }
        case .writeLocal:  writeLocal()
        case .noop:        lastWrittenSnapshot = currentSnapshot()
        }
    }

    private func onLocalChange() {
        // The sync toggle flipping ON -> immediate reconcile (it is NOT a synced
        // key, so the snapshot diff below would otherwise ignore it).
        let nowEnabled = enabled
        if nowEnabled && !wasEnabled { wasEnabled = true; reconcile(); return }
        wasEnabled = nowEnabled
        guard nowEnabled, !isApplyingRemote else { return }
        // didChangeNotification fires for EVERY key; only write if a synced value
        // actually changed (avoids constant churn + false "local wins").
        let snap = currentSnapshot()
        guard snap != lastWrittenSnapshot else { return }
        debounceTask?.cancel()
        debounceTask = Task { [weak self] in
            try? await Task.sleep(nanoseconds: 1_000_000_000)
            guard let self, !Task.isCancelled else { return }
            self.writeLocal()
        }
    }

    private func currentSnapshot() -> [String: Bool] {
        var s: [String: Bool] = [:]
        for k in PreferenceSync.syncedKeys { s[k] = defaults.bool(forKey: k) }
        return s
    }

    private func readRemote() -> SyncPayload? {
        let coordinator = NSFileCoordinator()
        var coordErr: NSError?
        var result: SyncPayload?
        coordinator.coordinate(readingItemAt: PreferenceSync.fileURL,
                               options: [], error: &coordErr) { url in
            guard let data = try? Data(contentsOf: url) else { return }
            result = try? JSONDecoder().decode(SyncPayload.self, from: data)
        }
        return result
    }

    private func writeLocal() {
        guard iCloudAvailable() else { return }
        let snap = currentSnapshot()
        let now = Date().timeIntervalSince1970
        let payload = SyncPayload(version: 1, updatedAt: now, values: snap)
        guard let data = try? JSONEncoder().encode(payload) else { return }
        try? FileManager.default.createDirectory(atPath: PreferenceSync.iCloudDir,
            withIntermediateDirectories: true)
        let coordinator = NSFileCoordinator()
        var coordErr: NSError?
        coordinator.coordinate(writingItemAt: PreferenceSync.fileURL,
                               options: .forReplacing, error: &coordErr) { url in
            try? data.write(to: url, options: .atomic)
        }
        lastWrittenSnapshot = snap
        defaults.set(now, forKey: PreferenceSync.kLocalLastWrite)
        defaults.set(now, forKey: PreferenceSync.kLastAppliedRemote)
    }

    private func applyRemote(_ payload: SyncPayload) {
        isApplyingRemote = true
        for k in PreferenceSync.syncedKeys {
            if let v = payload.values[k] { defaults.set(v, forKey: k) }
        }
        lastWrittenSnapshot = currentSnapshot()
        defaults.set(payload.updatedAt, forKey: PreferenceSync.kLastAppliedRemote)
        isApplyingRemote = false
    }
}
```

- [ ] **Step 2: Add the setting key + storage + Sync section in `Settings.swift`**

In `SettingsKey`, after `requireTouchID`, add:
```swift
    static let syncPrefsViaICloud = "auto2fa.sync.icloudPrefs"
```

In `SettingsView`'s `@AppStorage` block (after `requireTouchID`), add:
```swift
    @AppStorage(SettingsKey.syncPrefsViaICloud) private var syncPrefsViaICloud = false
```

Add a computed helper inside `SettingsView` (after the `@State private var launchAtLoginError` line):
```swift
    private var iCloudDriveAvailable: Bool {
        FileManager.default.fileExists(atPath:
            NSHomeDirectory() + "/Library/Mobile Documents/com~apple~CloudDocs")
    }
```

In the `Form`, AFTER the new `Privacy & Security` section and before the `Form`'s closing `}`, add:
```swift
                Section {
                    Toggle("Sync preferences via iCloud Drive (free)", isOn: $syncPrefsViaICloud)
                    Text("Syncs only these app preferences across your Macs via a file in iCloud Drive — no paid Apple Developer account needed. Never includes your hosts, tunnels, or 2FA secret.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    if syncPrefsViaICloud && !iCloudDriveAvailable {
                        Text("⚠︎ iCloud Drive isn't available — turn it on in System Settings to sync.")
                            .font(.caption)
                            .foregroundStyle(.orange)
                    }
                } header: { Text("Sync") }
```

- [ ] **Step 3: Own + start `PreferenceSync` in `Auto2FAApp.swift`**

After `@State private var networkMonitor: NetworkMonitor?` add:
```swift
    @State private var preferenceSync: PreferenceSync?
```

Add this method alongside the other `install…` methods (e.g. after `installNetworkMonitor()`):
```swift
    /// Start free iCloud-Drive preference sync (no-op unless the user opted in
    /// and is signed into iCloud Drive).
    private func installPreferenceSync() {
        guard preferenceSync == nil else { return }
        let sync = PreferenceSync()
        sync.start()
        preferenceSync = sync
    }
```

In the Dashboard window's outer `.onAppear` (from Task 3 Step 2), add `installPreferenceSync()` as the last call:
```swift
            .onAppear {
                SingleInstance.enforceOrExit()
                installMenuBarOnce()
                installSleepWakeMonitor()
                installNetworkMonitor()
                installNotificationHandling()
                installPreferenceSync()
            }
```

- [ ] **Step 4: Build → expect SUCCESS**

Run:
```bash
xcodebuild -project auto2fa-mac/SSH2FA.xcodeproj -scheme SSH2FA -configuration Release -derivedDataPath /tmp/a2fa_dd build 2>&1 | grep -E "error:|BUILD"
```
Expected: `** BUILD SUCCEEDED **`.

- [ ] **Step 5: Commit**

```bash
git add auto2fa-mac/SSH2FA/PreferenceSync.swift auto2fa-mac/SSH2FA/Settings.swift auto2fa-mac/SSH2FA/Auto2FAApp.swift
git commit -m "feat(ui): free iCloud-Drive preference sync (opt-in, prefs only)"
```

---

### Task 5: Full verification + push

**Files:** none (verification only)

- [ ] **Step 1: Run the logic tests → expect PASS**

```bash
xcodebuild test -project auto2fa-mac/SSH2FA.xcodeproj -scheme Auto2FATests -destination 'platform=macOS' 2>&1 | grep -E "Test Suite|passed|failed"
```
Expected: SyncCoreTests passed, 9 tests, 0 failures.

- [ ] **Step 2: Full release build → expect SUCCESS**

```bash
xcodebuild -project auto2fa-mac/SSH2FA.xcodeproj -scheme SSH2FA -configuration Release -derivedDataPath /tmp/a2fa_dd build 2>&1 | grep -E "error:|BUILD"
```
Expected: `** BUILD SUCCEEDED **`.

- [ ] **Step 3: Push both branches**

```bash
git push origin rust-rewrite
git checkout main -q && git merge --ff-only rust-rewrite -q && git push origin main && git checkout rust-rewrite -q
```

- [ ] **Step 4: (Optional, operator) Deploy + manual smoke**

Rebuild + deploy the packaged app (`auto2fa-mac/package-app.sh` → replace `/Applications/SSH2FA.app`), then manually verify:
- Settings → Privacy & Security → enable "Require Touch ID" → close + reopen the dashboard → Touch ID prompt appears; cancel → LockedView with Unlock; authenticate → content.
- Reopen within ~60s → no prompt (grace). After ~60s → prompt again.
- Settings → Sync → enable iCloud sync (with iCloud Drive on) → confirm `~/Library/Mobile Documents/com~apple~CloudDocs/SSH2FA/settings.json` appears and toggling a pref updates its `updatedAt`.

---

## Self-Review

**Spec coverage:**
- Touch ID gate on dashboard + logs → Task 3. ✓
- `deviceOwnerAuthentication` + fresh LAContext + grace → Task 2 Step 1. ✓
- Fail-open when availability false → Task 2 `evaluate()`. ✓
- Privacy setting + availability warning → Task 2 Step 2. ✓
- Free iCloud-Drive sync, allowlist, NSFileCoordinator, manual path → Task 4 Step 1. ✓
- Reconcile last-writer-wins + onLocalChange snapshot-diff + isApplyingRemote guard + toggle-on reconcile → Task 4 Step 1. ✓
- Triggers (launch, didBecomeActive, didChange debounced) → Task 4 Steps 1 & 3. ✓
- Graceful iCloud-unavailable degrade → `iCloudAvailable()` guards + Settings warning. ✓
- Hostless XCTest target over the two pure cores + payload round-trip → Task 1. ✓

**Placeholder scan:** none — every code step is complete.

**Type consistency:** `LockCore.shouldChallenge`, `SyncCore.resolve`, `SyncResolution` (`.applyRemote/.writeLocal/.noop`), `SyncPayload(version:updatedAt:values:)`, `BiometricLock.shouldChallengeNow/availability/authenticate`, `SettingsKey.requireTouchID/syncPrefsViaICloud` — names used identically across Tasks 1–4. ✓

Note: the pure-core types are compiled into BOTH the app target and the test bundle (separate modules, no shared state) — intentional, so tests stay hostless and never trigger the app's daemon side-effects.
