# Zero-Config Setup + Self-Healing — Design Spec

**Date:** 2026-06-21
**Status:** Direction approved (zero-config onboarding, daemon reads an app-owned ssh config via `-F`, self-healing config layer); pending spec review
**Goal:** A freshly-installed user provides only the *irreducible* inputs — a server name/address, their username, password, and 2FA secret — and can log in. Everything else (the background helper, the SSH config that makes `ssh <alias>` resolve, the warm ControlMaster) is set up and kept healthy **automatically**, with no SSH-config knowledge required and no manual steps. Misconfigurations self-detect and self-repair; the user is prompted only when a genuinely user-only input is missing.

---

## 1. Context & problem

Today the app is **alias-first**: it stores only an `~/.ssh/config` alias and the daemon runs `ssh <alias>`, relying on the user's hand-written config to resolve HostName/User/Port. A user who doesn't already have a working `~/.ssh/config` entry cannot log in:

- `AddHostSheet` has a single "Hostname or SSH alias" field, **no** username/address/port — the comment says *"the login user comes from ssh config."*
- `addHost` sends only `{alias, password, otpauth}`; the daemon does `ssh <alias>` with no HostName/User.
- The managed `ssh2fa.conf` block only carries `ControlMaster/ControlPath/ControlPersist` (warm-reuse), and is **Included** into `~/.ssh/config` only behind a one-time consent alert (`WarmReuseConsent`). Decline it and nothing is set up.
- `unreachableRegisteredHosts` already *detects* hosts that won't resolve — but only warns; it never fixes them.

What IS already automatic (do not rebuild): the daemon + LaunchAgent install/refresh/keep-alive (`installBundledDaemonIfNeeded` + `ensureRunning`), and the connection-layer self-healing in the Rust daemon (master sweep, orphan kill, corrupt-`tunnels.json` backup/recovery, stale-tmp sweep).

## 2. Design principles

- **Irreducible input only.** The user supplies what *only they* can know — server address, username, password, 2FA secret. The app derives/automates the rest.
- **Never require touching the user's `~/.ssh/config` for login.** The daemon reads an **app-owned** ssh config via `ssh -F`, so a fresh install logs in without modifying the user's file at all.
- **One source of truth.** A small app-owned sidecar (alias → connection fields) generates one managed ssh config file; both the daemon (`-F`) and (optionally) the user's own terminal (`Include`) consume the same file. DRY.
- **Self-heal the config/setup layer.** On launch and on every reload, detect drift (managed file missing/stale, daemon down, host unresolvable, broken Include) and repair it idempotently. Surface to the user only a *missing irreducible input*.
- **Safe by construction.** Every file write is atomic + backed up + revertible (reuse the existing `atomicWrite`/backup primitives). The user's `~/.ssh/config` is only ever touched for the *optional* terminal-reuse bonus, never for core login.
- **YAGNI.** v1 = direct connect (HostName/User/Port). No ProxyJump, no IdentityFile, no rewriting the user's own `Host` blocks.

## 3. Architecture overview

```
 AddHostSheet (Name/Address/User/Port + password + 2FA)
        │  writes
        ▼
 Sidecar  ~/.ssh2fa/managed_hosts.json   (alias → {hostName, user, port})
        │  generates
        ▼
 Managed file  ~/.ssh/ssh2fa.conf        (Host <alias> { HostName/User/Port + ControlMaster/ControlPath/ControlPersist })
        │
   ┌────┴───────────────────────────────────────────────┐
   │ REQUIRED (login)                  OPTIONAL (terminal reuse)
   ▼                                   ▼
 Daemon wrapper  ~/.ssh2fa/ssh.conf     ~/.ssh/config gets one `Include ssh2fa.conf`
   Include ~/.ssh/ssh2fa.conf           (so the user's OWN `ssh <alias>` also resolves;
   Include ~/.ssh/config                 auto-enabled with backup + revert, NOT load-bearing)
   ▼
 Daemon runs every ssh with `-F ~/.ssh2fa/ssh.conf`
   → resolves HostName/User/Port + ControlPath from the managed file,
     inherits the user's globals via the wrapper's `Include ~/.ssh/config`.
```

