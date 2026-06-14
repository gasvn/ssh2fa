# SSH Config ↔ SSH2FA Sync — Design Spec

**Date:** 2026-06-13
**Status:** Approved direction (Approach A), pending spec review
**Goal:** Make registering hosts effortless and keep `~/.ssh/config` and SSH2FA's registered hosts consistent — by *reading* the user's config for discovery, *augmenting* it with a single managed `Include` for ControlMaster, and *reconciling* drift — **without ever editing the user's own `Host` blocks.**

---

## 1. Context & problem

Today there are two separate stores linked only by the **ssh alias**:

- **`~/.ssh/config`** — user-maintained `Host <alias>` blocks (hostname, user, and the ControlMaster/ControlPath that makes warm-master reuse work).
- **`passwords.json`** (daemon-maintained, in `~/.ssh/`) — per-host password + 2FA secret, keyed by alias. The app's host list = these.

The app currently only **reads** config to *validate* (`AddHostSheet.aliasInSSHConfig`, a Troubleshoot health check) and **never writes** it. Friction:

1. **Double entry** — adding a host means editing `~/.ssh/config` *and* registering in the app.
2. **ControlMaster is unintuitive** — users must hand-configure `ControlMaster auto` / `ControlPath ~/.ssh/cm-auto2fa-%h` / `ControlPersist`, or the Terminal button / interactive `ssh` re-prompts for 2FA instead of reusing the warm master.
3. **Drift** — change one store, the other doesn't know.

## 2. Design principle (non-negotiable)

**Never clobber the user's hand-maintained config.** Researchers' `~/.ssh/config` is precious. SSH2FA may:
- **Read** any `Host` block (discovery, reconciliation).
- **Write** exactly one thing into `~/.ssh/config`: a single `Include ssh2fa.conf` line, added once, clearly marked, with a backup taken first, and only after explicit opt-in.
- **Own** `~/.ssh/ssh2fa.conf` entirely (it may rewrite it freely).
- It must **never** add, edit, or remove the user's own `Host` blocks (no hostname/user/anything).

## 3. Architecture

**All client-side (Swift).** No daemon/Rust change is needed:
- The daemon already spawns masters with explicit `-o ControlPath=…` flags, so it doesn't depend on the config.
- The managed `Include` exists purely for the **user's interactive `ssh <alias>`** and the **Terminal button** to reuse the warm master.
- The app is non-sandboxed and already reads `~/.ssh/config`; it can read+write these files directly.

New/changed pieces:
- `SSHConfigParser` (new, pure) — parse `~/.ssh/config` into `[ConfigHost]` (alias(es), hostname, user). Top-level `Host` blocks for v1; tolerant of comments/indentation. Unit-tested.
- `SSHConfigManager` (new) — generate `ssh2fa.conf` from the registered hosts, ensure the `Include` line, with backup + atomic writes + idempotency.
- Discovery UI (sheet/section) — list config hosts, one-click "Enable 2FA".
- Reconciliation — surface drift in Hosts view + Troubleshoot.
- Hooks in `AppState.addHost` / host-delete to regenerate `ssh2fa.conf`.

## 4. Capability 1 — Discovery / import (read)

- Parse `~/.ssh/config` → list every `Host` alias with its hostname/user and whether it's already registered (alias present in `appState.hosts`).
- **Entry point:** an "Add from ~/.ssh/config" affordance (in the Add Host flow and/or a dedicated "Import" sheet) showing config hosts **not yet registered**, each with **Enable 2FA** → opens the existing `AddHostSheet` **pre-filled** with the alias (hostname/user are informational; the app keys on alias) so the user only enters password + 2FA (or scans the QR).
- Benefit: zero re-typing; the app is aware of the user's machines.

## 5. Capability 2 — Managed ControlMaster `Include` (write, own file only)

