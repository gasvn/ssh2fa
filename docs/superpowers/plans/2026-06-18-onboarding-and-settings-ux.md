# Onboarding + Settings Clarity — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make a new user reach their first connected host via the easy import path, give them an in-app "Get started" checklist, and make Settings self-explanatory (fix unrendered backticks, add a "How it works" explainer, plain-language copy).

**Architecture:** All client-side SwiftUI, no daemon/Rust change. One new pure helper (`OnboardingChecklist`, unit-tested) + one new view (`GetStartedChecklist`) + targeted edits to `WelcomeSheet`, `HostsView`, `TerminalLauncher`, `AppState`, and `Settings`. Reuses the existing importer (`ImportHostsSheet`, `AddHostSheet` prefill) and first-connect notch.

**Tech Stack:** Swift 5.10 / SwiftUI / AppKit, XcodeGen (`project.yml`), `xcodebuild` test+build.

---

## Reference: build & test commands

From `auto2fa-mac/`:
```bash
cd /Users/shgao/logs/auto2fa_dev/auto2fa-mac && xcodegen generate    # after project.yml changes
# unit tests:
xcodebuild -project Auto2FA.xcodeproj -scheme Auto2FATests -destination 'platform=macOS' test 2>&1 | tail -25
# app build:
xcodebuild -project Auto2FA.xcodeproj -scheme Auto2FA -destination 'platform=macOS' build 2>&1 | tail -20
```
Single test class: add `-only-testing:Auto2FATests/<ClassName>`.

## File Structure

| File | Responsibility | New? | In test bundle? |
|------|----------------|------|-----------------|
| `Auto2FA/OnboardingChecklist.swift` | Pure: which Get-Started steps are done + whether to show the checklist | new | yes |
| `Auto2FA/Views/GetStartedChecklist.swift` | The checklist view (full empty-state mode + slim above-list mode) | new | no |
| `Auto2FA/TerminalLauncher.swift` | Set a "used a terminal at least once" flag | modify | no |
| `Auto2FA/Settings.swift` | `SettingsKey` additions; clarity pass (backticks, explainer, copy) | modify | no |
| `Auto2FA/Views/HostsView.swift` | Render the checklist (empty + onboarding-incomplete) | modify | no |
| `Auto2FA/Views/WelcomeSheet.swift` | Import-first first-run | modify | no |
| `Auto2FA/AppState.swift` | First-connect notch copy (drop backticks, name the Terminal action) | modify | no |
| `Auto2FATests/OnboardingChecklistTests.swift` | Unit tests for the pure helper | new | (is the bundle) |
| `project.yml` | Add `OnboardingChecklist.swift` to the test bundle | modify | — |

---

### Task 1: `OnboardingChecklist` pure helper (TDD)

**Files:**
- Create: `auto2fa-mac/Auto2FA/OnboardingChecklist.swift`
- Create: `auto2fa-mac/Auto2FATests/OnboardingChecklistTests.swift`
- Modify: `auto2fa-mac/project.yml`

- [ ] **Step 1: Write the failing test**

Create `auto2fa-mac/Auto2FATests/OnboardingChecklistTests.swift`:

```swift
import XCTest

final class OnboardingChecklistTests: XCTestCase {
    func testNothingDoneAtStart() {
        XCTAssertEqual(OnboardingChecklist.completed(hostCount: 0, anyConnected: false, usedTerminal: false), [])
    }

    func testStepsCompleteIndependently() {
        XCTAssertEqual(OnboardingChecklist.completed(hostCount: 1, anyConnected: false, usedTerminal: false), [.addHost])
        XCTAssertEqual(OnboardingChecklist.completed(hostCount: 2, anyConnected: true, usedTerminal: false), [.addHost, .seeConnect])
        XCTAssertEqual(OnboardingChecklist.completed(hostCount: 1, anyConnected: true, usedTerminal: true),
                       [.addHost, .seeConnect, .openTerminal])
    }

    func testShowsWhileIncomplete() {
        XCTAssertTrue(OnboardingChecklist.shouldShow(hostCount: 0, anyConnected: false, usedTerminal: false))
        XCTAssertTrue(OnboardingChecklist.shouldShow(hostCount: 1, anyConnected: false, usedTerminal: false))
        XCTAssertTrue(OnboardingChecklist.shouldShow(hostCount: 1, anyConnected: true, usedTerminal: false))
    }

    func testHiddenOnceAllDone() {
        XCTAssertFalse(OnboardingChecklist.shouldShow(hostCount: 1, anyConnected: true, usedTerminal: true))
    }
}
```

