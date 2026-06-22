# SSH2FA Liquid Glass Redesign — Design Spec

**Date:** 2026-06-12
**Status:** Approved direction, pending spec review
**Goal:** Transform the SSH2FA macOS UI into a maximal-but-HIG-compliant "Liquid Glass" (macOS 26 / Tahoe) experience by adopting native structural chrome and the full glass API, dropping all pre-26 fallbacks.

---

## 1. Context & current state

SSH2FA is a SwiftUI **menu-bar utility** with a main window (`WindowGroup("SSH2FA")`), a Settings scene, a Logs window, several modal sheets, and an AppKit status-bar item. Toolchain: **Xcode 26.5 / macOS 26.5 SDK, running macOS 26.2** — native `.glassEffect()` compiles and renders.

The codebase already has ~70% of a correct Liquid Glass foundation (audited 2026-06-12):

- `DesignTokens.swift` defines `glassCard()` / `glassChrome()` (real `.glassEffect(.regular, in:)` on 26 with a `≤15` material fallback) and `groupedContent()` (intentionally **opaque** content base layer), plus continuous radii, rounded fonts, `IconActionButton`/`IconTextActionButton`, `hoverLift`.
- Layering is already correct in principle: content opaque, glass reserved for floating chrome (`ContentView.swift:57-58` comment states this explicitly).

**Why it doesn't yet *look* like macOS 26 Liquid Glass:** the headline visual mechanism — the system auto-glassing **native `.toolbar` / sidebar / sheet chrome** — is unused. The main window is a hand-built `VStack` (`ContentView.swift:46-54`) with per-section headers in `HostsView`/`TunnelsView`; `Auto2FAApp.swift:70` sets `.windowToolbarStyle(.unifiedCompact)` but **no `.toolbar {}` is ever defined**. The window base is fully opaque (`ContentView.swift:61-62`). So there is no floating glass, no translucency, no morphing.

The lever is **structural adoption + the full glass API**, not material tweaks.

---

## 2. Design principles (the rules we follow)

From Apple's official guidance (HIG > Materials > Liquid Glass; "Meet Liquid Glass", WWDC25):

1. **Glass is for the navigation/control layer that floats above content.** Toolbars, floating action bars, popovers, palettes — yes. Lists/tables/text content — never glassed.
2. **No glass-on-glass.** A glass surface must not contain another glass surface.
3. **Use it sparingly** — accent, not wallpaper. Limit simultaneous glass layers.
4. **Tint is semantic** (primary action / state), not decoration.
5. **The one hard red line: content legibility.** List rows and text sit on opaque/near-opaque surfaces so the desktop never bleeds through behind text.
6. **Let the system handle accessibility** (Reduce Transparency / Increase Contrast / Reduce Motion) — never override it manually.

**Our interpretation of "最炫" (maximal):** pour every dazzle mechanism into the **chrome and interactions** — translucent window margins, native glass toolbar, morphing floating action bars, interactive glass, scroll-edge effects, concentric corners — while keeping content rows opaque and legible. This is maximal *and* compliant.

---

## 3. Scope

**In scope:** main window shell (window material + native toolbar + background extension), Hosts/Tunnels content surfaces, row floating action bars, all modal sheets (`AddHostSheet`, `NewTunnelSheet`, `NodePickerSheet`, `CustomNodeSheet`, `WelcomeSheet`, `TunnelDetailsPopover`), `CommandPalette`, `SettingsView`, the `DesignTokens.swift` token layer, and the deployment-target bump.

**Out of scope:** the AppKit menu-bar `NSMenu` (system auto-glasses native menus; custom value low), the notch (`NotchPresenter`/`PersistentNotchController` — styling owned by external `DynamicNotchKit`, pinned), `LogViewerView` beyond minimal token alignment (it is a monospaced text viewer; glass would hurt legibility), and the `BiometricLock`/`LockGate` screen.

---

## 4. Architecture changes

### 4a. Deployment target → macOS 26 only

- `project.yml`: `deploymentTarget.macOS: "14.0"` → `"26.0"`.
- **Consequence (explicit):** the app will no longer launch on macOS < 26. The user has accepted this ("不需要关心 mac os 14 的回退").
- Remove every `#available(macOS 26.0, *)` / `else` fallback branch across the codebase (sweep for `#available` and `if #available`). Token helpers become unconditional glass.

### 4b. Token layer redesign (`DesignTokens.swift`)

The token layer stays the single styling chokepoint. Changes:

- `glassCard()` / `glassChrome()` → drop the `else` material branch; call `.glassEffect(.regular, in:)` directly. Use `.rect(cornerRadius: .containerConcentric, style: .continuous)` where the surface should track its container's corner (window/sheet edges); keep explicit `Radius.*` for free-floating pills.
- **New `windowGlassBackground()`** — backs the main window in a translucent `NSVisualEffectView` material (`.underWindowBackground`) via a small `VisualEffectBackground: NSViewRepresentable`. This is what makes the window *margins* glassy (desktop ambient shows through) while content groups stay opaque.
- **New `glassActionBar { }`** — wraps a cluster of row quick-actions in a `GlassEffectContainer(spacing:)` so adjacent action pills share one sampling region and can morph. Includes a `@Namespace` plumbing helper for `glassEffectID`.
- **New `interactiveGlass(tint:)`** — `.glassEffect(.regular.tint(...).interactive())` for the few hero controls (primary toolbar action, palette).
- `groupedContent()` stays as-is (opaque content base) — it is the legibility guarantee.
- Remove `IconActionButton`/`IconTextActionButton`'s hand-rolled hover-rectangle **only where** they are replaced by glass pills (row action bars); keep them for dense inline icons that should stay flat.

### 4c. Window chrome (`Auto2FAApp.swift`, `ContentView.swift`)

- Replace `ContentView.windowBackground` (opaque `Color(nsColor:.windowBackgroundColor)`, lines 59-64) with `windowGlassBackground()` (translucent material). Content lists keep their opaque `groupedContent()` surfaces, so text legibility is preserved while the window floats.
- Add a real **`.toolbar { }`** to the main window content. Contents:
  - leading: app glyph + title region (or rely on titlebar).
  - center/principal: a **search field** implemented as a toolbar `TextField` (a `ToolbarItem`, **not** `.searchable` — there is no `NavigationStack`/`NavigationSplitView` to host it) that filters both hosts and tunnels by name. The existing in-pane tunnel filter bar's **text search folds into this global field**; any tunnel-specific filters there that are not plain text (e.g. tag/state chips) **remain in-pane** so no filtering capability is lost.
  - trailing: **New Tunnel** as the prominent primary action (`.buttonStyle(.glassProminent)` + semantic tint), **Add Host**, and an overflow `Menu` (Settings, Logs, Import/Export).
  - On macOS 26 these auto-render as floating Liquid Glass; symbols are prioritized automatically.
- `Auto2FAApp.swift:70`: `.windowToolbarStyle(.unifiedCompact)` → `.unified` (roomier glass toolbar). Keep `.defaultSize`.
- Apply `.backgroundExtensionEffect()` to the content scroll so it bleeds edge-to-edge under the toolbar (the "content floats under glass" look) and enable scroll-edge effects (default on; do **not** set `.scrollClipDisabled(true)`).
- Per-section headers in `HostsView`/`TunnelsView` slim to **title + count badge**; their add buttons are consolidated into the toolbar, but a small inline `+` is retained per section for glanceable affordance (same action target).

### 4d. Content layer (stays opaque)

- `HostsView` / `TunnelsView` lists keep `.groupedContent()`. No glass on rows themselves.
- `HostRow` / `TunnelRow` row bodies remain opaque content. Only their **transient hover action cluster** becomes glass (4e).
- Micro-indicators (`StatusBadge`, `StatusDot`, `TOTPCodeChip` countdown ring, count pills) **stay opaque tinted** — they sit on content and must read clearly. No glass. (Explicit non-goal: do not glass these.)

### 4e. Floating glass action bars (rows) — the morphing centerpiece

- In `HostRow.swift` / `TunnelRow.swift`, the hover-revealed action cluster (currently inline `IconTextActionButton`s, e.g. `HostRow.swift:201`, `TunnelRow.swift:269`) is wrapped in `glassActionBar { }` (`GlassEffectContainer`).
- Each action pill gets `.glassEffect()` + `.glassEffectID(<stable id>, in: ns)`; appearance/disappearance on hover is driven by `withAnimation(.bouncy)` so pills **fluidly merge out of / collapse into** a single glass blob. This is the signature macOS 26 interaction.
- The cluster floats above the row (control layer), transient — HIG-compliant.

### 4f. Sheets — adopt system glass, fix latent glass-on-glass