- The app maintains **`~/.ssh/ssh2fa.conf`**: one block **per registered host**, e.g.
  ```
  # Managed by SSH2FA — do not edit. Regenerated on host add/remove.
  Host kempner
      ControlMaster auto
      ControlPath ~/.ssh/cm-auto2fa-%h
      ControlPersist yes
  ```
  Per-host (not `Host *`) so SSH2FA never multiplexes hosts the user didn't register.
- The app ensures **`Include ssh2fa.conf`** is present in `~/.ssh/config`, inserted **near the top** (ssh uses first-obtained value for ControlMaster/ControlPath, so it must precede conflicting user blocks), wrapped in a marked region:
  ```
  # >>> SSH2FA managed (Include) >>>
  Include ssh2fa.conf
  # <<< SSH2FA managed (Include) <<<
  ```
- **Opt-in:** the first time SSH2FA wants to add the `Include`, it asks: *"Set up warm-connection reuse? SSH2FA will back up ~/.ssh/config and add one `Include` line (it never edits your own Host blocks)."* with **Set up** / **Not now** / **Show me the change**.
- **Safety:** timestamped backup (`~/.ssh/config.ssh2fa-backup-<ts>`) before the first edit; **atomic** writes (write temp + rename); **idempotent** (re-running never duplicates the line/region); `ssh2fa.conf` perms `600`.
- Benefit: warm-reuse + the Terminal button just work, with zero manual ssh-config surgery.

## 6. Capability 3 — Reconciliation / drift

- Compare registered hosts vs parsed config aliases on app focus / periodic reload:
  - **Registered but alias gone from config** → an inline warning on the host row + a Troubleshoot item ("kempner is registered but not a `Host` in ~/.ssh/config — it won't connect"). Offer: open the wizard / remove registration.
  - **In config but not registered** → a quiet nudge ("N hosts in your ssh config aren't 2FA-enabled — enable?") linking to the import sheet.
- `ssh2fa.conf` is regenerated whenever the registered set changes (add/remove), and stale `Host` blocks for unregistered hosts are dropped from it.

## 7. Safety, edge cases, idempotency

| Case | Handling |
|------|----------|
| `~/.ssh/config` missing | Create it (with the managed region) only on opt-in; else just skip the Include and operate read-only. |
| `Include ssh2fa.conf` already present (any form) | Detect + don't duplicate; normalize into the marked region. |
| User later deletes the Include | App notices on reconcile and re-offers (doesn't force). |
| Config is a symlink | Resolve + write through the link target; back up the target. |
| `SSH_CONFIG_PATH` set | Honor it for the directory (matches daemon + `aliasInSSHConfig`). |
| `Match`/`Include`/globs in user config | v1 parses top-level `Host` blocks for discovery; does not try to fully emulate ssh's matcher. Never edits them. |
| Write permission denied | Surface a clear error; never partially write. |
| Concurrent edits | Atomic temp+rename; re-read before each managed-region rewrite. |

## 8. Testing

- **Pure logic, unit-tested** (compiled into the headless test bundle like `SearchFilter`/`SlurmTime`):
  - `SSHConfigParser`: aliases (multi-alias `Host a b`), hostname/user extraction, comments/indentation, empty/missing.
  - `SSHConfigManager` generators: `ssh2fa.conf` content from a host set; Include-region insert/detect/idempotency on sample config strings (string-in/string-out, no real FS).
- **FS-touching writes**: a couple of tests against a temp directory (write → re-read → assert; re-run → no duplication).
- **UI**: build-gated + manual QA (import flow, opt-in prompt, backup created, Terminal reuses master after setup).

## 9. Out of scope (v1)

- Following `Include` directives / `Match` blocks when *parsing* (top-level `Host` only).
- Writing the user's `Host` blocks (hostname/user) — Approach C, explicitly rejected.
- Any daemon/Rust change.
- Two-way edits to the user's blocks; SSH2FA only ever owns `ssh2fa.conf` + the single Include line.
