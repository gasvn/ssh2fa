# Liquid Glass Redesign Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn the SSH2FA macOS UI into a maximal, HIG-compliant Liquid Glass (macOS 26) experience by adopting native glass chrome + the full glass API, dropping all pre-26 fallbacks.

**Architecture:** Keep the two-pane (Hosts-over-Tunnels) layout and the correct content/chrome split. Pour "glass" into the chrome and interactions: translucent window base, a native glass `.toolbar`, morphing glass row-action bars, real glass on the command palette, and system-auto-glass sheets. Content rows stay opaque for legibility. One pure search helper is the only unit-tested logic; everything else is presentation, gated by a successful macOS-26-SDK build + a manual visual QA checklist.

**Tech Stack:** SwiftUI (macOS 26 SDK / Xcode 26.5), AppKit (`NSVisualEffectView`), XcodeGen, `xcodebuild`, XCTest. Spec: `docs/superpowers/specs/2026-06-12-liquid-glass-redesign-design.md`.

**Branch:** `liquid-glass-redesign` (already created; the spec is already committed there).

**Build/test commands (used by every task):**
```bash
cd /Users/shgao/logs/auto2fa_dev/auto2fa-mac
xcodegen generate
xcodebuild -project Auto2FA.xcodeproj -scheme Auto2FA -destination 'platform=macOS' build 2>&1 | tail -25
# tests:
xcodebuild -project Auto2FA.xcodeproj -scheme Auto2FATests -destination 'platform=macOS' test 2>&1 | tail -25
```

**Note on TDD:** Only Task 2 (`SearchFilter`) is unit-testable logic and follows strict TDD. The other tasks are SwiftUI presentation; their verification is **(a)** the build succeeds against the macOS 26 SDK (catches glass-API misuse) and **(b)** the manual QA item is checked. This is the honest test for view code — do not fabricate unit tests for view modifiers.

---

## File Structure

| File | Responsibility | Task |
|------|----------------|------|
| `auto2fa-mac/project.yml` | deployment target → 26; add `Auto2FA` scheme; add `SearchFilter.swift` to test target | 1, 2 |
| `Auto2FA/DesignTokens.swift` | de-fallback `glassCard`/`glassChrome`; add `VisualEffectBackground`, `windowGlassBackground`, `interactiveGlass` | 1 |
| `Auto2FA/SearchFilter.swift` (new) | pure, Foundation-only text matcher | 2 |
| `Auto2FATests/SearchFilterTests.swift` (new) | unit tests for the matcher | 2 |
| `Auto2FA/AppState.swift` | `@Published var searchQuery` shared by toolbar + panes | 3 |
| `Auto2FA/ContentView.swift` + `Auto2FAApp.swift` | translucent window base, native glass `.toolbar`, `.unified` style | 4 |
| `Auto2FA/Views/TunnelsView.swift` + `Views/HostsView.swift` | fold filter-bar text into global search; filter hosts | 5 |
| `Auto2FA/Views/Components/HostRow.swift` | morphing glass action bar | 6 |
| `Auto2FA/Views/Components/TunnelRow.swift` | morphing glass action bar | 7 |
| `Auto2FA/Views/{AddHostSheet,NewTunnelSheet,CustomNodeSheet,NodePickerSheet,WelcomeSheet,TunnelDetailsPopover}.swift` | de-glass-on-glass + drop `.bar` chrome | 8 |
| `Auto2FA/Views/CommandPalette.swift` | real glass + tinted-glass selection | 9 |
| `Auto2FA/Settings.swift` | verify native form glass; remove fighting backgrounds | 10 |

---

## Task 1: Deployment target → 26 + token layer

**Files:**
- Modify: `auto2fa-mac/project.yml` (deploymentTarget; add `Auto2FA` scheme)
- Modify: `auto2fa-mac/Auto2FA/DesignTokens.swift:176-235`

- [ ] **Step 1: Bump deployment target + add an app scheme**

In `auto2fa-mac/project.yml`, change:
```yaml
  deploymentTarget:
    macOS: "14.0"
```
to:
```yaml
  deploymentTarget:
    macOS: "26.0"
```
And add an `Auto2FA` scheme so `-scheme Auto2FA` is reliable. Under the existing `schemes:` block (which currently only has `Auto2FATests:`), add:
```yaml
  Auto2FA:
    build:
      targets:
        Auto2FA: [run, build, archive]
    run:
      config: Debug
```

- [ ] **Step 2: Replace `glassCard` / `glassChrome` with unconditional glass + add new helpers**

In `DesignTokens.swift`, replace the entire `// MARK: Liquid Glass surfaces (macOS 26) with material fallback (14.0+)` section (currently lines 176-222, the `glassCard`, `groupedContent`, and `glassChrome` definitions) with this. **Keep `groupedContent` exactly as it is** — it is the opaque content layer:

```swift
    // MARK: Liquid Glass surfaces (macOS 26 — no fallback; app is 26-only)

    /// Primary elevated FLOATING surface — cards / snackbars / banners that hover
    /// above content. Real Liquid Glass.
    func glassCard(cornerRadius: CGFloat = Radius.card) -> some View {
        self.glassEffect(.regular, in: .rect(cornerRadius: cornerRadius, style: .continuous))
    }

    /// Quiet, OPAQUE grouped content surface for list sections — the BASE
    /// layer. Continuous rounded corners + a hairline border, NO blur / NO
    /// glass. This is what content (hosts/tunnels lists) sits in; glass is
    /// reserved for floating chrome above content.
    func groupedContent(cornerRadius: CGFloat = Radius.card) -> some View {
        self
            .background(
                Color(nsColor: .controlBackgroundColor),
                in: RoundedRectangle(cornerRadius: cornerRadius, style: .continuous)
            )
            .overlay(
                RoundedRectangle(cornerRadius: cornerRadius, style: .continuous)
                    .strokeBorder(Color(nsColor: .separatorColor).opacity(0.5), lineWidth: 1)
            )
    }

    /// Lighter glass for floating chrome — bars / palettes.
    func glassChrome(cornerRadius: CGFloat = Radius.control) -> some View {
        self.glassEffect(.regular, in: .rect(cornerRadius: cornerRadius, style: .continuous))
    }

    /// One interactive, semantically-tinted glass surface for hero controls.
    func interactiveGlass(tint: Color? = nil, cornerRadius: CGFloat = Radius.control) -> some View {
        let glass: Glass = tint.map { .regular.tint($0).interactive() } ?? .regular.interactive()
        return self.glassEffect(glass, in: .rect(cornerRadius: cornerRadius, style: .continuous))
    }

    /// Translucent window base — desktop ambient shows through the margins while
    /// content groups stay opaque, so the window "floats" (Liquid Glass look).
    func windowGlassBackground() -> some View {
        self.background(
            VisualEffectBackground(material: .underWindowBackground,
                                   blending: .behindWindow)
                .ignoresSafeArea()
        )
    }
```

(`glassChrome` gains a `cornerRadius` param with the same default it had before, so the single existing caller `ContentView.swift:78` keeps compiling unchanged.)

- [ ] **Step 3: Add the `VisualEffectBackground` representable + `AppKit` import**

At the top of `DesignTokens.swift`, change `import SwiftUI` to:
```swift
import SwiftUI
import AppKit
```
At the **end** of `DesignTokens.swift` (after the `extension View { … }` block), add:
```swift
// MARK: - Translucent material backing (AppKit bridge)

/// Thin wrapper over `NSVisualEffectView` so a SwiftUI view can sit on a
/// real desktop-sampling translucent material — the basis of the floating
/// window look. SwiftUI has no first-class equivalent on macOS.
struct VisualEffectBackground: NSViewRepresentable {
    let material: NSVisualEffectView.Material
    let blending: NSVisualEffectView.BlendingMode

    func makeNSView(context: Context) -> NSVisualEffectView {
        let v = NSVisualEffectView()
        v.material = material
        v.blendingMode = blending
        v.state = .active
        return v
    }

    func updateNSView(_ v: NSVisualEffectView, context: Context) {
        v.material = material
        v.blendingMode = blending
    }
}
```

- [ ] **Step 4: Build**