- [ ] **Step 2: Register the source in the test bundle**

In `auto2fa-mac/project.yml`, under `targets: → Auto2FATests: → sources:`, add after the `OTPSecret.swift` line:

```yaml
      - path: Auto2FA/OnboardingChecklist.swift
```

- [ ] **Step 3: Run the test to verify it fails**

```bash
cd /Users/shgao/logs/auto2fa_dev/auto2fa-mac && xcodegen generate && xcodebuild -project Auto2FA.xcodeproj -scheme Auto2FATests -destination 'platform=macOS' -only-testing:Auto2FATests/OnboardingChecklistTests test 2>&1 | tail -20
```
Expected: FAIL — `cannot find 'OnboardingChecklist' in scope`.

- [ ] **Step 4: Write the implementation**

Create `auto2fa-mac/Auto2FA/OnboardingChecklist.swift`:

```swift
import Foundation

/// The "Get started" steps a new user works through. Pure + Foundation-only so
/// it compiles into the headless test bundle (like SearchFilter / SlurmTime).
enum OnboardingStep: CaseIterable {
    case addHost       // registered at least one host
    case seeConnect    // a host reached the connected state
    case openTerminal  // used a host's Terminal action at least once
}

enum OnboardingChecklist {
    /// Which steps are complete given the live signals.
    static func completed(hostCount: Int, anyConnected: Bool, usedTerminal: Bool) -> Set<OnboardingStep> {
        var done: Set<OnboardingStep> = []
        if hostCount > 0 { done.insert(.addHost) }
        if anyConnected { done.insert(.seeConnect) }
        if usedTerminal { done.insert(.openTerminal) }
        return done
    }

    /// Show the checklist until every step is done (then the user knows the flow).
    static func shouldShow(hostCount: Int, anyConnected: Bool, usedTerminal: Bool) -> Bool {
        completed(hostCount: hostCount, anyConnected: anyConnected, usedTerminal: usedTerminal).count
            < OnboardingStep.allCases.count
    }
}
```

- [ ] **Step 5: Run the test to verify it passes**

```bash
cd /Users/shgao/logs/auto2fa_dev/auto2fa-mac && xcodebuild -project Auto2FA.xcodeproj -scheme Auto2FATests -destination 'platform=macOS' -only-testing:Auto2FATests/OnboardingChecklistTests test 2>&1 | tail -20
```
Expected: PASS (4 tests).

- [ ] **Step 6: Commit**

```bash
git add auto2fa-mac/Auto2FA/OnboardingChecklist.swift auto2fa-mac/Auto2FATests/OnboardingChecklistTests.swift auto2fa-mac/project.yml
git commit -m "feat(mac): OnboardingChecklist — pure get-started step + visibility logic"
```

---

### Task 2: "Used a terminal" signal + onboarding SettingsKeys

**Files:**
- Modify: `auto2fa-mac/Auto2FA/Settings.swift` (SettingsKey additions)
- Modify: `auto2fa-mac/Auto2FA/TerminalLauncher.swift`

- [ ] **Step 1: Add the SettingsKeys**

In `auto2fa-mac/Auto2FA/Settings.swift`, in the `SettingsKey` enum (next to `terminalApp`), add:

