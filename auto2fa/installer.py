"""Self-bootstrapping installer for auto2fa.

Generates this machine's deployment artifacts from the live environment so a
fresh clone needs zero manual path editing:

  - ~/.auto2fa/project-dir.txt   (repo path; read by the Mac app)
  - ~/.auto2fa/python-path.txt   (venv interpreter; read by the Mac app)
  - ~/Library/LaunchAgents/com.auto2fa.daemon.plist  (macOS service)

Invoked as `auto2fa install` after install.py has created the venv and
pip-installed the package. The artifact logic is per-OS dispatched so P3 can
add a Linux (systemd) branch without touching anything else.
"""
from __future__ import annotations

import datetime
import os
import platform
import shutil
import socket
import subprocess
import time
from dataclasses import dataclass

from . import credentials

LAUNCHD_LABEL = "com.auto2fa.daemon"


class InstallError(Exception):
    """A deployment step failed in a way the user must act on."""


@dataclass
class InstallPaths:
    repo_dir: str
    venv_dir: str
    venv_bin: str
    python_bin: str
    daemon_bin: str
    config_dir: str    # ~/.auto2fa
    ssh_config: str    # where passwords.json/tunnels.json live (~/.ssh by default)
    plist_path: str    # ~/Library/LaunchAgents/com.auto2fa.daemon.plist


_PLIST_TEMPLATE = """<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{daemon_bin}</string>
    </array>
    <key>WorkingDirectory</key>
    <string>{repo_dir}</string>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
    <key>EnvironmentVariables</key>
    <dict>
        <key>PATH</key>
        <string>{venv_bin}:/usr/bin:/bin:/usr/sbin:/sbin</string>
        <key>SSH_CONFIG_PATH</key>
        <string>{ssh_config}</string>
    </dict>
    <key>StandardOutPath</key>
    <string>/tmp/auto2fa_daemon.log</string>
    <key>StandardErrorPath</key>
    <string>/tmp/auto2fa_daemon.log</string>
    <key>ProcessType</key>
    <string>Background</string>
    <key>ThrottleInterval</key>
    <integer>10</integer>
    <key>ExitTimeOut</key>
    <integer>30</integer>
</dict>
</plist>
"""


def render_plist(paths: "InstallPaths") -> str:
    """Render the LaunchAgent plist for this machine. The daemon is launched
    via the venv's auto2fa-daemon console script (a concrete interpreter, never
    a login shell) and SSH_CONFIG_PATH is pinned to the resolved config dir."""
    return _PLIST_TEMPLATE.format(
        label=LAUNCHD_LABEL,
        daemon_bin=paths.daemon_bin,
        repo_dir=paths.repo_dir,
        venv_bin=paths.venv_bin,
        ssh_config=paths.ssh_config,
    )


def write_pointers(paths: "InstallPaths") -> None:
    """Write the two files the Mac app reads to discover the daemon:
    project-dir.txt (repo) and python-path.txt (interpreter). No trailing
    newline — the Swift side trims whitespace, but keep it exact."""
    os.makedirs(paths.config_dir, exist_ok=True)
    with open(os.path.join(paths.config_dir, "project-dir.txt"), "w") as f:
        f.write(paths.repo_dir)
    with open(os.path.join(paths.config_dir, "python-path.txt"), "w") as f:
        f.write(paths.python_bin)


def _install_launchagent(paths: "InstallPaths", *, _run) -> str:
    os.makedirs(os.path.dirname(paths.plist_path), exist_ok=True)
    # Back up an existing plist once before overwriting (matches the project
    # convention; lets the user revert a bad install).
    if os.path.exists(paths.plist_path):
        stamp = datetime.date.today().strftime("%Y%m%d")
        shutil.copy2(paths.plist_path, f"{paths.plist_path}.bak-{stamp}")
    with open(paths.plist_path, "w") as f:
        f.write(render_plist(paths))

    domain = f"gui/{os.getuid()}"
    target = f"{domain}/{LAUNCHD_LABEL}"
    # Unregister before re-registering.  Use the legacy `unload` form first
    # because `bootout` on macOS 15+ returns 0 but leaves the service in the
    # launchd database, which causes `bootstrap` to fail with error 5.  `unload`
    # reliably removes it.  Ignore all errors (clean-machine or already-unloaded).
    _run(["launchctl", "unload", paths.plist_path], capture_output=True)
    _run(["launchctl", "bootout", target], capture_output=True)
    r = _run(["launchctl", "bootstrap", domain, paths.plist_path],
             capture_output=True, text=True)
    if r.returncode != 0:
        raise InstallError(
            f"launchctl bootstrap failed ({r.returncode}): "
            f"{(r.stderr or '').strip()}")
    _run(["launchctl", "kickstart", "-k", target], capture_output=True)
    return f"LaunchAgent installed at {paths.plist_path} and loaded"


def render_service(paths: "InstallPaths", *, _run=subprocess.run) -> str:
    """Write + load the platform's auto-start service. Returns a human status
    line. Per-OS dispatch so P3 can add the Linux (systemd) branch here."""
    system = platform.system()
    if system == "Darwin":
        return _install_launchagent(paths, _run=_run)
    return (
        f"service auto-start not yet supported on {system} (P3) — pointers "
        f"written; start the daemon manually with `{paths.daemon_bin}`"
    )


def verify(paths: "InstallPaths", *, timeout: float = 10.0) -> str:
    """Best-effort: poll the IPC socket until it accepts a connection."""
    from . import ipc
    deadline = time.time() + timeout
    while time.time() < deadline:
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        try:
            s.connect(ipc.SOCKET_PATH)
            return "daemon socket is responding"
        except OSError:
            time.sleep(0.3)
        finally:
            s.close()
    return "daemon socket not responding yet — check /tmp/auto2fa_daemon.log"


def install() -> int:
    """`auto2fa install`: generate this machine's artifacts and load the
    service. Idempotent. Does NOT require a running daemon."""
    paths = detect()
    write_pointers(paths)
    status = render_service(paths)
    print(f"[auto2fa install] {status}")
    print(f"[auto2fa install] project-dir: {paths.repo_dir}")
    print(f"[auto2fa install] interpreter: {paths.python_bin}")
    print(f"[auto2fa install] config dir:  {paths.ssh_config}")
    if platform.system() == "Darwin":
        print(f"[auto2fa install] {verify(paths)}")
    return 0


def detect() -> InstallPaths:
    """Resolve every path the installer needs from the live environment.
    repo_dir is the parent of the auto2fa package this module lives in."""
    repo_dir = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    venv_dir = os.path.join(repo_dir, ".venv")
    venv_bin = os.path.join(venv_dir, "bin")
    return InstallPaths(
        repo_dir=repo_dir,
        venv_dir=venv_dir,
        venv_bin=venv_bin,
        python_bin=os.path.join(venv_bin, "python"),
        daemon_bin=os.path.join(venv_bin, "auto2fa-daemon"),
        config_dir=os.path.expanduser("~/.auto2fa"),
        ssh_config=credentials.config_dir(),
        plist_path=os.path.expanduser(
            f"~/Library/LaunchAgents/{LAUNCHD_LABEL}.plist"),
    )