Run the build command. Expected: **BUILD SUCCEEDED**. (If `Glass`/`glassEffect` symbols are unresolved, the SDK/deployment-target bump didn't take — re-check Step 1 and that `xcodegen generate` ran.)

- [ ] **Step 5: Commit**

```bash
git add auto2fa-mac/project.yml auto2fa-mac/Auto2FA/DesignTokens.swift
git commit -m "feat(ui): 26-only glass tokens — drop fallbacks, add window material + interactive glass"
```

---

## Task 2: Pure search-filter helper (TDD)

**Files:**
- Create: `auto2fa-mac/Auto2FA/SearchFilter.swift`
- Create: `auto2fa-mac/Auto2FATests/SearchFilterTests.swift`
- Modify: `auto2fa-mac/project.yml` (add `SearchFilter.swift` to the test target)

- [ ] **Step 1: Add the helper to the test target sources**

In `project.yml`, under `targets: → Auto2FATests: → sources:`, add the new file so it compiles headlessly into the test bundle (same pattern as `SyncCore.swift`):
```yaml
    sources:
      - path: Auto2FATests
      - path: Auto2FA/SyncCore.swift
      - path: Auto2FA/SearchFilter.swift
```

- [ ] **Step 2: Write the failing test**

Create `auto2fa-mac/Auto2FATests/SearchFilterTests.swift`:
```swift
import XCTest

final class SearchFilterTests: XCTestCase {
    func testBlankQueryMatchesEverything() {
        XCTAssertTrue(SearchFilter.matches(query: "", in: ["kempner"]))
        XCTAssertTrue(SearchFilter.matches(query: "   ", in: [nil]))
    }

    func testCaseInsensitiveSubstring() {
        XCTAssertTrue(SearchFilter.matches(query: "KEMP", in: ["kempner-login"]))
        XCTAssertTrue(SearchFilter.matches(query: "node01", in: [nil, "Node01", nil]))
    }

    func testNoMatchReturnsFalse() {
        XCTAssertFalse(SearchFilter.matches(query: "zzz", in: ["kempner", "txgent", nil]))
    }

    func testNilFieldsAreSkipped() {
        XCTAssertFalse(SearchFilter.matches(query: "x", in: [nil, nil]))
    }
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run the test command. Expected: **compile failure** — `cannot find 'SearchFilter' in scope` (the type doesn't exist yet).

- [ ] **Step 4: Write the minimal implementation**

Create `auto2fa-mac/Auto2FA/SearchFilter.swift`:
```swift
import Foundation

/// Pure, view-agnostic text matching for the global search field. Lives apart
/// from any SwiftUI / model type so it can be unit-tested headlessly (compiled
/// directly into the test bundle, like SyncCore). Used by HostsView, TunnelsView.
enum SearchFilter {
    /// True if `query` is blank (after trimming), or any non-nil field contains
    /// it (case-insensitive). nil fields are skipped.
    static func matches(query: String, in fields: [String?]) -> Bool {
        let q = query.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        if q.isEmpty { return true }
        return fields.contains { ($0 ?? "").lowercased().contains(q) }
    }
}
```

- [ ] **Step 5: Run the test to verify it passes**

Run `xcodegen generate` (to pick up the new test source) then the test command. Expected: **TEST SUCCEEDED** — `SearchFilterTests` 4/4 pass, and existing `SyncCoreTests` still pass.

- [ ] **Step 6: Commit**

```bash
git add auto2fa-mac/project.yml auto2fa-mac/Auto2FA/SearchFilter.swift auto2fa-mac/Auto2FATests/SearchFilterTests.swift
git commit -m "feat(ui): pure SearchFilter matcher + unit tests"
```

---

## Task 3: Shared search query on AppState

**Files:**
- Modify: `auto2fa-mac/Auto2FA/AppState.swift` (add one `@Published` property)

- [ ] **Step 1: Add the published property**

Find the AppState class declaration:
```bash
grep -n "class AppState" auto2fa-mac/Auto2FA/AppState.swift
grep -n "@Published var connectionError" auto2fa-mac/Auto2FA/AppState.swift
```
Immediately after the `connectionError` published property (a sibling view-state property), add:
```swift
    /// Global search text driven by the toolbar field; read by HostsView and
    /// TunnelsView to filter their lists. Empty = show everything.
    @Published var searchQuery: String = ""
```

- [ ] **Step 2: Build**

Run the build command. Expected: **BUILD SUCCEEDED** (nothing reads it yet; this just adds state).

- [ ] **Step 3: Commit**

```bash
git add auto2fa-mac/Auto2FA/AppState.swift
git commit -m "feat(ui): AppState.searchQuery for the global toolbar search"
```

---

## Task 4: Window chrome — translucent base + native glass toolbar

**Files:**
- Modify: `auto2fa-mac/Auto2FA/ContentView.swift:46-64` (mainStack + windowBackground)
- Modify: `auto2fa-mac/Auto2FA/Auto2FAApp.swift:70` (toolbar style)

- [ ] **Step 1: Make the window base translucent + extend content + add the toolbar**

In `ContentView.swift`, replace `mainStack` and the `windowBackground` computed property (lines 46-64) with:
```swift
    private var mainStack: some View {
        VStack(spacing: Spacing.l) {
            HostsView().frame(minHeight: 100)
            TunnelsView().frame(minHeight: 200)
        }
        .padding(Spacing.l)
        .frame(minWidth: 700, minHeight: 400)
        // Translucent floating window base; the lists keep their own opaque
        // grouped surfaces so text stays fully legible (Liquid Glass red line).
        .windowGlassBackground()
        .toolbar { mainToolbar }
    }

    @ToolbarContentBuilder
    private var mainToolbar: some ToolbarContent {
        ToolbarItem(placement: .principal) {
            HStack(spacing: Spacing.xs) {
                Image(systemName: "magnifyingglass").foregroundStyle(.secondary)
                TextField("Search hosts & tunnels", text: $appState.searchQuery)
                    .textFieldStyle(.plain)
                    .frame(minWidth: 180)
            }
        }
        ToolbarItemGroup(placement: .primaryAction) {
            Button { appState.presentAddHost() } label: {
                Label("Add Host", systemImage: "server.rack")
            }
            .buttonStyle(.glass)

            Button { appState.presentNewTunnel() } label: {
                Label("New Tunnel", systemImage: "plus")
            }
            .buttonStyle(.glassProminent)

            Menu {
                Button("Settings…") {
                    NSApp.sendAction(Selector(("showSettingsWindow:")), to: nil, from: nil)
                }
                Button("Show Logs…") { openWindow(id: "logs") }
                Divider()
                Button("Export Tunnels…") {
                    if let err = TunnelExportImport.exportToFile(appState.tunnels),
                       err != "cancelled" {
                        appState.showActionError("Export failed: \(err)")
                    }
                }
                Button("Import Tunnels…") {
                    let (imported, err) = TunnelExportImport.importFromFile()
                    if let imported, !imported.isEmpty {
                        Task { _ = await appState.importTunnels(imported) }
                    } else if let err, err != "cancelled" {
                        appState.showActionError(err)
                    }
                }
            } label: {
                Label("More", systemImage: "ellipsis.circle")
            }
        }
    }
```
Notes: `ContentView` already has `@Environment(\.openWindow) private var openWindow` (line 8) and `@EnvironmentObject var appState`. Do **not** add `.keyboardShortcut("n")` to the New Tunnel button — `Auto2FAApp.swift:73-77` already maps ⌘N → `presentNewTunnel()`; a second binding would conflict.

- [ ] **Step 2: Widen the toolbar style**

In `Auto2FAApp.swift:70`, change:
```swift
        .windowToolbarStyle(.unifiedCompact)
```
to:
```swift
        .windowToolbarStyle(.unified)
```

- [ ] **Step 3: Build**

Run the build command. Expected: **BUILD SUCCEEDED**.
- If `.buttonStyle(.glass)` / `.glassProminent` are unresolved, the SDK isn't 26 — re-check Task 1.
- The previous opaque `windowBackground` property is now deleted; confirm no other reference to it remains: `grep -n windowBackground auto2fa-mac/Auto2FA/ContentView.swift` should return nothing.

- [ ] **Step 4: Commit**

```bash
git add auto2fa-mac/Auto2FA/ContentView.swift auto2fa-mac/Auto2FA/Auto2FAApp.swift
git commit -m "feat(ui): translucent window base + native glass toolbar (search, add, overflow)"
```

---

## Task 5: Fold the tunnel filter bar text into the global search

**Files:**
- Modify: `auto2fa-mac/Auto2FA/Views/TunnelsView.swift` (`filter` state, `visibleTunnels`, `filterBar`, body)
- Modify: `auto2fa-mac/Auto2FA/Views/HostsView.swift` (`visibleHosts`)

- [ ] **Step 1: TunnelsView — drop local `filter`, use the global search**

In `TunnelsView.swift`, delete the local state line (line 8):
```swift
    @State private var filter: String = ""
```
Replace `visibleTunnels` (lines 18-30) with:
```swift
    /// Tunnels passing both the global search text and the tag filter.
    private var visibleTunnels: [Tunnel] {
        appState.tunnels.filter { t in
            if let tag = activeTagFilter, !t.tags.contains(tag) { return false }
            return SearchFilter.matches(
                query: appState.searchQuery,
                in: [t.name, t.lastNode, t.activeJump] + t.tags.map { Optional($0) }
            )
        }
    }
```

- [ ] **Step 2: TunnelsView — slim the filter bar (remove its text field, keep batch + tags)**

Replace `filterBar` (lines 122-178) with:
```swift
    private var filterBar: some View {
        VStack(spacing: Spacing.s) {
            if !selection.isEmpty {
                HStack(spacing: Spacing.s) {
                    Text("\(selection.count) selected")
                        .font(.caption).foregroundStyle(.secondary)
                    Button {
                        Task { await appState.batchTunnels(action: "start", names: Array(selection)) }
                    } label: { Label("Start", systemImage: "play.fill") }
                        .controlSize(.small)
                        .disabled(appState.batchInFlight)
                    Button {
                        Task { await appState.batchTunnels(action: "stop", names: Array(selection)) }
                    } label: { Label("Stop", systemImage: "stop.fill") }
                        .controlSize(.small)
                        .disabled(appState.batchInFlight)
                    Spacer()
                }
            }
            if !allTags.isEmpty {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: Spacing.xs + 2) {
                        tagChip("All", isActive: activeTagFilter == nil) {
                            activeTagFilter = nil
                        }
                        ForEach(allTags, id: \.self) { tag in
                            tagChip(tag, isActive: activeTagFilter == tag) {
                                activeTagFilter = (activeTagFilter == tag) ? nil : tag
                            }
                        }
                    }
                    .padding(.horizontal, Spacing.s)
                }
            }
        }
        .padding(Spacing.s)
        .background(.bar, in: RoundedRectangle(cornerRadius: Radius.control, style: .continuous))
    }
