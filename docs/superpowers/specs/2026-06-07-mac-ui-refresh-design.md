# SSH2FA macOS App — Native-Minimal UI Refresh (Design)

**Date:** 2026-06-07
**Scope:** Visual/UX refresh of the existing menu-bar app's **window dashboard**, plus
backend wiring to the Rust daemon (the latter already done in `DaemonProcess.swift`).
Presentation-layer only — no IPC/protocol/logic changes.

## Goal

Modernize the look and information density of the Hosts/Tunnels dashboard in a
**native-minimal** style (feels like a first-party Apple utility), surface more of the
data the daemon already provides, and remove visual inconsistency — **without changing
any behavior, keyboard shortcuts, or functionality**.

## Decisions (from brainstorming)

- **Shell:** keep the window dashboard + menu-bar item structure. Visual refresh only
  (no move to MenuBarExtra-first).
- **Style:** native-minimal — system materials/backgrounds, standard SwiftUI controls,
  SF Symbols, Dynamic Type, light/dark. **Color is reserved for status** (green/yellow/
  red) and the system accent for primary actions; everything else is neutral.
- **Rows:** two-line dense rows (primary line + a secondary metadata line), replacing the
  crowded multi-column `Table`.

## Principles

- One spacing scale: 4 / 8 / 12 / 16. One row metric set. Standardize sheet widths.
- Progressive disclosure: default rows are clean; deep details stay in the details popover.
- Shared components for status so Hosts and Tunnels render identically.

## Components & files

### New: `SSH2FA/DesignTokens.swift`
A single source of truth (plain `enum` namespaces, no runtime cost):
- `Spacing`: `xs=4, s=8, m=12, l=16`.
- `StatusColor`: maps a semantic state → Color. `connected/alive → green`,
  `connecting/starting → yellow`, `failed/stale/portBusy → red`, `idle/stopped → secondary`.
- `RowMetric`: row vertical padding, min height, icon size, mono font for identifiers.
- Small view modifiers: `.sectionHeaderStyle()`, `.cardSurface()` (neutral material +
  corner radius), consistent `.dashboardRow()` padding.

### New: `SSH2FA/Views/Components/StatusDot.swift`
`StatusDot(state:)` — the single status glyph used everywhere:
- filled circle in the status color; **pulsing animation only for connecting/starting**
  (reuse the existing pulse from the current `PulsingDot`); `⚠`-style treatment for
  failed/stale/portBusy via SF Symbol `exclamationmark.triangle.fill` (red) when the state
  is an attention state, plain `circle.fill` otherwise. Replaces the two divergent
  implementations in HostsView and TunnelsView.

### New: `SSH2FA/Views/Components/StatusBadge.swift`
`StatusBadge(state:, text:)` — `StatusDot` + a friendly label in the status color, used in
both row types for the leading status cell. Keeps host/tunnel status presentation identical.

### New: `SSH2FA/Views/Components/HostRow.swift`
Custom two-line host row (replaces the HostsView `Table` row):
- **Line 1:** `StatusDot` · alias (mono, primary) · resolved hostname (secondary, truncates)
  · pool pips (`●●` filled = ready slots, hollow = not) · mount indicator
  (`externaldrive.connected` green if mounted) · trailing **hover actions** (play/stop,
  mount/eject, rotate, terminal) using the existing `BackendClient` calls.
- **Line 2 (only when non-empty / non-trivial):** friendly last message (secondary caption,
  tooltip = raw `lastMsg`).
- Change-highlight (reuse `ChangeHighlight`) on status change.

### New: `SSH2FA/Views/Components/TunnelRow.swift`
Custom two-line tunnel row (replaces the TunnelsView `Table` row):
- **Line 1:** `StatusDot` · name (mono, primary) + inline glyphs `⚡`(autoStart),
  `terminal`(postConnectCmd) · `:local→:remote` (secondary mono) · node (secondary,
  `(no node yet)` tertiary) · trailing **hover actions** (play/stop, pick node, open
  browser, copy URL, details, delete).
- **Line 2 (secondary metadata):** `aliveSince` (e.g. `alive 2h` / `last alive 5m`) ·
  `via <jump or Auto>` (clickable jump-host menu, preserves the existing pin behavior) ·
  `<n> fails` (shown in red/orange only when `failCount > 0`) · tags as small capsules.
- Change-highlight on status change.

### Modify: `SSH2FA/Views/HostsView.swift`
- Replace `Table` with a `List` of `HostRow` (or `LazyVStack` inside a `ScrollView` if List
  styling fights the design — prefer `List` with `.plain`/`.inset` style for native feel).
- Restyle the empty state with the shared placeholder treatment.
- **Preserve:** all four actions (play/stop, mount/eject, rotate, terminal), disabled-state
  logic, in-flight spinners, pool/mount indicators, last-message tooltip.

### Modify: `SSH2FA/Views/TunnelsView.swift`
- Replace `Table` with a `List(selection:)` of `TunnelRow` — **multi-select must be
  preserved** (the batch start/stop toolbar depends on it).
- Restyle filter bar + tag chips with tokens; restyle empty state.
- **Preserve ALL existing behavior:** the filter (name/node/jump/tag), tag chips,
  multi-select + batch start/stop, every per-row action (play/stop, pick node, open
  browser, copy URL, details, delete), the full right-click context menu (start/stop, pick
  node, use jump host submenu, open, copy, clone, rename, tags submenu, autostart toggle,
  delete), and **every keyboard shortcut** (Space, Return, Delete, ⌘C, ⌘O, ⌘D), the Via
  jump-host menu with host-readiness dots, autostart/post-connect glyphs, alive-since
  subtext, change highlight.

### Modify: `SSH2FA/ContentView.swift`
- Section headers (HOSTS/TUNNELS): unified `.sectionHeaderStyle()` with a live count
  (e.g. `HOSTS · 4`) and a subtle divider.
- Restyle the error banner and undo snackbar to consistent tokens (material, corner radius,
  spacing). Keep their behavior (dismiss, 8s auto-hide, undo).
- Keep the vertical hosts/tunnels split and the toolbar (Add Host / New Tunnel / Reset),
  refined to consistent SF Symbol + label usage.

### Modify: the sheets (light normalization pass)
`NewTunnelSheet`, `AddHostSheet`, `NodePickerSheet`, `TunnelDetailsPopover`,
`CustomNodeSheet`, `WelcomeSheet`:
- Normalize to a small set of widths (e.g. compact 440 / wide 720) and the shared spacing
  scale + section styling. **No functional change** — same fields, same flows, same
  validation. This is a consistency pass, not a redo.

## What does NOT change

Interaction model, keyboard shortcuts, command palette, sheet functionality/flows, the
menu-bar dropdown menu (at most a light token pass), AppState, models, BackendClient, and
all daemon wiring. No new daemon capabilities are required.

## Non-goals

- No MenuBarExtra-first restructuring.
- No new backend features / no new IPC methods.
- No brand color / cards-with-shadows aesthetic (explicitly chose native-minimal).
- Not adding `Table` column sorting/resizing back (rows replace the table by design).

## Validation

- `xcodebuild -project SSH2FA.xcodeproj -scheme SSH2FA -configuration Debug build` must
  succeed after each task.
- Because changes are presentation-only, the Rust workspace tests are unaffected and the
  live daemon is untouched.
- Final visual confirmation is a user step (launch the rebuilt app).

## Risks

- **Functionality regression during Table→List refactor** is the main risk (selection,
  context menus, keyboard shortcuts). Mitigation: the plan ports behavior verbatim and each
  task lists the exact behaviors to preserve; reviewer checks against this spec.
