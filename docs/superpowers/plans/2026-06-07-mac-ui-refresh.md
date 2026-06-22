# Mac UI Refresh — Implementation Plan

> **For agentic workers:** Execute task-by-task. Each task ends with an `xcodebuild`
> compile gate (no unit tests — this is presentation-only SwiftUI). Preserve ALL behavior
> listed per task. Design spec: `docs/superpowers/specs/2026-06-07-mac-ui-refresh-design.md`.

**Goal:** Native-minimal visual refresh of the SSH2FA window dashboard (Hosts/Tunnels),
zero functionality loss.

**Build/verify (run from `auto2fa-mac/`):**
`xcodebuild -project SSH2FA.xcodeproj -scheme SSH2FA -configuration Debug -derivedDataPath build build`
→ must print `** BUILD SUCCEEDED **`.

**Tech:** SwiftUI, macOS 14+, SF Symbols, system materials. App dir:
`/Users/shgao/logs/auto2fa_dev/auto2fa-mac/SSH2FA`.

---

### Task UI-1: Design tokens + shared status components

**Files:**
- Create: `SSH2FA/DesignTokens.swift`
- Create: `SSH2FA/Views/Components/StatusDot.swift`
- Create: `SSH2FA/Views/Components/StatusBadge.swift`

**Do:**
- `DesignTokens.swift`: `enum Spacing { static let xs=4; s=8; m=12; l=16 }` (CGFloat);
  `enum StatusColor` with a `static func color(for state:) -> Color` keyed off the
  existing `SSHHost.DisplayState` and `Tunnel.DisplayState` (green/yellow/red/secondary as
  in the spec); `enum RowMetric` (vPad, minHeight, iconSize, `monoFont`); view modifiers
  `sectionHeaderStyle()`, `cardSurface()`, `dashboardRow()`.
- `StatusDot`: a `View` taking a unified status enum (or both DisplayStates via small
  initializers). Filled `circle.fill` in status color; pulse animation ONLY for
  connecting/starting (port the pulse from the current `PulsingDot` in HostsView/TunnelsView);
  use `exclamationmark.triangle.fill` (red) for failed/stale/portBusy.
- `StatusBadge`: `StatusDot` + label in status color.

**Preserve:** nothing replaced yet — these are additive. The existing `PulsingDot` stays
until UI-2/UI-3 swap to `StatusDot`.

**Gate:** xcodebuild succeeds.

---

### Task UI-2: HostRow + HostsView refactor

**Files:**
- Create: `SSH2FA/Views/Components/HostRow.swift`
- Modify: `SSH2FA/Views/HostsView.swift`

**Do:** Replace the `Table` with a `List` (`.plain` or `.inset`) of `HostRow`. `HostRow`
is the two-line layout from the spec (status badge · alias · hostname · pool pips · mount ·
hover actions; line 2 = friendly last message). Use `DesignTokens` + `StatusBadge`.
Restyle the empty state with a shared placeholder.

**PRESERVE (verbatim behavior):**
- All four row actions wired to the same `BackendClient`/`AppState` calls: play/stop
  (toggle active), mount/eject, rotate, open terminal.
- Disabled-state logic (busy / master-not-ready) and in-flight spinners
  (`inFlightHosts`).
- Pool count (`poolAlive`/2), mount indicator, last-message tooltip (raw `lastMsg`).
- Change-highlight on status change.
- Empty-state "Add your first SSH host" action.

**Gate:** xcodebuild succeeds.

---

### Task UI-3: TunnelRow + TunnelsView refactor (largest — preserve everything)

**Files:**
- Create: `SSH2FA/Views/Components/TunnelRow.swift`
- Modify: `SSH2FA/Views/TunnelsView.swift`

**Do:** Replace the `Table` with a `List(selection:)` of `TunnelRow`. **Multi-select must
work** (drives the batch toolbar). `TunnelRow` is the two-line layout from the spec
(status badge · name + ⚡/terminal glyphs · `:local→:remote` · node · hover actions; line 2
= `aliveSince · via <jump> · <n> fails` + tag capsules). Use `DesignTokens`/`StatusBadge`.
Restyle the filter bar + tag chips + empty state with tokens.

**PRESERVE (verbatim behavior — check each):**
- Filter field (name/node/jump/tag, case-insensitive) and tag chips quick-filter.
- Multi-selection + the batch Start/Stop toolbar that appears on selection.
- Every per-row action: play/stop, pick node, open browser (disabled if not alive),
  copy URL, details (opens `TunnelDetailsPopover`), delete (confirmation dialog).
- The FULL right-click context menu: Start/Stop, Pick node…, Use jump host → submenu
  (Auto + each host with readiness dots + checkmarks), Open in browser, Copy localhost:PORT,
  Clone…, Rename…, Tags → submenu (+ Clear all), Start on daemon launch toggle, Delete.
- EVERY keyboard shortcut: Space (toggle), Return (pick node), Delete (delete), ⌘C (copy
  URL), ⌘O (open browser), ⌘D (clone).
- The Via jump-host menu (pin behavior, host-readiness ● / ○).
- autostart (⚡) and post-connect (terminal) glyphs; alive-since subtext; change-highlight.

**Gate:** xcodebuild succeeds. After build, manually re-read the diff to confirm no action/
shortcut/menu item was dropped (compare against the PRESERVE list).

---

### Task UI-4: ContentView shell + sheet normalization

**Files:**
- Modify: `SSH2FA/ContentView.swift`
- Modify (light pass): `SSH2FA/Views/NewTunnelSheet.swift`, `AddHostSheet.swift`,
  `NodePickerSheet.swift`, `TunnelDetailsPopover.swift`, `CustomNodeSheet.swift`,
  `WelcomeSheet.swift`

**Do:**
- ContentView: unified section headers with live counts (`HOSTS · 4`, `TUNNELS · 2`) via
  `sectionHeaderStyle()`; restyle error banner + undo snackbar to tokens (material, radius,
  spacing) keeping behavior (dismiss, 8s auto-hide, undo); refine toolbar SF Symbol+label
  consistency.
- Sheets: normalize widths to a small set (compact 440 / wide 720) and apply the shared
  spacing scale + section styling. **No functional change** — identical fields/flows/
  validation.

**PRESERVE:** error-banner dismiss + tooltip, undo snackbar undo/auto-hide, all sheet
fields/flows/validation, toolbar actions (Add Host / New Tunnel / Reset), command-palette
and welcome-sheet triggers.

**Gate:** xcodebuild succeeds.

---

### Task UI-5: Final build, consistency sweep, commit

**Do:**
- Full `xcodebuild` Debug build → `** BUILD SUCCEEDED **`.
- Quick sweep: grep for the old `PulsingDot` usages — all should now route through
  `StatusDot` (remove the now-dead duplicate implementations if unused).
- Confirm no leftover `Table(` in HostsView/TunnelsView.
- Commit the UI refresh.
- Hand off to user for the visual check (launch the rebuilt app).

**Gate:** xcodebuild succeeds; commit made.
