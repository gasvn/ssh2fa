# Rename: Auto2FA → SSH2FA

**Goal:** rename the product to **SSH2FA** before the first release, so the name
self-explains (it's about SSH logins, not an authenticator app). Decided:
- **Surgical depth** — rename everything a user or the OS sees; keep internal
  Rust crate names (`a2fa-*`) and source folders (`auto2fa-rs/`, `auto2fa-mac/`,
  Swift target `Auto2FA`) as an internal codename (zero user-visible benefit to
  churning ~hundreds of imports).
- **Migration A** — no migration code ships (no external users). On the dev's
  live machine, read existing secrets from the old Keychain and re-add the 4
  hosts through the new app so the new daemon writes them with a correct ACL.

## Token map (the exact substitutions)

| Old | New | Where | Safe-global? |
|-----|-----|-------|--------------|
| `com.auto2fa` | `com.ssh2fa` | bundle id, LaunchAgent label, prefs/caches paths | yes |
| `~/.auto2fa` / `.auto2fa/` | `~/.ssh2fa` / `.ssh2fa/` | daemon dir, socket dir, marker | yes (leading dot — won't hit `auto2fa-rs`) |
| `auto2fa.sock` | `ssh2fa.sock` | unix socket | yes |
| `auto2fa_daemon.log` | `ssh2fa_daemon.log` | /tmp log | yes |
| `"auto2fa"` (Keychain SERVICE) | `"ssh2fa"` | keychain.rs const + `security -s auto2fa` in swift/scripts/cask | targeted |
| `a2fa-daemon` | `ssh2fa-daemon` | daemon **binary** name only (NOT `a2fa-core`/`a2fa_core`) | targeted |
| `Auto2FA` (display) | `SSH2FA` | UI strings, window/menu titles, docs, README, landing, cask | **file-by-file** (keep code identifiers: `Auto2FAApp`, folder `Auto2FA/`, Xcode target) |
| repo `gasvn/auto2fa` | `gasvn/ssh2fa` | doc/cask URLs + GitHub repo rename | targeted |

**App product name:** in `project.yml` set `PRODUCT_NAME: SSH2FA` + bundle id
`com.ssh2fa.app` + `CFBundleName: SSH2FA`, while leaving the **target/folder**
named `Auto2FA` (internal). Result: `SSH2FA.app`, executable `MacOS/SSH2FA`,
bundle id `com.ssh2fa.app` — internal codename preserved.

**Daemon binary:** rename only the daemon crate's binary output to
`ssh2fa-daemon` (Cargo `[[bin]] name`), keep crate/package + lib `a2fa-*`.
Update every Swift/script/cask reference to the binary name.

## Execution order (verification-gated, rollback-safe)

1. **Branch** `rename-ssh2fa` (done).
2. **Rust:** SERVICE const, `~/.auto2fa`→`~/.ssh2fa`, socket, log, binary
   `ssh2fa-daemon`. → `cargo build` + `cargo test` (451) + clippy green.
3. **Swift:** bundle id/PRODUCT_NAME/CFBundleName, LaunchAgent label, paths,
   socket, log, daemon-binary refs, Keychain `-s ssh2fa`, display strings. →
   `xcodebuild` green.
4. **Docs/cask/scripts/landing/README:** strings + repo URLs + cask token
   `auto2fa`→`ssh2fa` (file `Casks/ssh2fa.rb`). → `brew style` green.
5. **Package** `SSH2FA.app` via package-app.sh.
6. **GATE — live cutover (needs explicit go-ahead):**
   a. Dump old secrets: `security find-generic-password -s auto2fa -a <host>.password -w` (+ `.otpauth`) for each host (from `~/.ssh/passwords.json` host list).
   b. Deploy `SSH2FA.app`; it installs `com.ssh2fa.daemon` → new `~/.ssh2fa`.
   c. Re-add each host via the new app's Add-Host (paste dumped secrets); new daemon stores under service `ssh2fa` with correct ACL; test-login confirms.
   d. **Verify 4 hosts Connected.** Only THEN bootout + remove the old
      `com.auto2fa.daemon` LaunchAgent + `~/.auto2fa` + old Keychain `auto2fa`
      entries.
7. **Commit, push, merge to rust-rewrite + main, rename GitHub repo**
   `gasvn/auto2fa`→`gasvn/ssh2fa` (auto-redirects old URLs).

**Rollback:** through step 5 everything is on a branch, nothing on the live
machine changes. The old `com.auto2fa.daemon` keeps running until step 6d
verifies the new one — if cutover fails, the old setup is still live.

## Risks
- **Keychain ACL prompt-storm** (memory: has wedged the machine) — mitigated by
  Migration A (new daemon writes its own entries) instead of cross-service copy.
- **Over-broad replace** breaking `a2fa_core` imports or folder paths —
  mitigated by targeted (not global) replacement of `a2fa-daemon` and `Auto2FA`.
- **Two daemons fighting for a socket** — new daemon uses `~/.ssh2fa/ssh2fa.sock`
  (different path), so old + new can run side-by-side during cutover.
