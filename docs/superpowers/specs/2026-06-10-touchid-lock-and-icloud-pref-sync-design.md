# Design — Touch ID lock + free iCloud-Drive preference sync

Date: 2026-06-10
Status: approved (awaiting spec review → implementation plan)
Scope: macOS app only (`auto2fa-mac/`). No Rust/daemon changes.

## Goal

Two user-requested features, both on the **no-paid-account** path:

1. **Touch ID lock** — optionally require biometric (Touch ID, with Mac-password
   fallback) auth to open the windows that expose private data.
2. **iCloud preference sync (free)** — optionally sync the app's UI preferences
   across the user's Macs *without* the paid Apple Developer Program, by writing
   a file into the user's iCloud Drive folder.

Both are **off by default** (no behavior change unless the user opts in).

## Why this approach (cost constraint)

- **Touch ID** uses `LocalAuthentication`, which needs **no entitlement** on a
  non-sandboxed app — works with the existing free "Apple Development" cert.
- **iCloud KVS** (`NSUbiquitousKeyValueStore`) is the "proper" sync API but the
  iCloud capability is **paid-program only** (the free personal team cannot
  provision the `ubiquity-kvstore` entitlement). So instead we use the
  **entitlement-free** trick: a non-sandboxed app can read/write a plain file in
  `~/Library/Mobile Documents/com~apple~CloudDocs/…` (the iCloud Drive
  container), and iCloud Drive syncs it across the user's Macs. No fee, no
  entitlement, no provisioning.

## Non-goals (YAGNI)

- NOT syncing host/tunnel configs or any secret (TOTP secret, SSH passwords).
  Those live daemon-side (Keychain + JSON) and must not enter iCloud — keeping
  the single on-device factor on-device is the safer choice.
- NOT using `NSUbiquitousKeyValueStore` (paid) or `url(forUbiquityContainerIdentifier:)`
  (also entitlement-gated). Path is constructed manually.
- NOT per-key conflict merge — file-level **last-writer-wins** is sufficient for
  ~8 boolean prefs.
- Touch ID does NOT gate the menu-bar icon or individual actions — **window-level
  gate only** (the menu-bar item shows aggregate counts only and must stay
  always-visible).

---

## Feature 1 — Touch ID lock

### What it gates
The **Dashboard window** (`WindowGroup "Auto2FA"`) and the **Logs window**
(`WindowGroup id: "logs"`). These are the private surfaces: host list, usernames,
tunnel configs, the live rotating TOTP code chip (`TOTPCodeChip`), and log lines
that can contain host names. The **menu-bar status item stays open** (aggregate
counts only).

### Component: `BiometricLock` (new `Auto2FA/BiometricLock.swift`)
`@MainActor final class BiometricLock: ObservableObject`

State:
- `@Published var lastSuccessfulAuth: Date?` — when the user last authenticated.
- reads `enabled` from `UserDefaults` (`SettingsKey.requireTouchID`).

API:
- `static func availability() -> (ok: Bool, reason: String?)` — wraps
  `LAContext().canEvaluatePolicy(.deviceOwnerAuthentication, error:)`. Used by the
  Settings toggle to warn if neither biometrics nor a password are available
  (rare, but e.g. no password set).
- `func authenticate() async -> Bool` — creates a **fresh** `LAContext`, calls
  `evaluatePolicy(.deviceOwnerAuthentication, localizedReason: "Unlock Auto2FA")`
  (bridged to async via `withCheckedContinuation`). On success sets
  `lastSuccessfulAuth = Date()` and returns true.
- `func markLocked()` — sets `lastSuccessfulAuth = nil` (forces a prompt next
  open). Not strictly needed given the grace logic, but available for an explicit
  "Lock now" affordance later.

**Pure, testable core** (free function or static, no `LAContext`):
```
func shouldChallenge(enabled: Bool, lastAuth: Date?, now: Date,
                     grace: TimeInterval) -> Bool
// !enabled                                  -> false
// lastAuth within `grace` of now            -> false
// otherwise                                 -> true
```
Default `grace = 60` seconds. This implements "re-lock when the window closes,
with a 60 s grace": each window open re-evaluates `shouldChallenge`; reopening
within 60 s of the last successful auth does not re-prompt; after 60 s it does.

### Component: `LockGate` (SwiftUI view wrapper, in `BiometricLock.swift`)
`LockGate { <window content> }` observes the shared `BiometricLock`:
- On appear (and on `scenePhase`/active change): if
  `shouldChallenge(...)` is false → render the wrapped content. Else render
  `LockedView` and kick off `authenticate()`.
- `LockedView`: centered lock SF Symbol + app name + **"Unlock"** button.
  Auto-triggers `authenticate()` once on appear; the button retries after a
  cancel/failure. On success the gate swaps to the content.
- The Dashboard and Logs `WindowGroup` root views are each wrapped in `LockGate`.

A single shared `BiometricLock` instance is injected via `@EnvironmentObject`
(created in `Auto2FAApp`), so both windows share `lastSuccessfulAuth` (and thus
the grace window).

### Settings
- New key `SettingsKey.requireTouchID = "auto2fa.security.requireTouchID"`
  (default `false`).
- New **"Privacy & Security"** `Section` with toggle **"Require Touch ID to open
  the dashboard"** + caption noting it falls back to the Mac login password and
  also gates the Logs window. On enable, call `BiometricLock.availability()`; if
  not ok, show an inline warning (but still allow the toggle — the user may set a
  password later).

### Error handling
- `evaluatePolicy` failure/cancel → gate stays locked, "Unlock" button retries.
- No biometrics + no password (availability false) → toggle warns; if somehow
  enabled, the gate falls back to a non-blocking state (content shown) rather than
  trapping the user out of their own app. (Prefer lockout-safety over strictness.)