```swift
    /// Set the first time a host's "Open Terminal" actually launches — drives the
    /// onboarding checklist's "open a terminal" step.
    static let usedTerminal = "auto2fa.usedTerminal"
    /// User dismissed the Get-Started checklist — hide it for good.
    static let onboardingDismissed = "auto2fa.onboardingDismissed"
```

- [ ] **Step 2: Set the flag when a terminal launches**

In `auto2fa-mac/Auto2FA/TerminalLauncher.swift`, in `launch(host:choice:controlPath:)`, set the flag right after the `.command` file opens successfully. Find the `NSLog("[SSH2FA] openSSH host=...` line inside the `do {}` and add immediately before it:

```swift
            UserDefaults.standard.set(true, forKey: SettingsKey.usedTerminal)
```

- [ ] **Step 3: Build the app target**

```bash
cd /Users/shgao/logs/auto2fa_dev/auto2fa-mac && xcodebuild -project Auto2FA.xcodeproj -scheme Auto2FA -destination 'platform=macOS' build 2>&1 | tail -20
```
Expected: **BUILD SUCCEEDED**.

- [ ] **Step 4: Commit**

```bash
git add auto2fa-mac/Auto2FA/Settings.swift auto2fa-mac/Auto2FA/TerminalLauncher.swift
git commit -m "feat(mac): record first terminal use for the onboarding checklist"
```

---

### Task 3: `GetStartedChecklist` view

**Files:**
- Create: `auto2fa-mac/Auto2FA/Views/GetStartedChecklist.swift`

- [ ] **Step 1: Create the view**

Create `auto2fa-mac/Auto2FA/Views/GetStartedChecklist.swift`:

```swift
import SwiftUI

/// In-app "Get started" guide. Two modes:
/// - `compact: false` — the full empty-state panel (icon + steps + the import /
///   add-host CTAs) shown when there are no hosts yet.
/// - `compact: true` — a slim card shown ABOVE the host list while onboarding is
///   still incomplete, with a dismiss button.
struct GetStartedChecklist: View {
    @EnvironmentObject var appState: AppState
    let compact: Bool
    /// Called when the user dismisses the compact card.
    var onDismiss: () -> Void = {}

    private var done: Set<OnboardingStep> {
        OnboardingChecklist.completed(
            hostCount: appState.hosts.count,
            anyConnected: appState.hosts.contains { $0.displayState == .connected },
            usedTerminal: UserDefaults.standard.bool(forKey: SettingsKey.usedTerminal))
    }

    var body: some View {
        if compact { compactCard } else { fullPanel }
    }

    // MARK: - Steps

    private func stepRow(_ step: OnboardingStep, _ text: String) -> some View {
        let isDone = done.contains(step)
        return HStack(spacing: Spacing.s) {
            Image(systemName: isDone ? "checkmark.circle.fill" : "circle")
                .foregroundStyle(isDone ? Color.green : Color.secondary)
            Text(text)
                .strikethrough(isDone, color: .secondary)
                .foregroundStyle(isDone ? .secondary : .primary)
        }
        .font(.callout)
    }

    private var steps: some View {
        VStack(alignment: .leading, spacing: Spacing.xs) {
            stepRow(.addHost, "Add your first host")
            stepRow(.seeConnect, "Watch it connect (stays warm in the background)")
            stepRow(.openTerminal, "Open a Terminal from its row — no 2FA code to type")
        }
    }

    // MARK: - Full empty-state panel

    private var fullPanel: some View {
        VStack(spacing: Spacing.m) {
            Image(systemName: "checklist")
                .font(.largeTitle)
                .foregroundStyle(.tint)
            Text("Get started")
                .font(.title3)
            steps
                .padding(Spacing.m)
                .groupedContent(cornerRadius: Radius.control)
            if !appState.importableHosts.isEmpty {
                Button { appState.presentImport() } label: {
                    Label("Found \(appState.importableHosts.count) host(s) in ~/.ssh/config — pick which to protect",
                          systemImage: "sparkles")
                }
                .buttonStyle(.glassProminent)
            }
            Button { appState.presentAddHost() } label: {
                Label("Add a host manually", systemImage: "plus")
            }
            .controlSize(.large)
            .buttonStyle(.borderedProminent)
            Text("On a SLURM cluster? You can also forward a local port to a compute node — see the Tunnels tab.")
                .font(.caption).foregroundStyle(.tertiary)
                .multilineTextAlignment(.center)
                .padding(.horizontal, 32)
        }
        .padding(Spacing.l)
    }

    // MARK: - Compact above-list card

    private var compactCard: some View {
        HStack(alignment: .top, spacing: Spacing.m) {
            VStack(alignment: .leading, spacing: Spacing.xs) {
                Text("Get started").font(.callout.weight(.semibold))
                steps
            }
            Spacer()
            Button { onDismiss() } label: {
                Image(systemName: "xmark.circle.fill").foregroundStyle(.secondary)
            }
            .buttonStyle(.borderless)
            .help("Dismiss")
        }
        .padding(Spacing.m)
        .groupedContent(cornerRadius: Radius.control)
    }
}
```

