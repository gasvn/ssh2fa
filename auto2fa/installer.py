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
import subprocess
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