```

- [ ] **Step 3: TunnelsView — only show the filter bar when it has content**

In the `body`, replace the non-empty branch (lines 37-42):
```swift
            } else {
                filterBar
                tunnelsList
                    .controlSize(compactRows ? .small : .regular)
                    .font(compactRows ? .caption : .body)
            }
```
with:
```swift
            } else {
                if !selection.isEmpty || !allTags.isEmpty {
                    filterBar
                }
                tunnelsList
                    .controlSize(compactRows ? .small : .regular)
                    .font(compactRows ? .caption : .body)
            }
```

- [ ] **Step 4: HostsView — filter hosts by the global search**

In `HostsView.swift`, add a computed property right after the `body` (before `// MARK: - Header`, around line 17):
```swift
    private var visibleHosts: [SSHHost] {
        appState.hosts.filter { SearchFilter.matches(query: appState.searchQuery, in: [$0.host]) }
    }
```
Then in `hostsList` (line 50), change:
```swift
            ForEach(appState.hosts) { host in
```
to:
```swift
            ForEach(visibleHosts) { host in
```

- [ ] **Step 5: Build**

Run the build command. Expected: **BUILD SUCCEEDED**. Confirm no leftover `filter` reference in TunnelsView: `grep -n "\bfilter\b" auto2fa-mac/Auto2FA/Views/TunnelsView.swift` should only show unrelated words (e.g. `activeTagFilter`, `filterBar`), not the deleted `$filter`/`filter =`.