- [ ] **Step 2: Build the app target**

```bash
cd /Users/shgao/logs/auto2fa_dev/auto2fa-mac && xcodegen generate && xcodebuild -project Auto2FA.xcodeproj -scheme Auto2FA -destination 'platform=macOS' build 2>&1 | tail -20
```
Expected: **BUILD SUCCEEDED**. (If `Spacing`/`Radius`/`.groupedContent`/`.glassProminent` are unresolved, confirm the exact names against `HostsView.swift`, which uses all of them.)

- [ ] **Step 3: Commit**

```bash
git add auto2fa-mac/Auto2FA/Views/GetStartedChecklist.swift
git commit -m "feat(mac): GetStartedChecklist view (empty-state panel + slim above-list card)"
```

---

### Task 4: Wire the checklist into `HostsView`

**Files:**
- Modify: `auto2fa-mac/Auto2FA/Views/HostsView.swift`

- [ ] **Step 1: Add onboarding state + the checklist into the body**

In `auto2fa-mac/Auto2FA/Views/HostsView.swift`, add a dismissal flag near the top of the struct (after `@EnvironmentObject var appState`):

```swift
    @AppStorage(SettingsKey.onboardingDismissed) private var onboardingDismissed = false

    private var onboardingActive: Bool {
        !onboardingDismissed && OnboardingChecklist.shouldShow(
            hostCount: appState.hosts.count,
            anyConnected: appState.hosts.contains { $0.displayState == .connected },
            usedTerminal: UserDefaults.standard.bool(forKey: SettingsKey.usedTerminal))
    }
```

- [ ] **Step 2: Replace the empty state with the checklist + show the slim card above a non-empty list**

Replace the existing `body` content block:

```swift
    var body: some View {
        VStack(alignment: .leading, spacing: Spacing.s) {
            header
            if appState.hosts.isEmpty {
                emptyState
            } else if visibleHosts.isEmpty {
                noMatches
            } else {
                hostsList
            }
        }
        .padding(Spacing.m)
    }
```

with:

```swift
    var body: some View {
        VStack(alignment: .leading, spacing: Spacing.s) {
            header
            if appState.hosts.isEmpty {
                // No hosts yet → the checklist IS the empty state (carries the CTAs).
                GetStartedChecklist(compact: false)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                // Has hosts but onboarding not finished → slim guide above the list.
                if onboardingActive {
                    GetStartedChecklist(compact: true) { onboardingDismissed = true }
                }
                if visibleHosts.isEmpty {
                    noMatches
                } else {
                    hostsList
                }
            }
        }
        .padding(Spacing.m)
    }
```

- [ ] **Step 3: Remove the now-unused `emptyState` property**