- On macOS 26, sheets receive an inset Liquid Glass background automatically. Remove custom `.background(.bar)` headers/footers (e.g. `AddHostSheet.swift:87,308`, `WelcomeSheet.swift:107`) that fight the system treatment.
- **Critical correction:** form-field groups inside sheets currently use `glassCard()` (e.g. `AddHostSheet.swift:147`, `NewTunnelSheet.swift:121`, `CustomNodeSheet.swift:40`, `NodePickerSheet.swift:70`, `WelcomeSheet.swift:66`, `TunnelDetailsPopover.swift:103`). Once the sheet background is system glass, `glassCard` inside it = **glass-on-glass (rule #2 violation)**. These inner groups switch to **`groupedContent()`** (opaque) for correct single-layer hierarchy.
- `TunnelDetailsPopover` stat cells (`color.opacity(0.10)`, line 202) stay opaque tinted (content metric — legibility).

### 4g. CommandPalette (`CommandPalette.swift`)

- It is a floating overlay = chrome. Replace raw `.background(.ultraThinMaterial)` (line 201) with `glassChrome()` / `interactiveGlass`. Selection highlight (line 237, hardcoded accent) → tinted glass highlight so the selected row reads as a floating glass chip.

### 4h. Settings (`SettingsView`)

- Keep native `.formStyle(.grouped)` (line 131) — it auto-adopts correct materials on 26. Remove any explicit opaque section backgrounds that would fight it. No structural change; verify visually.

---

## 5. New / changed files

| File | Responsibility | Change |
|------|----------------|--------|
| `project.yml` | build config | deployment target → 26.0 |
| `DesignTokens.swift` | styling chokepoint | drop fallbacks; add `windowGlassBackground`, `glassActionBar`, `interactiveGlass`, concentric corners; `VisualEffectBackground` representable |
| `Auto2FAApp.swift` | app shell | `.unified` toolbar style |
| `ContentView.swift` | main window | translucent window bg, `.toolbar{}`, `.searchable`/search, `.backgroundExtensionEffect()` |
| `HostsView.swift` / `TunnelsView.swift` | content panes | slim headers; search filtering; keep `groupedContent` |
| `HostRow.swift` / `TunnelRow.swift` | rows | hover action cluster → morphing `glassActionBar` |
| `AddHostSheet`, `NewTunnelSheet`, `NodePickerSheet`, `CustomNodeSheet`, `WelcomeSheet`, `TunnelDetailsPopover` | sheets | drop `.bar` chrome; inner `glassCard` → `groupedContent` (de-glass-on-glass) |
| `CommandPalette.swift` | palette | raw material → `glassChrome`; tinted-glass selection |
| `SettingsView` (`Settings.swift`) | settings | verify native form glass; remove fighting backgrounds |

---

## 6. Animations & morphing

- Row action morph: `withAnimation(.bouncy)` + `glassEffectID` in a shared `GlassEffectContainer`.
- Toolbar/sheet transitions: system-provided (free with native adoption).
- Existing motion (`hoverLift`, error-banner/undo transitions) retained but re-evaluated so it doesn't fight the new glass (e.g. drop redundant manual shadows where glass already provides depth).
- All motion is automatically toned down by the system under Reduce Motion — no manual handling.

---

## 7. Error handling & edge cases

- **Reduce Transparency / Increase Contrast:** handled by the system; we add no manual `accessibilityReduceTransparency` overrides.
- **Concentric corners:** `.containerConcentric` needs a container that defines a corner radius; for free-floating pills (no such container) use explicit `Radius.*` instead, to avoid a zero-radius surprise.
- **Build sweep:** bumping to 26-only may reveal other `#available` sites or APIs guarded for older systems; sweep `auto2fa-mac` for `#available` / `@available` and simplify.
- **Window without titlebar (menu-bar overflow window):** `installMenuBarOnce()` (`Auto2FAApp.swift:138`) finds the main window by title "SSH2FA"; the toolbar/material changes must not change the window title or this lookup breaks.

---

## 8. Testing strategy

These are **presentation-only** changes (no daemon/behavior logic touched), so correctness is guarded by:

1. **Build gate:** `xcodegen generate` + `xcodebuild -scheme Auto2FA ... build` against the macOS 26 SDK must succeed — this catches glass-API misuse and the deployment-target sweep.
2. **Existing unit tests:** `Auto2FATests` (`SyncCoreTests`) must still pass — proves no logic regression.
3. **New unit tests for any extracted pure logic:** if a non-view helper is introduced (e.g. a function mapping host/tunnel state → semantic glass tint, or the host+tunnel search-filter predicate), it gets dedicated unit tests in `Auto2FATests` (per the repo's testing requirement). Pure SwiftUI view modifiers are verified by build + manual QA, not unit tests.
4. **Manual visual QA checklist:** toolbar renders as floating glass; window margins translucent while row text fully legible; row hover action bar morphs; sheets show system inset glass with opaque inner groups (no glass-on-glass); palette is glass; Reduce Transparency still legible (toggle in System Settings); light + dark mode.

---

## 9. Risks & mitigations

| Risk | Mitigation |
|------|-----------|
| Translucent window hurts legibility | Content rows stay on opaque `groupedContent()`; only margins/chrome translucent |
| Glass-on-glass inside sheets | Inner form groups switch `glassCard` → `groupedContent` (4f) |
| 26-only locks out old macOS | Explicitly accepted by user; documented consequence |
| Toolbar consolidation changes add-action affordance | Keep a slim inline `+` per section pointing at the same action |
| `.containerConcentric` zero-radius on non-container surfaces | Use explicit `Radius.*` for free-floating pills |
| Menu-bar window lookup by title breaks | Do not alter window title |
| Over-glassing micro-indicators | Explicit non-goal: badges/dots/rings stay opaque |

---

## 10. Out of scope (restated)

AppKit menu-bar `NSMenu`; the notch (DynamicNotchKit); `LogViewerView` deep restyle; `BiometricLock` screen. These may be revisited in a follow-up.