---

## Feature 2 — free iCloud-Drive preference sync

### Component: `PreferenceSync` (new `Auto2FA/PreferenceSync.swift`)
`@MainActor final class PreferenceSync`

- `static let syncedKeys: [String]` — **allowlist**: `notchEnabled`,
  `notchPersistent`, `notchDoNotDisturb`, `autoOpenBrowser`, `autoRecoverOnWake`,
  `spawnDaemonOnLaunch`, `compactRows`, `requireTouchID`. Explicitly **excludes**
  `welcomeShown` (device-local) and anything secret.
- reads `enabled` from `SettingsKey.syncPrefsViaICloud` (default `false`).
- iCloud Drive path (constructed manually, no entitlement):
  `NSHomeDirectory() + "/Library/Mobile Documents/com~apple~CloudDocs/Auto2FA/settings.json"`.
- `func iCloudAvailable() -> Bool` — the `com~apple~CloudDocs` dir exists iff
  iCloud Drive is on.

### File format
```json
{ "version": 1, "updatedAt": 1718000000.0, "values": { "auto2fa.notch.enabled": true, ... } }
```
`values` holds the typed pref values (booleans here). `updatedAt` is wall-clock
epoch seconds at the time of that write.

### Reconcile logic (last-writer-wins)
Device-local bookkeeping (in UserDefaults, NOT synced):
- `localLastWriteAt: Date` — when this device last changed a synced pref.
- `lastAppliedRemoteAt: Date` — `updatedAt` of the remote we last applied.

**Pure, testable core:**
```
enum Resolution { case applyRemote, writeLocal, noop }
func resolve(remoteUpdatedAt: Date?, lastAppliedRemoteAt: Date,
             localLastWriteAt: Date) -> Resolution
// remote newer than both what we applied AND our local change -> applyRemote
// our local change newer than remote (or no remote)           -> writeLocal
// otherwise                                                    -> noop
```

Operations (all file I/O via `NSFileCoordinator`, best-effort, never crash):
- `reconcile()`: coordinated read of the file → `remoteUpdatedAt` + values.
  `resolve(...)` decides:
  - `.applyRemote`: write each allowlisted value into `UserDefaults` **with the
    write-loop guard set** (see below); set `lastAppliedRemoteAt = remoteUpdatedAt`.
  - `.writeLocal`: coordinated write of current prefs with `updatedAt = max(now,
    localLastWriteAt)`; set `lastAppliedRemoteAt` to that.
  - `.noop`: nothing.
- `onLocalChange()` (from `UserDefaults.didChangeNotification`, debounced ~1 s):
  `UserDefaults.didChangeNotification` fires for *every* key (window-frame
  autosave, unrelated state), so first **diff the allowlisted snapshot** against
  the last-written snapshot; if no synced value actually changed, **return**
  (no write, no timestamp bump — avoids constant iCloud churn and false "local
  wins"). Only on a real synced-value change: set `localLastWriteAt = now`, then
  `writeLocal()`. Guarded so applying remote values doesn't recurse: an
  `isApplyingRemote` flag suppresses `onLocalChange` while `.applyRemote` runs.

### Triggers
- On launch (after `AppState` bootstrap) → `reconcile()`.
- `NSApplication.didBecomeActiveNotification` → `reconcile()`.
- `UserDefaults.didChangeNotification` (debounced) → `onLocalChange()`.
- Toggling the sync setting on → `reconcile()` (seeds the file if none).

### Settings
- New key `SettingsKey.syncPrefsViaICloud = "auto2fa.sync.icloudPrefs"`
  (default `false`).
- Toggle **"Sync preferences via iCloud Drive (free)"** in a **"Sync"** `Section`,
  with caption explaining it's preferences-only (no secrets/configs) and free.
- If `!iCloudAvailable()`, the toggle row shows "iCloud Drive not available"
  and the feature no-ops.

### Error handling
- Missing folder / not signed into iCloud → `iCloudAvailable()` false → no-op.
- Corrupt or partially-synced JSON → decode throws → treat as no remote →
  `writeLocal()` (heals the file).
- All coordinated reads/writes log on failure and return; no fatal paths.

---

## Testing

The app currently has **no Swift test target** (all tests are Rust). Add a
lightweight `Auto2FATests` XCTest target via `auto2fa-mac/project.yml`
(`@testable import Auto2FA`). Cover the two pure cores (no `LAContext` / no real
iCloud needed):

- `shouldChallenge`: disabled → false; `lastAuth == nil` → true; within grace →
  false; just past grace → true.
- `resolve`: remote-newer → applyRemote; local-newer → writeLocal; no-remote →
  writeLocal; equal/older-remote → noop.
- `PreferenceSync` JSON encode→decode round-trip preserves values + `updatedAt`.

UI wiring (LockGate over the windows, Settings toggles) verified via
`xcodebuild` BUILD SUCCEEDED + manual run.

## Files touched
- New: `Auto2FA/BiometricLock.swift`, `Auto2FA/PreferenceSync.swift`,
  `Auto2FATests/…` test files.
- Edit: `Auto2FA/Settings.swift` (3 keys + 2 sections/toggles),
  `Auto2FA/Auto2FAApp.swift` (own the two shared objects; wrap both WindowGroups
  in `LockGate`; wire sync triggers), `auto2fa-mac/project.yml` (add test target).

## Security note (for SECURITY.md follow-up)
The iCloud sync deliberately carries **only UI preferences**. The threat-model
statement that "the on-device TOTP secret collapses 2FA to single-factor" is
unchanged — no secret crosses iCloud. Touch ID adds an at-rest access barrier to
the UI surfaces but does not change where the secret lives.