- [ ] **Step 6: Commit**

```bash
git add auto2fa-mac/Auto2FA/Views/TunnelsView.swift auto2fa-mac/Auto2FA/Views/HostsView.swift
git commit -m "feat(ui): global toolbar search drives host + tunnel filtering"
```

---

## Task 6: Morphing glass action bar — HostRow

**Files:**
- Modify: `auto2fa-mac/Auto2FA/Views/Components/HostRow.swift` (`hovering` state, `actions`, hover animation)

- [ ] **Step 1: Add a namespace + drive hover with a bouncy morph**

In `HostRow.swift`, change the state line (line 16):
```swift
    @State private var hovering = false
```
to:
```swift
    @State private var hovering = false
    @Namespace private var actionGlassNS
```
In `body`, change the animation modifier (line 119) and the hover handler (line 126). Replace:
```swift
        .animation(.easeInOut(duration: 0.12), value: hovering)
```
with (delete that line) and change:
```swift
        .onHover { hovering = $0 }
```
to:
```swift
        .onHover { h in withAnimation(.bouncy(duration: 0.35)) { hovering = h } }
```

- [ ] **Step 2: Rebuild `actions` as a morphing glass cluster**

Replace the `actions` computed property (lines 150-202) with the same three buttons (identical actions / labels / disabled logic) wrapped in a `GlassEffectContainer`, each a glass pill that morphs via `glassEffectID`:
```swift
    private var actions: some View {
        GlassEffectContainer(spacing: Spacing.xs) {
            HStack(spacing: Spacing.xs) {
                glassActionButton(id: "toggle",
                                  disabled: appState.inFlightHosts.contains(host.host),
                                  help: host.active ? "Disconnect host" : "Connect host") {
                    Task { await appState.toggleHost(host) }
                } label: {
                    if isBusy {
                        HStack(spacing: Spacing.xs) {
                            ProgressView().controlSize(.small).scaleEffect(0.6)
                                .frame(width: 14, height: 14)
                            Text(host.active ? "Disconnect" : "Connect").font(.caption)
                        }
                    } else {
                        Label(host.active ? "Disconnect" : "Connect",
                              systemImage: host.active ? "stop.fill" : "play.fill")
                    }
                }

                glassActionButton(id: "mount",
                                  disabled: isBusy || (!host.isMasterReady && !host.isMounted),
                                  help: host.isMounted ? "Unmount filesystem" : "Mount filesystem") {
                    Task { await appState.toggleMount(host) }
                } label: {
                    Label(host.isMounted ? "Unmount" : "Mount",
                          systemImage: host.isMounted ? "eject.fill" : "externaldrive.badge.plus")
                }

                glassActionButton(id: "terminal",
                                  disabled: !host.isMasterReady,
                                  help: "Open Terminal") {
                    openTerminal(for: host)
                } label: {
                    Label("Terminal", systemImage: "terminal")
                }
            }
        }
    }

    /// One morphing glass pill in the hover action bar. `id` ties it to the row's
    /// GlassEffectContainer namespace so pills fluidly merge/split on hover.
    @ViewBuilder
    private func glassActionButton<L: View>(
        id: String,
        disabled: Bool,
        help: String,
        action: @escaping () -> Void,
        @ViewBuilder label: () -> L
    ) -> some View {
        Button(action: action, label: label)
            .buttonStyle(.plain)
            .font(.caption)
            .padding(.horizontal, 8)
            .frame(height: 22)
            .foregroundStyle(disabled ? AnyShapeStyle(.tertiary) : AnyShapeStyle(.primary))
            .glassEffect(.regular.interactive(), in: .capsule)
            .glassEffectID(id, in: actionGlassNS)
            .disabled(disabled)
            .help(help)
            .accessibilityLabel(help)
    }
```

