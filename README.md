<div align="center">

# SSH2FA

### Log into your 2FA-gated cluster once. Stay connected.

A macOS menu-bar app that keeps a warm SSH connection to your Duo / TOTP-protected
hosts and answers the 2FA login for you — so your own `ssh` connects **instantly**,
with no code to retype and zero ssh-config editing.

[**Website**](https://shgao.site/ssh2fa/) · [**Download**](https://github.com/gasvn/ssh2fa/releases) · [**Security model**](SECURITY.md)

![platform: macOS 26+](https://img.shields.io/badge/platform-macOS%2026%2B-black?logo=apple)
![arch: universal](https://img.shields.io/badge/arch-arm64%20%2B%20x86__64-informational)
![license: MIT](https://img.shields.io/badge/license-MIT-green)
[![latest release](https://img.shields.io/github/v/release/gasvn/ssh2fa?display_name=tag&sort=semver)](https://github.com/gasvn/ssh2fa/releases)
[![website](https://img.shields.io/badge/website-shgao.site%2Fssh2fa-5b9cff)](https://shgao.site/ssh2fa/)

</div>

---

You type your Duo / TOTP code **once**. After that, SSH2FA holds a live
ControlMaster connection to each host, so every `ssh`, `scp`, `rsync`, and
editor-remote reuses it — no second-factor prompt, ever:

```console
$ ssh gpu-04
✓ connected — reused a warm master    (no password · no 2FA code)
[gpu-04 ~]$
```

> 👉 See the **[product tour with screenshots →](https://shgao.site/ssh2fa/)** for the menu-bar
> panel, the live SLURM countdown, the ⌘K command palette, Touch ID, and QR setup.

## Features

**Stay connected**
- 🔑 **No-retype 2FA login** — keeps one warm master per host and auto-submits your password + a fresh TOTP at login. Codes are serialized across hosts that share a Duo secret, so a code is never replayed in its window.
- ♻️ **Self-healing** — adopts live connections on launch and rebuilds them after sleep/wake, Wi-Fi changes, and reboots. Zero re-login.
- ⌨️ **Your terminal, zero config** — your own `ssh` reuses the warm master too, via one consented, backed-up `Include` line. It never touches your own `Host` blocks.

**Run your cluster**
- 🚇 **Port forwarding** — forward a local port to a running job's **SLURM compute node** (picked from a live `squeue` list, with a TIME_LEFT countdown and a heads-up before the allocation ends), or **straight to a registered host** (no jump host, no node).
- 📁 **Mount in Finder** — browse a host's filesystem over sshfs.
- ⌘ **Command palette + menu bar** — connect, open a terminal, mount, or tunnel from `⌘K` or the menu-bar panel. Full **CLI & TUI** too.

**Set up & stay safe**
- 🚀 **Zero-config setup** — add a host by **name, address, and username**; SSH2FA writes the SSH config for you, so you never need to know or edit `~/.ssh/config`. Already have aliases? **One-click import** reads them from your config.
- 📷 **QR / paste the secret** — scan a Duo / TOTP **QR code**, or paste the `otpauth://` URL, to capture the 2FA secret (no Base32 to type).
- 🔒 **Locked down** — passwords and TOTP secrets live in the macOS **Keychain**; an optional **Touch ID** lock gates revealing a credential. No telemetry.
- 🩺 **Safe by default** — a Troubleshoot panel runs health checks, hosts are **test-logged-in before saving** (never a lockout), and you're warned if a host drifts out of your ssh config.
- 🍎 **Native macOS 26** — Liquid Glass UI, universal binary, iCloud preference sync, update notifications.

## 60-second quickstart

1. **Install** — `brew install --cask --no-quarantine gasvn/tap/ssh2fa`, or see [Install](#install) for the one-line script / DMG download.
2. **Add Host** → enter the host's **name, address, and your username** (or pick an existing ssh alias), then your **password** and **2FA secret** — type it, paste an `otpauth://` URL, or **scan the QR**. SSH2FA writes the SSH config for you and **test-logs-in before saving**.
3. Done — open a terminal and `ssh <alias>`. No code to type, and it stays connected.

Stuck? **menu bar → Troubleshoot…** runs health checks and tells you what's wrong.

> **What it's for, honestly:** this is built for **HPC / SLURM clusters that use
> keyboard-interactive 2FA** (e.g. FAS-RC with Duo). If you don't use a cluster,
> the warm-SSH + 2FA pieces still work fine; the tunnel / node-picker features
> just won't have anything to talk to. Because the second factor is stored on
> your Mac and submitted for you, this is a deliberate convenience/security
> trade-off — read the **[security model](SECURITY.md)** before you rely on it.

## How it works

- Maintains **one stable, health-checked master connection per host** — a dropped link is rebuilt automatically, without a fresh login.
- **Answers 2FA for you** at login (password + TOTP from the Keychain), serializing codes across hosts that share a Duo secret.
- **Adopts live masters** across daemon restarts and app updates — zero re-login.
- **Port forwarding:** forwards a local port to a running job's compute node resolved from a live `squeue` list (`ssh -N -J … -L …`, with staleness detection when the job ends), or directly to a registered host (`ssh -N -L …`).
- Recovers automatically after **sleep/wake** and network changes.

### Components

| Piece | What |
|-------|------|
| `SSH2FA.app` | SwiftUI menu-bar app — the UI. |
| `ssh2fa-daemon` | Rust background daemon — the engine. Runs under a per-user LaunchAgent. |
| `a2fa` / `a2fa-tui` | Rust CLI and terminal UI (optional; talk to the same daemon). |

The app and daemon communicate over a unix-socket JSON-RPC at `~/.ssh2fa/ssh2fa.sock`.

## Install

**Two ways — pick one. Both take under a minute.** SSH2FA isn't notarized yet
(the [$99 goal](https://shgao.site/ssh2fa/#support)), so each one clears macOS's
one-time "unverified developer" warning for you.

### Easiest — paste one line in Terminal

Open **Terminal** (press <kbd>⌘</kbd><kbd>Space</kbd>, type `Terminal`, hit Enter),
paste **one** block below, press Enter. SSH2FA downloads, installs, and opens
itself — no warnings, nothing else to click.

```sh
# If you use Homebrew (you also get `brew upgrade` later):
brew install --cask --no-quarantine gasvn/tap/ssh2fa
```

```sh
# No Homebrew? Use this instead — copy all 6 lines:
curl -fL https://github.com/gasvn/ssh2fa/releases/latest/download/SSH2FA.dmg -o /tmp/SSH2FA.dmg \
  && hdiutil attach /tmp/SSH2FA.dmg -nobrowse -quiet \
  && ditto /Volumes/SSH2FA/SSH2FA.app /Applications/SSH2FA.app \
  && hdiutil detach /Volumes/SSH2FA -quiet \
  && xattr -dr com.apple.quarantine /Applications/SSH2FA.app \
  && open /Applications/SSH2FA.app
```

### No Terminal? Just click through it

1. **[Download SSH2FA.dmg](https://github.com/gasvn/ssh2fa/releases/latest/download/SSH2FA.dmg)** — it saves to your **Downloads** folder.
2. Double-click **SSH2FA.dmg**. In the window that opens, **drag the SSH2FA icon onto the Applications folder** shown beside it.
3. Open your **Applications** folder and double-click **SSH2FA**. macOS says *"Apple could not verify…"* — click **Done** (not "Move to Trash").
4. Open **System Settings → Privacy & Security** and scroll to the bottom. You'll see *"SSH2FA was blocked…"* — click **Open Anyway**, type your Mac password, then click **Open**.
5. Done — SSH2FA's icon is now in your **menu bar** (top-right of the screen). It has no Dock icon; it lives up there.

Once it's open, click the **SSH2FA menu-bar icon → Add Host** to connect your first
cluster (credentials are stored in the macOS Keychain). On first run the app also
installs its background helper (LaunchAgent `com.ssh2fa.daemon`) — nothing for you
to configure.

> **When it's notarized** (the $99 Developer ID), all of this collapses to
> "download → open." See [docs/RELEASE.md](docs/RELEASE.md) for the notarization
> setup and building from source (a self-built app has zero Gatekeeper friction).

## Requirements

- **macOS 26+** — universal binary (Apple Silicon + Intel).
- **No SSH config required** — the guided **Add Host** writes it for you. (Already have `~/.ssh/config` aliases? The app can import them instead.)
- A host that uses **keyboard-interactive 2FA** (Duo / TOTP) — you supply the password and the `otpauth://` secret once.
- **macFUSE + sshfs** (optional) only if you use the filesystem-mount feature.

## Build from source

<details>
<summary>Rust daemon / CLI / TUI + macOS app</summary>

```sh
# Rust daemon / CLI / TUI
cd auto2fa-rs
cargo build --release            # binaries in target/release/
cargo test --workspace -- --test-threads=1

# macOS app — the .xcodeproj is generated from project.yml by XcodeGen
brew install xcodegen            # one time
cd auto2fa-mac && xcodegen generate
xcodebuild -project SSH2FA.xcodeproj \
  -scheme SSH2FA -configuration Release build
```

`source "$HOME/.cargo/env"` first if cargo isn't on your PATH. The `.xcodeproj`
is a generated artifact (not in git) — run `xcodegen generate` after cloning. To
produce a signed, notarized DMG, use
[`auto2fa-mac/package-app.sh`](auto2fa-mac/package-app.sh) — see
[docs/RELEASE.md](docs/RELEASE.md).

</details>

## Security

SSH2FA stores SSH passwords and TOTP secrets in your macOS Keychain and submits
the second factor for you. That convenience means your second factor no longer
protects you against someone with access to your unlocked Mac — turn on the
optional **Touch ID** lock to require your fingerprint before a credential is
revealed. **Read [SECURITY.md](SECURITY.md)** for the full threat model before
you rely on it.

## Support

SSH2FA is free and open source. An [Apple Developer membership ($99/yr)](https://shgao.site/ssh2fa/#support)
would let the app be **notarized** — installing with zero Gatekeeper warnings for
everyone. If it saves you the daily 2FA dance, you can [chip in on Ko-fi ☕](https://ko-fi.com/shgao).

## License

[MIT](LICENSE).
