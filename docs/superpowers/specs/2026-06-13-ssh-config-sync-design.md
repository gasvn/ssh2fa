# SSH Config ↔ SSH2FA Sync — Design Spec

**Date:** 2026-06-13
**Status:** Approved direction, pending spec review
**Goal:** Make hosts effortless to register and免 2FA spam everywhere — by treating `~/.ssh/config` as the source of truth, *importing* from it, making the app's own actions reuse the daemon's warm master with **zero manual ssh-config editing by the user**, and reconciling drift — **without ever editing the user's own `Host` blocks** and with **only one** clearly-consented, backed-up line ever written to `~/.ssh/config`.

**Guiding principle (from the user):** *用户自己一行配置都不用改。* The app does all the ssh plumbing; the user only ever picks which hosts to protect and enters credentials once.

---

## 1. Context & problem

SSH2FA is **a 2FA-credential layer that sits on top of `~/.ssh/config`** — not an independent host database. Confirmed in code:

- **The Add-Host wizard stores only an *alias*.** It deliberately has no username/hostname-of-record field — "the login user comes from ssh config" (`AddHostSheet.swift:98-100`). The app keys everything on the alias.
- **The daemon connects by `ssh <alias>` and resolves connection details from config**, including the ControlPath via `ssh -G <alias>` (`ssh/control.rs`). It cannot reach a host the user's config (or a real hostname) doesn't describe.
- **The wizard already health-checks "is this alias a `Host` in `~/.ssh/config`?"** and warns if not (`AddHostSheet.swift:102-105`).

So `~/.ssh/config` already **is** the authority for *which hosts exist and how to reach them*; `passwords.json` (daemon-maintained, in `~/.ssh/`) just adds a password + TOTP secret keyed by alias.

Friction today:

1. **Double entry** — the host is already in `~/.ssh/config`, yet the user re-types its alias to register it.
2. **2FA re-prompts outside the daemon** — the daemon spawns its master with explicit `-o ControlMaster/-o ControlPath` flags (`pty_auth.rs:389`), so it works regardless of config. But the app's **Terminal button** runs bare `exec ssh "<host>"` (`TerminalLauncher.swift:64`), and the user's **own** `ssh <alias>` in their terminal, both miss the warm master and re-prompt for 2FA — unless the user hand-configures `ControlMaster`/`ControlPath` themselves (exactly the manual config we want to abolish).
3. **Drift** — a host registered but later removed from config silently can't connect; hosts in config aren't discoverable for one-click enablement.

## 2. Design principles (non-negotiable)

1. **Zero manual ssh-config editing by the user.** Anything ssh needs configured, the app configures (in its *own* file) or passes as explicit flags. The user never hand-writes `ControlMaster`/`ControlPath`.
2. **Never clobber the user's hand-maintained config.** SSH2FA may **read** any `Host` block; it must **never** add/edit/remove the user's own `Host` blocks.
3. **At most one line is ever written into `~/.ssh/config`**: a single `Include ssh2fa.conf`, added once, clearly marked, after a timestamped backup, and **only** after an explicit one-time consent. Everything else lives in `~/.ssh/ssh2fa.conf`, a file SSH2FA owns entirely.

## 3. Architecture

**All client-side (Swift). No daemon/Rust change.** The app is non-sandboxed and already reads `~/.ssh/config`; it can read+write these files and run `ssh -G` directly.

New / changed pieces:

- **`SSHConfigParser`** (new, pure) — parse `~/.ssh/config` into `[ConfigHost]` (alias(es), `HostName`, `User`). Top-level `Host` blocks for v1; tolerant of comments/indentation; skips wildcard patterns (`Host *`, globs). Unit-tested.
- **`ControlPathResolver`** (new, pure-ish) — given an alias, return the ControlPath the daemon's master is bound to, mirroring `resolve_control_base`: run `ssh -G <alias>`, take `controlpath`; else fall back to `~/.ssh/cm-ssh2fa-<alias>`. The parse half (pick `controlpath` out of `ssh -G` text + apply fallback) is pure and unit-tested; the spawn is a thin wrapper.
- **`SSHConfigManager`** (new) — generate `~/.ssh/ssh2fa.conf` from the registered host set; ensure the `Include` line in `~/.ssh/config`, with backup + atomic writes + idempotency. The string transforms (generate conf, insert/detect/normalize the marked Include region) are pure and unit-tested; FS writes are thin and temp-dir-tested.
- **`TerminalLauncher`** (modify) — emit the warm-reuse ssh invocation (capability 2).
- **Discovery/import UI** + **reconciliation** surfacing, plus hooks in `AppState.addHost` / host-delete to regenerate `ssh2fa.conf`.