- [ ] **Step 3: Build**

Run the build command. Expected: **BUILD SUCCEEDED**.
- Fallback if `glassEffectID` causes a build or runtime issue with `.plain` buttons: remove the `.glassEffect(...).glassEffectID(...)` pair from `glassActionButton` and instead use `.buttonStyle(.glass)` on each button inside the `GlassEffectContainer`. You still get glass pills that appear/merge via the container; only the per-pill morph id is dropped. Note this fallback in the commit if used.

- [ ] **Step 4: Commit**

```bash
git add auto2fa-mac/Auto2FA/Views/Components/HostRow.swift
git commit -m "feat(ui): HostRow hover actions become a morphing glass bar"
```

---

## Task 7: Morphing glass action bar — TunnelRow

**Files:**
- Modify: `auto2fa-mac/Auto2FA/Views/Components/TunnelRow.swift` (`hovering` state, `actions`, hover animation)

- [ ] **Step 1: Add a namespace + bouncy hover**

In `TunnelRow.swift`, change the state line (line 22):
```swift
    @State private var hovering = false
```
to:
```swift
    @State private var hovering = false
    @Namespace private var actionGlassNS
```
In `body`, delete the animation line (line 137):
```swift
        .animation(.easeInOut(duration: 0.12), value: hovering)
```
and change the hover handler (line 146):
```swift
        .onHover { hovering = $0 }
```
to:
```swift
        .onHover { h in withAnimation(.bouncy(duration: 0.35)) { hovering = h } }
```

- [ ] **Step 2: Rebuild `actions` as a morphing glass cluster**

Replace the `actions` computed property (lines 216-270) with (same four buttons, identical actions / labels / disabled logic):
```swift
    private var actions: some View {
        GlassEffectContainer(spacing: Spacing.xs) {
            HStack(spacing: Spacing.xs) {
                glassActionButton(id: "toggle",
                                  disabled: appState.inFlightTunnels.contains(tunnel.name),
                                  help: tunnelIsOn ? "Stop tunnel" : "Start tunnel") {
                    Task { await appState.toggleTunnel(tunnel) }
                } label: {
                    if isBusy {
                        HStack(spacing: Spacing.xs) {
                            ProgressView().controlSize(.small).scaleEffect(0.6)
                                .frame(width: 14, height: 14)
                            Text(tunnelIsOn ? "Stop" : "Start").font(.caption)
                        }
                    } else {
                        Label(tunnelIsOn ? "Stop" : "Start",
                              systemImage: tunnelIsOn ? "stop.fill" : "play.fill")
                    }
                }

                glassActionButton(id: "node",
                                  disabled: isBusy,
                                  help: "Pick compute node") {
                    appState.presentNodePicker(for: tunnel)
                } label: {
                    Label("Node", systemImage: "list.bullet.rectangle")
                }

                glassActionButton(id: "open",
                                  disabled: isBusy || tunnel.displayState != .alive,
                                  help: "Open in browser") {
                    openInBrowser(tunnel)
                } label: {
                    Label("Open", systemImage: "safari")
                }

                glassActionButton(id: "copy",
                                  disabled: false,
                                  help: "Copy localhost URL") {
                    copyURL(tunnel.url)
                } label: {
                    Label("Copy", systemImage: "doc.on.doc")
                }
            }
        }
    }

    /// One morphing glass pill in the hover action bar (mirrors HostRow).
    @ViewBuilder
    private func glassActionButton<L: View>(
        id: String,
        disabled: Bool,
        help: String,
        action: @escaping () -> Void,
        @ViewBuilder label: () -> L
    ) -> some View {
        Button(action: action, label: label)
            .buttonStyle(.plain)
            .font(.caption)
            .padding(.horizontal, 8)
            .frame(height: 22)
            .foregroundStyle(disabled ? AnyShapeStyle(.tertiary) : AnyShapeStyle(.primary))
            .glassEffect(.regular.interactive(), in: .capsule)
            .glassEffectID(id, in: actionGlassNS)
            .disabled(disabled)
            .help(help)
            .accessibilityLabel(help)
    }
```

