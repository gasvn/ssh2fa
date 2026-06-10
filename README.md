# Auto2FA

![platform: macOS 13+](https://img.shields.io/badge/platform-macOS%2013%2B-black?logo=apple)
![arch: universal](https://img.shields.io/badge/arch-arm64%20%2B%20x86__64-informational)
![license: MIT](https://img.shields.io/badge/license-MIT-green)
[![latest release](https://img.shields.io/github/v/release/gasvn/auto2fa?display_name=tag&sort=semver)](https://github.com/gasvn/auto2fa/releases)

A macOS menu-bar app + background daemon that keeps **SSH ControlMaster** pools
warm to 2FA-protected hosts, auto-answering the **Duo / TOTP** login so you log
in once and stay connected — plus **SLURM-aware port forwarding** to compute
nodes.

### 60-second quickstart

1. **Download** `Auto2FA.dmg` from [Releases](https://github.com/gasvn/auto2fa/releases) → drag the app to `/Applications` → open it. (Un-notarized build? First launch: **System Settings → Privacy & Security → “Open Anyway.”**)
2. **Add Host** → enter your ssh-config **alias**, **password**, and your **2FA secret** (the wizard has a "How do I get this?" walkthrough, incl. Duo). It test-logs-in before saving.
3. Done — open a terminal and `ssh <alias>`. No code to type, and it stays connected.

Stuck? **menu bar → Troubleshoot…** runs health checks and tells you what's wrong.

<!-- Screenshot: drop an image at docs/screenshot.png and uncomment:
![Auto2FA dashboard](docs/screenshot.png)
(Couldn't auto-capture one headlessly — it needs Accessibility permission.) -->


> **What it's for, honestly:** this is built for **HPC / SLURM clusters that
> use keyboard-interactive 2FA** (e.g. FAS-RC with Duo). The TOTP secret is
> stored in your macOS Keychain and submitted for you. If you don't use a
> cluster, the SSH-master + 2FA pieces still work; the tunnel / node-picker
> features just won't have anything to talk to. See **[Security model](SECURITY.md)**
> before you decide this is right for you — storing the second factor on the
> same machine is a deliberate convenience/security trade-off.

## What it does

- Maintains a **2-slot ControlMaster pool** per host, health-checked and
  auto-rotated, so a dropped connection recovers without a fresh login.
- **Answers 2FA for you** at login (password + TOTP from the Keychain),
  serializing codes across hosts that share a Duo secret so you never replay a
  code.
- **Adopts live masters** across daemon restarts / app updates — zero re-login.
- **SLURM port forwarding**: pick a running job's compute node from a live
  `squeue` list and forward a local port to it (`ssh -N -J … -L …`), with
  staleness detection when the job ends.
- Recovers automatically after **sleep/wake** and network changes.
- Optional **SSHFS mount** of a host's filesystem.

Components:

| Piece | What |
|-------|------|
| `Auto2FA.app` | SwiftUI menu-bar app (the UI). |
| `a2fa-daemon` | Rust background daemon (the engine). Runs under a per-user LaunchAgent. |
| `a2fa` / `a2fa-tui` | Rust CLI and terminal UI (optional, talk to the same daemon). |

The app and daemon talk over a unix-socket JSON-RPC at `~/.auto2fa/auto2fa.sock`.
(The original Python implementation has been fully rewritten in Rust; the
`auto2fa/` Python tree is kept only as a historical reference.)

## Requirements

- **macOS 13+** (Apple Silicon; a universal build incl. Intel is available — see
  [docs/RELEASE.md](docs/RELEASE.md)).
- An **`~/.ssh/config`** with host aliases for the machines you connect to
  (the app refers to hosts by their ssh alias).
- A host that uses **keyboard-interactive 2FA** (Duo / TOTP) — you supply the
  password and the `otpauth://` secret once via the Add-Host wizard.
- **macFUSE + sshfs** (optional) only if you use the filesystem-mount feature.

## Install

**From a release (recommended):** download `Auto2FA.dmg` from
[Releases](https://github.com/gasvn/auto2fa/releases), drag `Auto2FA.app` to
`/Applications`, and launch it. On first run the app installs the bundled
daemon to `~/.auto2fa/` and registers the `com.auto2fa.daemon` LaunchAgent for
you — nothing else to set up. Then use **Add Host** to register a host's
credentials (stored in the Keychain).

**If the release is notarized** (Developer ID), it just opens. **If it isn't**
(a free, un-notarized build), macOS Gatekeeper blocks it the first time — open
it once via **System Settings → Privacy & Security → "Open Anyway"**, or clear
the quarantine flag yourself:

```sh
xattr -dr com.apple.quarantine /Applications/Auto2FA.app
```

After that it launches normally and the bundled daemon runs in place. (See
[docs/RELEASE.md](docs/RELEASE.md) for why notarization needs a paid Apple
Developer account, and how to build/distribute without one.)

**From source:** see below + [docs/RELEASE.md](docs/RELEASE.md). A self-built
app signed with your own free Apple ID has no Gatekeeper friction at all.

## Build from source

```sh
# Rust daemon / CLI / TUI
cd auto2fa-rs
cargo build --release            # binaries in target/release/
cargo test --workspace -- --test-threads=1

# macOS app — the .xcodeproj is generated from project.yml by XcodeGen
brew install xcodegen          # one time
cd auto2fa-mac && xcodegen generate
xcodebuild -project Auto2FA.xcodeproj \
  -scheme Auto2FA -configuration Release build
```

`source "$HOME/.cargo/env"` first if cargo isn't on your PATH. The `.xcodeproj`
is a generated artifact (not in git) — run `xcodegen generate` after cloning.

To produce a signed, notarized DMG, use
[`auto2fa-mac/package-app.sh`](auto2fa-mac/package-app.sh) — see
[docs/RELEASE.md](docs/RELEASE.md).

## Security

It stores SSH passwords and TOTP secrets in your macOS Keychain and submits the
second factor automatically. **Read [SECURITY.md](SECURITY.md)** for the threat
model and what that trade-off means.

## License

[MIT](LICENSE).