## 4. Capability 1 — Import-first onboarding (read)

The spine of the feature, because import isn't a convenience — it's the natural way to register, given that hosts already live in config.

- Parse `~/.ssh/config` → list every concrete `Host` alias (skip wildcard/glob patterns) with its `HostName`/`User` (shown for recognition) and whether it's already registered (alias ∈ `appState.hosts`).
- **Onboarding centerpiece:** on first launch / empty host list, surface *"Found N hosts in your ~/.ssh/config — pick which to protect with 2FA."* Each un-registered host has **Enable 2FA** → opens the existing `AddHostSheet` **pre-filled with the alias** (HostName/User are informational; the app keys on alias) so the user only enters the password + TOTP (or scans the QR).
- Also available any time as an **"Add from ~/.ssh/config"** affordance in the Add-Host flow.
- Benefit: zero re-typing; the app already knows the user's machines.

## 5. Capability 2 — Terminal button warm-reuse (no config write)

Make the app's **"Open Terminal"** reuse the daemon's warm master so it never re-prompts for 2FA — **without touching any config file.**

- `TerminalLauncher` resolves the host's ControlPath via `ControlPathResolver` (same value the daemon's master binds — `control.rs:234` notes the single master binds the stable base ControlPath directly, "the path `ssh <host>` attaches to with nothing in between").
- The generated `.command` becomes:
  ```bash
  #!/bin/bash
  exec ssh -o ControlMaster=no -o ControlPath="$HOME/.ssh/cm-ssh2fa-<host>" "<host>"
  ```
  (`<host>`/path are the resolver's output; `ControlMaster=no` = attach-only, never try to *become* master from the terminal.) If `ssh -G` reported an explicit `controlpath`, that exact value is used instead of the fallback.
- If a live master socket exists, ssh attaches instantly — no 2FA. If none exists (daemon not connected yet), it falls back to a normal connection (ssh ignores a missing control socket with `ControlMaster=no` + nonexistent path → opens fresh); the button still works.
- Benefit: the app's primary "jump into a shell" path is warm and 2FA-free with **zero** edits to `~/.ssh/config`.

## 6. Capability 3 — Automatic ControlMaster, one-time consent (the single Include)

Closes the last gap — the user's **own** `ssh <alias>` typed in **their** terminal (outside the app) — without ever making them hand-edit config. The app owns the configuration; the user taps consent once.

- The app maintains **`~/.ssh/ssh2fa.conf`** (perms `600`, regenerated on host add/remove): one block **per registered host** —
  ```
  # Managed by SSH2FA — do not edit. Regenerated on host add/remove.
  Host kempner
      ControlMaster auto
      ControlPath ~/.ssh/cm-ssh2fa-kempner
      ControlPersist yes
  ```
  - **Per-host (not `Host *`)** so SSH2FA never multiplexes hosts the user didn't register.
  - **`ControlPath` is the literal per-alias fallback path (`cm-ssh2fa-<alias>`), not `%h`.** This is exactly what the daemon binds when config has no `controlpath`, so daemon + client + Terminal button all agree on one socket and enabling the Include causes **no** master rebuild.
- The app ensures **`Include ssh2fa.conf`** is present in `~/.ssh/config`, inserted **near the top** (ssh "first value wins" — it must precede any conflicting user block), wrapped in a marked region:
  ```
  # >>> SSH2FA managed (Include) >>>
  Include ssh2fa.conf
  # <<< SSH2FA managed (Include) <<<
  ```
- **One-time consent at the natural moment** — right after the **first** host is successfully enabled (not a buried Settings toggle, not a scary technical dialog):
  > *"Make `ssh <alias>` in your own Terminal skip the 2FA prompt too? SSH2FA backs up your SSH config and adds one `Include` line — it never touches your existing hosts."*
  > **[Set it up] · [Not now] · [Show me the change]**
  - **Set it up** → the Include is added; from then on **every** host is automatic, the user is never asked again.
  - **Not now** → the app's Terminal button warm-reuse (capability 2) still works; the offer is **not** nagged. A small "Enable warm reuse for my own terminal" affordance lives in Settings to turn it on later.
- **Safety:** timestamped backup (`~/.ssh/config.ssh2fa-backup-<ts>`) before the first edit; **atomic** writes (temp + rename); **idempotent** (re-running never duplicates the line/region; an existing `Include ssh2fa.conf` in any form is detected and normalized into the marked region, never duplicated).

## 7. Capability 4 — Reconciliation / drift (quiet)

Compare registered hosts vs parsed config aliases on app focus / reload, deliberately low-noise:

- **Registered but alias gone from config** → a real error (the host can't connect). Surface inline on the host row + a Troubleshoot item ("kempner is registered but is no longer a `Host` in ~/.ssh/config — it won't connect"), offering: open the wizard / remove registration. This extends the wizard's existing `aliasInSSHConfig` check to the live host list.
- **In config but not registered** → folded into the **import sheet** only (the "Found N hosts…" list). **No persistent nag** — researchers keep many non-cluster hosts (github, jump boxes, VPS) in config; the app must not pester them to "enable 2FA" on all of them.
- `ssh2fa.conf` is regenerated whenever the registered set changes; stale `Host` blocks for unregistered hosts are dropped from it.

## 8. Safety, edge cases, idempotency

| Case | Handling |
|------|----------|
| `~/.ssh/config` missing | Read-only discovery is empty. Create the file (with the managed region) only on Include opt-in. |
| `Include ssh2fa.conf` already present (any form) | Detect + don't duplicate; normalize into the marked region. |
| User later deletes the Include | Noticed on reconcile; re-offered once (never forced). |
| Config is a symlink | Resolve + back up + write through the link target. |
| `SSH_CONFIG_PATH` set | Honor it for the config directory (matches daemon + `aliasInSSHConfig`). `ssh2fa.conf`/backup live in the same directory. |
| `Match`/`Include`/globs in user config | v1 parses top-level concrete `Host` blocks for discovery; skips wildcard patterns; never tries to fully emulate ssh's matcher; never edits them. |
| Enabling Include changes the resolved ControlPath | Avoided by design: `ssh2fa.conf` uses the literal `cm-ssh2fa-<alias>` path = the daemon's no-config fallback, so the resolved path is unchanged and no master is rebuilt. |
| Terminal button when no master is live | `ControlMaster=no` + nonexistent socket → ssh opens a normal connection; button still works (may prompt for 2FA that one time). |
| Write permission denied | Clear error; never partially write (atomic temp+rename). |
| Concurrent edits | Atomic temp+rename; re-read before each managed-region rewrite. |

## 9. Testing

- **Pure logic, unit-tested** (compiled into the headless test bundle like `SearchFilter`/`SlurmTime`):
  - `SSHConfigParser`: single + multi-alias (`Host a b`), `HostName`/`User` extraction, comments/indentation, wildcard skipping, empty/missing input.
  - `ControlPathResolver` (parse half): pick `controlpath` out of sample `ssh -G` text (case-insensitive); fallback to `cm-ssh2fa-<alias>` when absent.
  - `SSHConfigManager` (string transforms): `ssh2fa.conf` content from a host set; Include-region insert / detect / normalize / idempotency on sample config strings (string-in/string-out, no real FS).
- **FS-touching writes**: a few tests against a temp directory (write → re-read → assert; re-run → no duplication; backup created).
- **UI / integration**: build-gated + manual QA — import flow pre-fills the wizard; Terminal button attaches to a live master with no 2FA; one-time Include consent backs up config and adds exactly one line; reconcile flags a removed host.

## 10. Out of scope (v1)

- Following `Include`/`Match` directives or emulating ssh's matcher when *parsing* (top-level concrete `Host` only).
- Writing the user's `Host` blocks / any `HostName`/`User` (rejected — the app keys on alias and relies on the user's config for reachability).
- Any daemon/Rust change.
- Two-way edits to the user's blocks; SSH2FA owns only `ssh2fa.conf` + the single consented `Include` line.
