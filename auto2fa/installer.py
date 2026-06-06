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
