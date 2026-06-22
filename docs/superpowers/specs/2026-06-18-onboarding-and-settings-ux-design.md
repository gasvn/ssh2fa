# New-User Onboarding + Settings Clarity — Design Spec

**Date:** 2026-06-18
**Status:** Approved direction (import-first onboarding + empty-state checklist + Settings clarity), pending spec review
**Goal:** Get a brand-new user to their **first connected host** with minimal friction, and make Settings self-explanatory — aligned with 2026 onboarding standards (value-first, lead with the easiest path, progressive disclosure, skippable, no tutorial walls).

---

## 1. Context & problem

Today's first-run is `WelcomeSheet` — a single modal that explains 3 steps + the menu bar, then offers **"Add my first host"** (the *manual* wizard) as the primary action. Problems vs. 2026 standards:

1. **Wrong primary action.** The easiest path — import from `~/.ssh/config` (one click, alias pre-filled) — is the feature a new user is *least* likely to discover, yet the welcome's primary button is manual entry. (Import UI exists: `ImportHostsSheet`, `AddHostSheet(prefillAlias:)`, batch-return to the importer — all already built.)
2. **Info-dump.** All 3 steps + tunnels/SLURM are explained up front (a mild "tutorial wall"). 2026 guidance: show what matters *when* it matters; reveal SLURM/tunnels only when relevant.
3. **Settings opacity.** `Settings` already has per-toggle captions (good), but: several use backticks (`` `ssh <host>` ``, `` `Include` ``) which **SwiftUI `Text` does not render as code** → users see literal backtick characters; copy carries jargon (daemon, LaunchAgent, ControlMaster, SMAppService) with no top-level "what is this app / how does it work" framing.

## 2. Design principles

- **Value first, one clear next action.** Lead with the result ("type your 2FA once, then just `ssh`") and the single easiest action (import).
- **Progressive / contextual.** Don't explain tunnels/SLURM in onboarding; surface them when the user opens the Tunnels tab.
- **Skippable + re-visitable.** Skipping never traps; the empty-state checklist is the always-available guide.
- **Plain language in Settings.** Every control says what it does and why you'd want it, in human terms; no unrendered markup.
- **Reuse, don't rebuild.** Build on `WelcomeSheet`, `ImportHostsSheet`, `AddHostSheet` (prefill + batch-return), `celebrateFirstConnectIfNeeded`, the existing import/warm-reuse plumbing.

## 3. Architecture

All client-side (Swift/SwiftUI). No daemon/Rust change. Three onboarding pieces + one Settings pass:

- **`WelcomeSheet` rework** (modify) — import-first first-run.
- **`HostsView` empty state** (modify) — the "Get started" checklist (capability C).
- **First-connect hint** (modify) — reuse the existing first-connect celebration to nudge "try Open Terminal".
- **`OnboardingChecklist`** (new, pure) — computes which checklist steps are done from `(hosts, hasAnyConnected)`; unit-tested.
- **`Settings` clarity pass** (modify) — fix backticks, add a "How it works" explainer, plain-language copy, ordering.

## 4. Capability 1 — Import-first welcome (rework `WelcomeSheet`)

On first run (daemon reports 0 hosts):

- **Header:** the value prop, tightened to one line + a one-line "who it's for" (keep the warm, honest tone). Drop the 3-row "how it works" card from the *welcome* (it moves to the empty-state checklist / contextual hints).
- **Primary action depends on config:**
  - If `appState.importableHosts` is non-empty → a compact **"Found N hosts in `~/.ssh/config`"** card with a prominent **"Pick hosts to protect →"** button that opens the existing `ImportHostsSheet`. (The importer already lists un-registered config hosts with hostname/user and an "Enable 2FA" per host → prefilled wizard → batch-return.)
  - If empty (no config, or all registered) → primary becomes **"Add a host manually"** (`presentAddHost()`), with a one-line note that SSH2FA refers to hosts by their `~/.ssh/config` alias.
- **Secondary:** "Add a host manually" (when import is primary) and **"Skip for now"**. All dismissal routes persist `welcomeShown` (already fixed).
- **No tunnels/SLURM mention here.**

