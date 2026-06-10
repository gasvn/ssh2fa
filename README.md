# Auto2FA

A macOS menu-bar app + background daemon that keeps **SSH ControlMaster** pools
warm to 2FA-protected hosts, auto-answering the **Duo / TOTP** login so you log
in once and stay connected — plus **SLURM-aware port forwarding** to compute
nodes.

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

> Releases are signed with a Developer ID and notarized by Apple, so Gatekeeper
> lets them run. A locally-built `.app` is **not** notarized and macOS will
> block it unless you remove the quarantine attribute yourself.

**From source:** see below + [docs/RELEASE.md](docs/RELEASE.md).

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
