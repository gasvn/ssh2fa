# SSH2FA on Linux

The daemon, CLI and TUI run on Linux. The menu-bar app does not — it is
SwiftUI/AppKit — so on Linux the dashboard is `a2fa-tui`.

Everything else is the same program: one warm `ControlMaster` per host, the
password + TOTP answered for you, SLURM-aware tunnels, and `ssh <alias>` from
any terminal connecting instantly with no 2FA prompt.

## Install

```sh
git clone https://github.com/gasvn/ssh2fa && cd ssh2fa/auto2fa-rs
./scripts/install-linux.sh                # desktop (keyring)
./scripts/install-linux.sh --vault file   # headless server (see below)
```

No root, nothing system-wide. It installs `ssh2fa-daemon`, `a2fa-cli` and
`a2fa-tui` into `~/.local/bin`, plus a **systemd user service** that starts the
daemon at login and restarts it if it dies.

Requirements: a Rust toolchain to build (`rustup`, no root), `ssh`, and
systemd. `sshfs` only if you want the remote-filesystem mounts.

```sh
systemctl --user status ssh2fa-daemon      # is it up
journalctl --user -u ssh2fa-daemon -f      # logs
a2fa-cli list                              # hosts + tunnels
a2fa-tui                                   # dashboard
```

To keep the daemon running after you log out — which is what you want on a
server, so connections stay warm between SSH sessions:

```sh
loginctl enable-linger "$USER"
```

## Where credentials are stored

This is the one decision Linux forces that macOS does not.

| `SSH2FA_VAULT` | store | when |
|---|---|---|
| unset (default) | freedesktop **Secret Service** over D-Bus (gnome-keyring, KWallet, KeePassXC) | desktop session |
| `file` | **owner-only file**, `~/.ssh/credentials.json`, mode `0600` | headless server |
| `secret-service` | Secret Service, and fail if it is missing | when you want it enforced |

Set it on the service:

```sh
systemctl --user edit ssh2fa-daemon    # [Service] / Environment=SSH2FA_VAULT=file
```

The default does **not** silently fall back to the file. If no keyring is
usable the daemon refuses credential operations and says so, because quietly
writing a TOTP seed to disk is not a decision a program should make for you.

### Why a headless server usually needs `--vault file`

`gnome-keyring-daemon` can be running, `org.freedesktop.secrets` can answer a
D-Bus ping, and secrets can still be unusable:

```
Object does not exist at path "/org/freedesktop/secrets/collection/login"
```

The **login keyring is created and unlocked by PAM at a graphical login**. A
box you only ever SSH into never has one, so there is no collection to write to.
SSH2FA detects exactly this case and tells you, rather than failing every
credential read with raw D-Bus text.

### What the file vault does and does not protect

It is `0600`, owner-only, written atomically, and refused outright if the mode
is ever widened (a careless `chmod -R`, a restore that lost its modes).

It is **not encrypted**, and that is deliberate rather than an omission. The
daemon's job is to restore connections by itself after a reboot, before anyone
types anything. A passphrase-encrypted vault cannot do that: either the daemon
blocks until a human attaches, or the passphrase sits next to the ciphertext
and buys nothing.

In this threat model it is a smaller step than it sounds. `~/.ssh/config`
already points at `ControlPath ~/.ssh/cm-ssh2fa-*`, and any process running as
you can ride those live sockets into every host with no password and no 2FA at
all. Anyone who can read your `$HOME` has already won. If that is not
acceptable on a particular machine, run a keyring there instead.

## Differences from macOS

| | macOS | Linux |
|---|---|---|
| UI | menu-bar app (SwiftUI) | `a2fa-tui` |
| service manager | launchd | systemd user unit |
| credentials | login Keychain | Secret Service / file vault |
| unmount | `umount -f` | `fusermount3 -u` (unprivileged) |
| notifications | `osascript` | `notify-send` |
| clipboard | `pbcopy` | `wl-copy` / `xclip` / `xsel` |
| mount table | `/sbin/mount` | `/bin/mount` (+ a ` type <fs>` column) |

Nothing else differs: the ssh argv, the expect loop, the OTP replay guard, the
tunnel engine and the IPC protocol are shared code.

## Restarts keep your connections

The unit sets `KillSignal=SIGKILL` on purpose. A *graceful* stop makes the
daemon tear down every ControlMaster it owns, so `systemctl --user restart`
would drop your warm sessions and force a fresh 2FA login. SIGKILL leaves the
masters running — they are detached mux processes, not children — and the next
start **adopts** them, so a daemon upgrade costs zero re-authentication.

Nothing leaks: the kernel releases the flock, the next start removes the stale
socket, and orphaned tunnel processes are reaped at boot.

## Testing a build without touching anything real

```sh
cargo test --workspace          # unit + integration suite
./scripts/linux-e2e.sh          # full daemon end-to-end, no real server
```

`linux-e2e.sh` starts an isolated daemon (own socket, own config dir, own file
vault) and puts a fake `ssh` on `PATH` that plays a real login dialogue —
password prompt, verification-code prompt, and a TOTP it verifies itself. It
exercises the whole path including the password-only (no 2FA) case, and asserts
that your real `~/.ssh` was never touched.

To verify the live credential store on a machine:

```sh
SSH2FA_ALLOW_DEVELOPMENT_KEYCHAIN=1 \
  cargo test -p a2fa-core --test secret_service -- --ignored --nocapture
```

## Uninstall

```sh
systemctl --user disable --now ssh2fa-daemon
rm ~/.config/systemd/user/ssh2fa-daemon.service
rm -rf ~/.config/systemd/user/ssh2fa-daemon.service.d
rm ~/.local/bin/{ssh2fa-daemon,a2fa-cli,a2fa-tui}
rm -rf ~/.ssh2fa                     # state
rm -f  ~/.ssh/credentials.json       # file vault, if you used one
```
