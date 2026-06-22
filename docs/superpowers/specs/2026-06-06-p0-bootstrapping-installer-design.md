# P0: Self-bootstrapping installer

**Status:** Design approved, pending implementation
**Date:** 2026-06-06
**Part of:** the "make auto2fa general" effort (P0 of P0–P3)

## Problem

auto2fa's Python code is already portable — every path goes through
`os.path.expanduser("~/...")`, and there is no hardcoded username or machine
path in the logic. What binds an install to one specific machine lives
entirely in three hand-written deployment artifacts:

- `~/Library/LaunchAgents/com.ssh2fa.daemon.plist` — absolute interpreter and
  project paths
- `~/.ssh2fa/project-dir.txt` — absolute repo path (read by the Mac app)
- `~/.ssh2fa/python-path.txt` — absolute venv interpreter (read by the Mac app)

Nothing generates these from the environment. Moving to a new Mac, reinstalling,
or handing the project to someone else means editing absolute paths by hand —
and the repo's checked-in `LaunchAgents/com.ssh2fa.daemon.plist` template is
stale (it points at `/usr/local/bin/ssh2fa-daemon`, which does not match the
venv-based layout the daemon actually uses). The recurring "data vanished after
reboot" failures were a symptom of this gap: the code assumes a generic per-user
environment, but the boot/deploy wiring was implicitly tied to one interactive
shell's PATH and venv location.

## Goal

One command on a fresh clone, on any of the user's Macs, produces a fully
self-contained, login-auto-starting install with **zero manual path editing**.
Re-runnable (idempotent). This is also the foundation for sharing (P1) and Linux
support (P3): the artifact-generation logic is written once, in Python, behind a
per-OS dispatch seam.

Non-goals for P0 (explicitly deferred):
- No changes to the Swift Mac app (it keeps reading the two pointer files).
- No unified `~/.ssh2fa/config.toml` (later cross-cutting work).
- No Slurm decoupling (P2).
- No Linux service generation yet (P3 fills the seam P0 leaves).
- No `uninstall` / `doctor` subcommands (out of chosen scope).

## Architecture