Width/structure stays a modal sheet (consistent with the app's other sheets), but shorter and action-led.

## 5. Capability 2 — Empty-state "Get started" checklist (`HostsView`)

When the Hosts list is empty (post-skip, or before the first add), the Hosts pane shows a **Get started** checklist instead of a bare empty string. It is the in-app, always-revisitable onboarding (capability C, light):

- ☐/☑ **Add your first host** — done when `appState.hosts` is non-empty. Row action: the import CTA (or "Add manually").
- ☐/☑ **See it connect** — done when any host `displayState == .connected`.
- ☐/☑ **Open a terminal with no 2FA** — done when the user has used the Terminal button at least once (a `UserDefaults` flag set by `TerminalLauncher`).
- A muted line: "On a SLURM cluster? You can also forward a port to a compute node — see the Tunnels tab." (contextual pointer, not a step.)
- The existing **"Add from ~/.ssh/config"** / **Add Host** buttons remain the primary affordances.

The checklist disappears once the user has ≥1 host AND it has connected (i.e., they're past onboarding); it can be re-shown if they remove all hosts. State is derived, not stored (except the "used terminal" flag).

## 6. Capability 3 — First-connect contextual hint

Reuse `AppState.celebrateFirstConnectIfNeeded` (already fires a one-time notch on the first-ever host connect). Adjust its copy to nudge the next action: e.g. **"Connected — your `ssh` is warm now. Try a host's Terminal button: no 2FA."** One-time, dismissible, gated on the existing first-connect flag. No new surface.

## 7. Capability 4 — Settings clarity pass

A copy/clarity pass on `SettingsView` (General tab), no behavior change to the toggles themselves:

1. **Fix unrendered backticks** — every caption that uses backticks for `ssh`/`Include`/etc. is rebuilt so code terms render correctly (concatenated `Text` with `.font(.system(.caption, design: .monospaced))` spans, or plain text). This is a real rendering bug today.
2. **"How SSH2FA works" explainer** — a short, friendly card pinned at the **top of General** (above the first section): 2–3 lines — *"SSH2FA answers the password + 2FA prompt for you and keeps a warm connection to each host, so `ssh`, `scp`, and your editor connect instantly with no code to type. Your password and 2FA secret live in the macOS Keychain."* Sets the mental model before the toggles.
3. **Plain-language copy** — reduce jargon in captions: "the auto2fa daemon" → "the background helper that keeps your connections alive"; "LaunchAgent" / "SMAppService" phrasing softened or moved to a secondary line; "ControlMaster" → "warm connection". Keep each caption's "what + why".
4. **Order by relevance** — sections ordered: How-it-works (new) → Launch → Terminal → Warm reuse → Notifications → Tunnels → Sleep & Wake → Security → Daemon (advanced last) → iCloud sync. (Current order is close; tighten.)

Troubleshoot/About tabs unchanged (Troubleshoot already exists; deep-link was added earlier).

## 8. Edge cases

| Case | Handling |
|------|----------|
| `~/.ssh/config` missing / no importable hosts | Welcome primary becomes "Add a host manually"; checklist step 1 action = Add Host. |
| User skips welcome | `welcomeShown` set; empty-state checklist is the fallback guide; never re-nags the modal. |
| User removes all hosts later | Checklist can re-appear (derived state); welcome modal does NOT re-appear (welcomeShown stays true). |
| Many config hosts incl. non-cluster (github, VPS) | Importer lists them; user picks; no pressure to enable all (no "enable everything" default). |
| Reduce Motion / accessibility | No new animations beyond existing; first-connect hint respects the notch settings already in place. |

## 9. Testing

- **Pure logic, unit-tested** (headless bundle, like `SearchFilter`/`SlurmTime`):
  - `OnboardingChecklist`: given `(hostCount, anyConnected, usedTerminal)` → the set of completed steps + whether the checklist should show.
- **UI**: build-gated + manual QA — first-run with config hosts (import path), first-run with no config (manual path), skip → checklist, first connect → hint, Settings render (no literal backticks, explainer present, copy reads plainly).

## 10. Out of scope (v1)

- No multi-screen tour / carousel (anti-pattern per 2026 guidance).
- No role/persona segmentation beyond the implicit "just SSH" vs "SLURM tunnels" contextual split.
- No telemetry/analytics.
- No daemon/Rust change.
- No restructuring of the Tunnels onboarding (only a one-line pointer from the Hosts checklist).