- [ ] **Step 3: Build**

Run the build command. Expected: **BUILD SUCCEEDED**. (Same `glassEffectID` fallback as Task 6 Step 3 applies if needed.)

- [ ] **Step 4: Commit**

```bash
git add auto2fa-mac/Auto2FA/Views/Components/TunnelRow.swift
git commit -m "feat(ui): TunnelRow hover actions become a morphing glass bar"
```

---

## Task 8: De-glass-on-glass in sheets + drop `.bar` chrome

Once a sheet's background is system Liquid Glass (automatic on macOS 26), an inner `glassCard` would be glass-on-glass (HIG violation). Swap those inner panels to the opaque `groupedContent`, and drop the `.bar` header/footer fills so the system sheet glass shows.

**Files:**
- Modify: `Views/AddHostSheet.swift:87, 147, 242, 308`
- Modify: `Views/NewTunnelSheet.swift:121`
- Modify: `Views/CustomNodeSheet.swift:40`
- Modify: `Views/NodePickerSheet.swift:70`
- Modify: `Views/WelcomeSheet.swift:66, 107`
- Modify: `Views/TunnelDetailsPopover.swift:103`

- [ ] **Step 1: Swap in-sheet `glassCard` → `groupedContent`**

In each of these files, change the line `.glassCard(cornerRadius: Radius.control)` to `.groupedContent(cornerRadius: Radius.control)`:
- `AddHostSheet.swift:147` and `AddHostSheet.swift:242`
- `NewTunnelSheet.swift:121`
- `CustomNodeSheet.swift:40`
- `NodePickerSheet.swift:70`
- `WelcomeSheet.swift:66`
- `TunnelDetailsPopover.swift:103`

(Do **not** touch `ContentView.swift:112` or `:78` — those are the floating undo snackbar / error banner, which are correct chrome glass and stay.)

- [ ] **Step 2: Drop the `.bar` header/footer fills**

Delete these `.background(.bar)` modifier lines so the system sheet glass shows through the header/footer:
- `AddHostSheet.swift:87` (header) — remove `.background(.bar)`
- `AddHostSheet.swift:308` (footer) — remove `.background(.bar)`
- `WelcomeSheet.swift:107` (footer) — remove `.background(.bar)`

(Leave `TunnelsView.swift:177` — that `.bar` is the in-pane filter bar over content, not a sheet, and is intentional. Leave `LogViewerView.swift:42` — out of scope.)

- [ ] **Step 3: Build**

Run the build command. Expected: **BUILD SUCCEEDED**. Verify the swaps landed: `grep -rn "glassCard" auto2fa-mac/Auto2FA/Views` should now show **only** `ContentView.swift:112` (the snackbar) — no sheet files.

- [ ] **Step 4: Commit**

```bash
git add auto2fa-mac/Auto2FA/Views/AddHostSheet.swift auto2fa-mac/Auto2FA/Views/NewTunnelSheet.swift auto2fa-mac/Auto2FA/Views/CustomNodeSheet.swift auto2fa-mac/Auto2FA/Views/NodePickerSheet.swift auto2fa-mac/Auto2FA/Views/WelcomeSheet.swift auto2fa-mac/Auto2FA/Views/TunnelDetailsPopover.swift
git commit -m "fix(ui): sheets use system glass — inner panels opaque (no glass-on-glass), drop .bar"
```

---

## Task 9: CommandPalette — real glass + tinted-glass selection

**Files:**
- Modify: `auto2fa-mac/Auto2FA/Views/CommandPalette.swift:201, 235-237, 136`

- [ ] **Step 1: Replace the raw material with real glass chrome**

In `CommandPalette.swift`, change line 201:
```swift
        .background(.ultraThinMaterial)
```
to:
```swift
        .glassChrome(cornerRadius: Radius.card)
```

- [ ] **Step 2: Make the selected row a tinted glass chip**

In the `row(_:isSelected:)` builder, replace the selection background (line 237):
```swift
        .background(isSelected ? Color.accentColor : Color.clear)
```
with a conditional tinted glass (the `isEnabled:` flag turns it off for unselected rows so there is no glass-on-glass for the 99% un-selected rows):
```swift
        .glassEffect(.regular.tint(.accentColor).interactive(),
                     in: .rect(cornerRadius: Radius.control, style: .continuous),
                     isEnabled: isSelected)
```
Keep the existing `isSelected ? .white : …` foreground colors (lines 222, 227, 230) — they stay legible over the accent-tinted glass.

- [ ] **Step 3: Simplify the always-true availability check**