Two layers, so the chicken-and-egg problem (auto2fa isn't installed yet, so we
can't run `auto2fa install`) is solved by a thin stdlib-only bootstrap that
hands off to richer, testable, package-resident logic.

```
install.py            (repo root; stdlib-only; runs under the system python3)
   1. locate repo dir (its own __file__ location); use the python3 that is
      running install.py (sys.executable) as the venv base
   2. create <repo>/.venv if absent  (python3 -m venv)
   3. <repo>/.venv/bin/pip install -e .   (pulls deps incl. keyring)
   4. exec <repo>/.venv/bin/auto2fa install   (hand off)
   5. print clear next steps / verification result
        │
        ▼
auto2fa/installer.py   (the real logic; invoked as the `auto2fa install` subcommand)
   • detect()          -> resolve repo_dir, venv interpreter (.venv/bin/ssh2fa-daemon
                          + .venv/bin/python), config_dir() (~/.ssh), platform
   • write_pointers()  -> write ~/.ssh2fa/project-dir.txt and
                          ~/.ssh2fa/python-path.txt (the Mac app discovers via these)
   • render_service()  -> per-OS dispatch:
        macOS: render the LaunchAgent plist from a template with the detected
               venv ssh2fa-daemon path, WorkingDirectory, and SSH_CONFIG_PATH;
               write to ~/Library/LaunchAgents/com.ssh2fa.daemon.plist;
               launchctl bootout (ignore failure) + bootstrap + kickstart -k
        non-macOS: write pointers only, log "service auto-start not yet
               supported on <os> (P3)", exit success (do NOT fail)
   • verify()          -> after load, confirm the socket responds (best-effort)
```

### Where logic lives / why

- `install.py` is intentionally dumb and dependency-free so it runs on a bare
  machine (macOS ships only `/usr/bin/python3`). It does environment setup
  (venv + pip) and nothing platform-clever.
- All artifact generation lives in `auto2fa/installer.py` so it is unit-testable
  and reusable by the P3 Linux branch (and conceivably a future `uninstall`).
- The `auto2fa install` subcommand is added to `auto2fa/cli.py`'s argparse, next
  to the existing subcommands.

### The plist template

The template is an inline string constant in `installer.py` (avoids package-data
/ MANIFEST plumbing so it is always present after `pip install -e .`), with
placeholders for the interpreter path, working directory, and `SSH_CONFIG_PATH`.
The installer renders it with detected values. The stale checked-in `LaunchAgents/com.ssh2fa.daemon.plist`
is removed (or reduced to a documentation pointer) — the installer becomes the
single source of truth so the two can never drift again.

## Data flow

1. User: `git clone … && cd auto2fa && python3 install.py`
2. `install.py` creates `.venv`, `pip install -e .`, runs `.venv/bin/auto2fa install`
3. `installer.detect()` resolves all paths from the live environment
4. `installer.write_pointers()` writes the two `~/.ssh2fa/*.txt` files
5. `installer.render_service()` writes + (re)loads the LaunchAgent
6. `installer.verify()` checks the socket; print "installed, daemon serving N
   hosts" or a clear failure with the log path

## Idempotency

- Re-running overwrites the three artifacts with freshly-detected values.
- An existing LaunchAgent plist is backed up once to
  `com.ssh2fa.daemon.plist.bak-YYYYMMDD` before being overwritten (matching the
  convention already used this session).
- `launchctl bootstrap` returning "already loaded" is handled by always
  `bootout`-ing first (ignoring "no such process").
- `python3 -m venv` is skipped if `.venv` already exists; `pip install -e .` is
  safe to repeat.

## Error handling

| Failure | Behavior |
|---|---|
| `venv` creation fails | print stderr, abort with non-zero exit |
| `pip install` fails | surface pip output, abort |
| `launchctl bootstrap` "already loaded" | bootout first (idempotent), retry |
| non-macOS platform | write pointers, print P3 notice, exit 0 (don't fail) |
| socket not responding after load | warn, point at `/tmp/ssh2fa_daemon.log`, non-fatal |

## Testing

Per project policy, cover new and changed code.

- **Unit (pure functions in `installer.py`):**
  - `render_service()` produces a plist that parses as valid XML / passes
    `plutil -lint`, and contains the detected interpreter path, working dir, and
    `SSH_CONFIG_PATH`.
  - `detect()` returns the expected repo/venv/config paths for a constructed
    layout (use a tmp dir + monkeypatched HOME).
  - `write_pointers()` writes both files with the resolved absolute paths.
  - `subprocess`/`launchctl` calls are mocked; assert the correct argv
    (bootout-then-bootstrap-then-kickstart order, idempotent re-run).
- **Integration (slower, opt-in):**
  - Run the bootstrap end-to-end against a tmp `HOME` and tmp repo copy; assert
    `.venv` is created, the console script exists, and pointers are written.
    Service load is mocked or skipped in CI.

## Success criteria

- Fresh clone on any of the user's Macs: `python3 install.py` →
  login-auto-starting daemon, data present, no manual path edits.
- Idempotent re-run leaves a working install.
- After install: `~/.ssh2fa/ssh2fa.sock` responds and `auto2fa list` lists hosts.
- The stale repo plist template no longer exists to mislead.

## Sets up later phases

- P1 (share to others): the same `installer.py` is what a bundled-in-.app build
  will call; de-personalization removes the `~/logs/auto2fa_dev` default once the
  installer reliably writes `project-dir.txt`.
- P3 (Linux): fill the `render_service()` non-macOS branch with a
  `systemd --user` unit writer + `systemctl --user` load; everything else
  (detect, pointers, venv bootstrap) already works cross-platform.