**Why the wrapper file (`~/.ssh2fa/ssh.conf`) is separate from the managed file (`~/.ssh/ssh2fa.conf`):** the managed file has **no** `Include` lines, so when the optional terminal-reuse `Include ssh2fa.conf` is added to `~/.ssh/config`, there is no circular include. The daemon wrapper includes the managed file **and** the user's config (one-directional). `ssh -F` ignores `~/.ssh/config`/`/etc/ssh/ssh_config` by default, so the wrapper's explicit `Include ~/.ssh/config` is what re-inherits the user's globals.

## 4. Components

### 4.1 Sidecar store (new, app-owned) — `~/.ssh2fa/managed_hosts.json`
`ManagedHostConn { alias: String, hostName: String, user: String, port: Int }` keyed by alias. The **source of truth** for guided hosts' connection params. Written on guided add; read by the config generator. A host with no sidecar entry (imported alias relying on the user's own config) generates only the warm-reuse block (today's behavior) — backward compatible.

### 4.2 Managed config generator (modify `SSHConfigManager`)
`generateManagedConf` input changes from `[alias]` to `[(alias, conn: ManagedHostConn?)]`. Per alias:
- `conn == Some` → emit `Host <alias>` with `HostName/User/Port` **plus** the existing `ControlMaster/ControlPath/ControlPersist`.
- `conn == None` → emit only the three ControlMaster lines (unchanged).
The file itself contains **no** `Include` lines. New helper `ensureDaemonWrapper(dir)` writes `~/.ssh2fa/ssh.conf` = `Include ~/.ssh/ssh2fa.conf\nInclude ~/.ssh/config\n` (idempotent, atomic).

### 4.3 Daemon `-F` threading (Rust, `a2fa-core`)
Thread a single app-config path into the daemon's ssh invocations so it resolves from the wrapper. The path is derived from the existing `config_dir()` (`~/.ssh2fa/ssh.conf`). Call sites that need `-F` (they resolve HostName/User from config):
- `ssh/master.rs` `start_master` argv (the master login).
- `ssh/control.rs` the `ssh -G <host>` ControlPath resolution.
- `tunnels/forward.rs` `build_forward_argv` **and** `build_direct_argv`.
- `ssh/pty_auth.rs` the interactive login pty.
- the test-login path used by `testHostCredentials`.
The mux `-O check/exit` calls already pass explicit `-o ControlPath=` and need no config resolution; adding `-F` there is harmless but optional. **Backward-compat:** existing alias-only hosts still resolve because the wrapper ends with `Include ~/.ssh/config`. If the wrapper file is absent (older state), the daemon falls back to no `-F` (today's behavior) so it never hard-fails on a missing app config.

### 4.4 `AddHostSheet` redesign (Swift)
Step 1 fields become: **Name** (friendly label → sanitized to a valid ssh alias token), **Server address** (→ HostName), **Username** (→ User), **Port** (advanced, default 22), then **Password**, **2FA secret** (unchanged). On submit, BEFORE `addHost`/test-login: write the sidecar entry, regenerate `ssh2fa.conf`, ensure the daemon wrapper exists. Then the existing test-login runs (now it resolves the new host via `-F`) and gates saving. The import-from-config path (existing aliases) is unchanged — those create a sidecar-less host.

### 4.5 Self-heal reconcile (new, app-side) — runs on launch + on every `reloadAll`
A single idempotent pass (`SetupReconciler`, pure decision core + thin IO shell) that:
1. Ensures the daemon wrapper (`~/.ssh2fa/ssh.conf`) exists and is correct.
2. Regenerates `~/.ssh/ssh2fa.conf` from `(hosts ∪ sidecar)` — **always**, not gated on warm-reuse consent (the daemon needs it). `writeManagedConf` already skips unchanged content.
3. For each registered host that is **unresolvable** (`unreachableRegisteredHosts`) AND has no sidecar conn → it's the one case needing the user: surface a non-blocking "add the server address for `<host>`" affordance. With sidecar conn, step 2 already fixed it.
4. If the optional terminal-reuse Include is enabled, verify it's intact in `~/.ssh/config`; re-add if a user edit removed/broke it.
5. Daemon liveness is already handled by `ensureRunning`; the reconciler just confirms.
All repairs are atomic + backed up. The decision logic ("given this observed state, what repairs are needed?") is a pure function, unit-tested.

### 4.6 Optional terminal-reuse auto-enable (replaces the consent alert)
Because the Include is no longer load-bearing, the `WarmReuseConsent` blocking alert is removed. The Include into `~/.ssh/config` is **auto-enabled** on first host add (backup + Settings one-click revert + a one-time non-blocking notch toast: "Your own `ssh <host>` now skips 2FA too — undo in Settings"). A Settings toggle remains to turn it off (which `disableInclude` reverts cleanly). This is the only place the user's `~/.ssh/config` is touched, and it's non-essential.

### 4.7 Conflict detection
When the chosen **Name** collides with a `Host` the user already defines in their **own** config (not the managed file), warn in the wizard and require a different name or explicit "use my existing entry" (which falls back to today's alias-only host). App-created names never collide silently.

## 5. Data flow (guided add, fresh install)

1. User: Name="cannon", Address="login.rc.fas.harvard.edu", User="jdoe", Port=22, password, 2FA.
2. App: write sidecar `{cannon → host/user/port}` → regenerate `~/.ssh/ssh2fa.conf` (with `Host cannon { HostName … User … Port … + ControlMaster … }`) → ensure `~/.ssh2fa/ssh.conf` wrapper.
3. App → daemon `test_host_credentials(cannon, …)`; daemon runs `ssh -F ~/.ssh2fa/ssh.conf … cannon` → resolves → password+2FA login succeeds.
4. App: `addHost(cannon, …)` persists creds (Keychain) + starts the master (daemon `-F` again). Host is live.
5. App (one-time): auto-enable the terminal-reuse Include (toast), so `ssh cannon` in the user's own terminal also works.
6. Subsequent launches: the reconciler re-asserts all of the above; nothing for the user to do.

## 6. Error handling

- Wizard validation: address + user required; Name sanitized to a legal ssh token (and de-duplicated against existing aliases); port defaults to 22.
- Every file write: atomic + timestamped backup (reuse `atomicWrite`/existing backup). On write failure, surface a clear actionable error (never silent), and leave prior state intact.
- Test-login remains a hard gate before saving credentials (prevents the "bad creds → retry storm" class).
- Daemon `-F` fallback: missing wrapper → daemon behaves as today (no `-F`) rather than failing — so a half-initialized state degrades gracefully.
- Reconcile failures are logged + surfaced once, not loop-spammed.

## 7. Testing

- **Pure logic, headless unit tests:** managed-conf generation (block with/without conn fields; file has no Include); daemon-wrapper content; sidecar JSON round-trip (+ backward-compat decode of a missing field); Name sanitize + alias de-dup + conflict detection; the `SetupReconciler` decision function (given observed state → required repairs).
- **Rust unit tests:** the ssh argv builders include `-F <path>` (assert presence + position) for master/forward/direct/`-G`; backward-compat (no wrapper → no `-F`).
- **Integration/manual QA:** fresh-install flow end-to-end (guided add → login with an empty `~/.ssh/config`); existing-alias user keeps working under daemon `-F`; reconcile repairs a deleted `ssh2fa.conf`; revert cleanly removes the Include + managed blocks.

## 8. Phasing (for the implementation plan)

- **Phase 1 — zero-config login (core):** sidecar + conf generator change + daemon wrapper + daemon `-F` threading + `AddHostSheet` redesign + conflict detection. After this, a fresh user logs in with no `~/.ssh/config`.
- **Phase 2 — self-heal + terminal-reuse polish:** `SetupReconciler` (launch/reload), always-regenerate, drop the consent alert for auto-Include-with-revert, the "missing address" non-blocking prompt.

## 9. Out of scope (v1)

- ProxyJump / bastion / IdentityFile (the connection-scope decision was direct-only).
- Rewriting or reordering the user's own `Host` blocks (we only add one `Include` line + our marked region, both revertible).
- Migrating existing alias-only hosts into the sidecar (they keep working via the wrapper's `Include ~/.ssh/config`; offering to "fill in details" is a possible later nicety).
- Any change to the daemon's connection-layer self-healing (already robust).