Delete the entire `private var emptyState: some View { … }` block (its content — icon, "No SSH hosts yet", the import + add buttons — is now in `GetStartedChecklist.fullPanel`). Keep `noMatches`, `header`, `hostsList`, `visibleHosts`, `countPill`.

- [ ] **Step 4: Build the app target**

```bash
cd /Users/shgao/logs/auto2fa_dev/auto2fa-mac && xcodebuild -project Auto2FA.xcodeproj -scheme Auto2FA -destination 'platform=macOS' build 2>&1 | tail -20
```
Expected: **BUILD SUCCEEDED**. (If a "emptyState is unused" or "cannot find emptyState" error appears, ensure Step 3 fully removed the property and no other reference to it remains.)

- [ ] **Step 5: Manual QA**

1. With 0 hosts → the Get-Started panel shows (steps + import/add CTAs).
2. Add a host → panel becomes the slim card above the list; "Add your first host" is checked.
3. Once the host connects + you open a Terminal once → all 3 checked → the card disappears on next render.
4. The × on the slim card dismisses it permanently (survives relaunch).

- [ ] **Step 6: Commit**

```bash
git add auto2fa-mac/Auto2FA/Views/HostsView.swift
git commit -m "feat(mac): show the Get-Started checklist in the Hosts pane during onboarding"
```

---

### Task 5: Import-first `WelcomeSheet`

**Files:**
- Modify: `auto2fa-mac/Auto2FA/Views/WelcomeSheet.swift`

- [ ] **Step 1: Replace `content` and `footer` with an import-first layout**

In `auto2fa-mac/Auto2FA/Views/WelcomeSheet.swift`, replace the `content` computed property and the `footer` computed property (and the now-unused `row(icon:title:body:)` helper) with:

```swift
    private var content: some View {
        VStack(alignment: .leading, spacing: Spacing.m) {
            if !appState.importableHosts.isEmpty {
                VStack(alignment: .leading, spacing: Spacing.s) {
                    Label("Found \(appState.importableHosts.count) host(s) in your ~/.ssh/config",
                          systemImage: "sparkles")
                        .font(.callout.weight(.semibold))
                    Text("Pick which to protect — we pre-fill the alias, you just enter the password and 2FA secret (or scan the QR). We test-login before saving so a wrong code can't lock you out.")
                        .font(.callout).foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                    Button {
                        UserDefaults.standard.set(true, forKey: SettingsKey.welcomeShown)
                        dismiss()
                        DispatchQueue.main.asyncAfter(deadline: .now() + 0.25) {
                            appState.presentImport()
                        }
                    } label: {
                        Label("Pick hosts to protect →", systemImage: "square.and.arrow.down")
                            .frame(maxWidth: .infinity)
                    }
                    .controlSize(.large)
                    .buttonStyle(.borderedProminent)
                }
                .padding(Spacing.m)
                .groupedContent(cornerRadius: Radius.control)
            } else {
                Text("SSH2FA refers to each host by its `~/.ssh/config` alias. Add your first one and enter its password + 2FA secret — that's it.")
                    .font(.callout).foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(Spacing.m)
                    .groupedContent(cornerRadius: Radius.control)
            }
        }
        .padding(Spacing.xl)
    }

    private var footer: some View {
        HStack {
            Button("Skip for now") {
                UserDefaults.standard.set(true, forKey: SettingsKey.welcomeShown)
                dismiss()
            }
            Spacer()
            // Manual add: prominent only when there's no easier import path.
            // (.bordered/.borderedProminent are PrimitiveButtonStyles and can't
            // be type-erased into one ?: expression, so branch the whole button.)
            if appState.importableHosts.isEmpty {
                Button(action: addManually) {
                    Label("Add a host manually", systemImage: "plus")
                }
                .buttonStyle(.borderedProminent).controlSize(.large)
            } else {
                Button(action: addManually) {
                    Label("Add a host manually", systemImage: "plus")
                }
                .buttonStyle(.bordered).controlSize(.large)
            }
        }
        .padding(Spacing.xl)
    }

    private func addManually() {
        UserDefaults.standard.set(true, forKey: SettingsKey.welcomeShown)
        dismiss()
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.25) {
            appState.presentAddHost()
        }
    }
```