With the 26 deployment target, `#available(macOS 14, *)` at lines 136-140 is always true. Replace:
```swift
                if #available(macOS 14, *) {
                    NSApp.sendAction(Selector(("showSettingsWindow:")), to: nil, from: nil)
                } else {
                    NSApp.sendAction(Selector(("showPreferencesWindow:")), to: nil, from: nil)
                }
```
with:
```swift
                NSApp.sendAction(Selector(("showSettingsWindow:")), to: nil, from: nil)
```

- [ ] **Step 4: Build**

Run the build command. Expected: **BUILD SUCCEEDED**.

- [ ] **Step 5: Commit**

```bash
git add auto2fa-mac/Auto2FA/Views/CommandPalette.swift
git commit -m "feat(ui): CommandPalette on real glass with a tinted-glass selection chip"
```

---

## Task 10: Settings — verify native form glass

**Files:**
- Inspect/Modify: `auto2fa-mac/Auto2FA/Settings.swift`

- [ ] **Step 1: Check for opaque backgrounds fighting the native form**

`Settings.swift` already uses `.formStyle(.grouped)` (auto-adopts correct material on macOS 26). Confirm nothing overrides it:
```bash
grep -n "\.background(" auto2fa-mac/Auto2FA/Settings.swift
```
- If the grep shows an explicit opaque/material background applied to the `Form`, a `Section`, or the `TabView` root (e.g. `.background(Color(...))` / `.background(.bar)` on those containers), **remove that modifier** so the native grouped-form material shows.
- If the grep returns nothing, or only backgrounds on small inline elements (status pills, health badges), this task is a no-op — leave them.

- [ ] **Step 2: Build**

Run the build command. Expected: **BUILD SUCCEEDED**.

- [ ] **Step 3: Commit (only if Step 1 changed anything)**

```bash
git add auto2fa-mac/Auto2FA/Settings.swift
git commit -m "fix(ui): let Settings' grouped form adopt native material"
```
If Step 1 made no change, skip the commit and note "Settings already clean — no change."

---

## Task 11: Final build, full test run, manual QA

**Files:** none (verification only)

- [ ] **Step 1: Clean build + full test suite**

```bash
cd /Users/shgao/logs/auto2fa_dev/auto2fa-mac
xcodegen generate
xcodebuild -project Auto2FA.xcodeproj -scheme Auto2FA -destination 'platform=macOS' clean build 2>&1 | tail -20
xcodebuild -project Auto2FA.xcodeproj -scheme Auto2FATests -destination 'platform=macOS' test 2>&1 | tail -20
```
Expected: **BUILD SUCCEEDED** and **TEST SUCCEEDED** (`SearchFilterTests` + `SyncCoreTests` all pass).

- [ ] **Step 2: Manual visual QA checklist**

Launch the built app (or `open` the product) and verify:
- [ ] Window toolbar renders as floating Liquid Glass; search field filters both Hosts and Tunnels live.
- [ ] Window margins are translucent (desktop ambient visible) while host/tunnel **row text is fully legible** (opaque grouped surfaces).
- [ ] Hovering a host row and a tunnel row reveals a glass action bar that **morphs/bounces** in; actions still work (connect, mount, terminal / start, node, open, copy).
- [ ] New Tunnel / Add Host toolbar buttons open their sheets; sheets show system inset glass with **opaque inner panels** (no glass-on-glass), no leftover `.bar` strips.
- [ ] ⌘⇧P palette is glass; the selected row is a tinted glass chip and arrow-key navigation reads clearly.
- [ ] Toggle System Settings → Accessibility → **Reduce Transparency** ON: everything stays legible (system frosts the glass). Toggle OFF again.
- [ ] Light mode and Dark mode both look right.

- [ ] **Step 3: Finish the development branch**

Announce: "I'm using the finishing-a-development-branch skill to complete this work." Then follow **superpowers:finishing-a-development-branch** to verify tests, present merge options, and execute the choice.

---

## Self-Review notes (for the executor)

- **Spec coverage:** every spec section 4a–4h maps to a task (4a→T1, 4b→T1, 4c→T4, 4d→T5/T6/T7, 4e→T6/T7, 4f→T8, 4g→T9, 4h→T10; deployment-target sweep→T1+T9). The `glassActionBar` token helper named in the spec was intentionally **not** built — inline `GlassEffectContainer` per row (T6/T7) is simpler than threading a `@Namespace` through a helper (YAGNI).
- **Naming consistency:** `glassActionButton(id:disabled:help:action:label:)` and `actionGlassNS` are defined identically in both HostRow (T6) and TunnelRow (T7). `searchQuery` is the single shared name across AppState (T3), ContentView (T4), HostsView/TunnelsView (T5). `SearchFilter.matches(query:in:)` signature is identical in the helper (T2), its tests (T2), and both call sites (T5).
- **No placeholders:** every code step shows complete code; every verification step has an exact command + expected result.
