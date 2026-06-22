#!/usr/bin/env python3
"""Bootstrap auto2fa on a fresh clone.

Run with the system Python (no dependencies required):

    python3 install.py

Creates a dedicated .venv next to this file, installs auto2fa into it, then
hands off to `auto2fa install` to generate this machine's deployment artifacts
(LaunchAgent plist + ~/.auto2fa pointer files) and load the daemon at login.
Safe to re-run.
"""
import os
import subprocess
import sys


def main() -> int:
    repo_dir = os.path.dirname(os.path.abspath(__file__))
    venv_dir = os.path.join(repo_dir, ".venv")
    venv_python = os.path.join(venv_dir, "bin", "python")
    auto2fa_bin = os.path.join(venv_dir, "bin", "auto2fa")

    if not os.path.isdir(venv_dir):
        print(f"[bootstrap] creating venv at {venv_dir}")
        r = subprocess.run([sys.executable, "-m", "venv", venv_dir])
        if r.returncode != 0:
            print("[bootstrap] venv creation failed", file=sys.stderr)
            return r.returncode

    print("[bootstrap] upgrading pip")
    subprocess.run([venv_python, "-m", "pip", "install", "--upgrade", "pip"])
    print("[bootstrap] installing auto2fa (editable) into the venv")
    r = subprocess.run([venv_python, "-m", "pip", "install", "-e", repo_dir])
    if r.returncode != 0:
        print("[bootstrap] pip install failed", file=sys.stderr)
        return r.returncode

    print("[bootstrap] generating deployment artifacts")
    return subprocess.run([auto2fa_bin, "install"]).returncode


if __name__ == "__main__":
    sys.exit(main())