- [ ] **Step 2: Tighten the header copy (optional but recommended)**

In `header`, the third `Text` (the tertiary "Built for HPC…") can stay. Leave the header as-is — the value prop is already good. (No code change required in this step; it's a confirmation that the header is intentionally kept.)

- [ ] **Step 3: Build the app target**

```bash
cd /Users/shgao/logs/auto2fa_dev/auto2fa-mac && xcodebuild -project Auto2FA.xcodeproj -scheme Auto2FA -destination 'platform=macOS' build 2>&1 | tail -20
```
Expected: **BUILD SUCCEEDED**.

- [ ] **Step 4: Manual QA**

1. Fresh defaults (`defaults delete com.ssh2fa.app auto2fa.welcomeShown`), 0 hosts, with importable config hosts → welcome shows the **"Pick hosts to protect →"** prominent CTA; "Add a host manually" is the secondary button.
2. "Pick hosts to protect" closes the welcome and opens the importer.
3. With NO importable hosts → "Add a host manually" is the prominent action.
4. Skip persists (doesn't re-appear on relaunch).

- [ ] **Step 5: Commit**

```bash
git add auto2fa-mac/Auto2FA/Views/WelcomeSheet.swift
git commit -m "feat(mac): import-first welcome — lead with picking hosts from ~/.ssh/config"
```

---

### Task 6: First-connect notch copy (drop the unrendered backticks)

**Files:**
- Modify: `auto2fa-mac/Auto2FA/AppState.swift`

- [ ] **Step 1: Fix the notch description**

In `auto2fa-mac/Auto2FA/AppState.swift`, in `celebrateFirstConnectIfNeeded()`, replace the `notchPresenter.show(...)` description (the notch renders plain text — backticks show literally):

```swift
        notchPresenter.show(
            systemImage: "checkmark.seal.fill",
            title: "Connected!",
            description: "\(h.host) is live — open a Terminal from its row. No code to type.",
            tint: .green)
```

- [ ] **Step 2: Build the app target**

```bash
cd /Users/shgao/logs/auto2fa_dev/auto2fa-mac && xcodebuild -project Auto2FA.xcodeproj -scheme Auto2FA -destination 'platform=macOS' build 2>&1 | tail -20
```
Expected: **BUILD SUCCEEDED**.

- [ ] **Step 3: Commit**

```bash
git add auto2fa-mac/Auto2FA/AppState.swift
git commit -m "fix(mac): first-connect notch copy — plain text (backticks didn't render) + name the Terminal action"
```

---

### Task 7: Settings clarity pass

**Files:**
- Modify: `auto2fa-mac/Auto2FA/Settings.swift`

- [ ] **Step 1: Add a "How SSH2FA works" explainer at the top of General**

In `auto2fa-mac/Auto2FA/Settings.swift`, inside the General `Form`, add a new FIRST `Section` immediately after `Form {`:

```swift
                Section {
                    HStack(alignment: .top, spacing: Spacing.m) {
                        Image(systemName: "bolt.shield")
                            .font(.title2).foregroundStyle(.tint)
                        VStack(alignment: .leading, spacing: 4) {
                            Text("How SSH2FA works")
                                .font(.callout.weight(.semibold))
                            Text("It answers the password + 2FA prompt for you and keeps a warm connection to each host, so ssh, scp, and your editor connect instantly with no code to type. Your password and 2FA secret are stored in the macOS Keychain.")
                                .font(.caption).foregroundStyle(.secondary)
                                .fixedSize(horizontal: false, vertical: true)
                        }
                    }
                } header: { Text("Overview") }
```

- [ ] **Step 2: Fix the unrendered backticks in the Warm-reuse caption**

Replace the warm-reuse `Text(warmReuseEnabled ? … : …)` caption (it uses backticks, which SwiftUI `Text` shows literally) with plain prose:

```swift
                    Text(warmReuseEnabled
                         ? "On — running ssh <host> in your own Terminal reuses SSH2FA's warm connection (via one Include line added to ~/.ssh/config)."
                         : "Off — the app's “Open Terminal” already reuses the connection. Turning this on also makes ssh <host> in your own Terminal skip the 2FA prompt.")
                        .font(.caption).foregroundStyle(.secondary)
```

- [ ] **Step 3: De-jargon the Daemon section caption**

Replace the Daemon section's `Toggle` label + caption:

```swift
                Section {
                    Toggle("Start the background helper when this app launches", isOn: $spawnDaemonOnLaunch)
                    Text("SSH2FA uses a small background helper to keep your connections alive. Leave this on unless you run it yourself.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                } header: { Text("Background helper") }
```

- [ ] **Step 4: Scan for any remaining backticks in Settings captions and convert them**

```bash
cd /Users/shgao/logs/auto2fa_dev/auto2fa-mac && grep -n '`' Auto2FA/Settings.swift
```
For each remaining hit inside a user-facing `Text(...)`, rewrite the backticked term as plain text (e.g. `` `Include` `` → `Include`, `` `ssh <host>` `` → `ssh <host>`). Do NOT change Swift string interpolation or comments — only the literal backtick characters inside displayed strings.

- [ ] **Step 5: Build the app target**

```bash
cd /Users/shgao/logs/auto2fa_dev/auto2fa-mac && xcodebuild -project Auto2FA.xcodeproj -scheme Auto2FA -destination 'platform=macOS' build 2>&1 | tail -20
```
Expected: **BUILD SUCCEEDED**.

- [ ] **Step 6: Manual QA**

1. Open Settings → General. The **Overview / "How SSH2FA works"** card is at the top.
2. No caption shows a literal backtick character anywhere.
3. The Daemon section reads "Background helper" with plain-language copy.

- [ ] **Step 7: Commit**

```bash
git add auto2fa-mac/Auto2FA/Settings.swift
git commit -m "feat(mac): Settings clarity — How-it-works explainer, plain-language copy, render code terms (no literal backticks)"
```

---

### Task 8: Full suite + final smoke

**Files:** none (verification only)

- [ ] **Step 1: Full unit-test suite**

```bash
cd /Users/shgao/logs/auto2fa_dev/auto2fa-mac && xcodegen generate && xcodebuild -project Auto2FA.xcodeproj -scheme Auto2FATests -destination 'platform=macOS' test 2>&1 | tail -25
```
Expected: PASS — all suites green (incl. the new `OnboardingChecklistTests`).

- [ ] **Step 2: App build**

```bash
cd /Users/shgao/logs/auto2fa_dev/auto2fa-mac && xcodebuild -project Auto2FA.xcodeproj -scheme Auto2FA -destination 'platform=macOS' build 2>&1 | tail -20
```
Expected: **BUILD SUCCEEDED**.

- [ ] **Step 3: End-to-end smoke (manual)**

Fresh defaults → welcome leads with import → pick a host → enter creds → it connects → first-connect notch → Get-Started card ticks off → open Terminal (no 2FA) → card disappears → Settings reads clearly.

- [ ] **Step 4: Finish the branch**

Announce "I'm using the finishing-a-development-branch skill to complete this work." Then follow **superpowers:finishing-a-development-branch**.

---

## Notes for the implementer

- No daemon/Rust change — all SwiftUI + UserDefaults.
- Keep `OnboardingChecklist` Foundation-only (no SwiftUI import) so it stays in the headless test bundle.
- `GetStartedChecklist` reuses the exact design tokens `HostsView` already uses (`Spacing`, `Radius`, `.groupedContent`, `.glassProminent`); copy names from there if any are unresolved.
- The first-connect hint already existed — this plan only fixes its copy. Don't add a second hint surface.
