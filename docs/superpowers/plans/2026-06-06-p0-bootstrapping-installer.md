# P0 Self-Bootstrapping Installer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** One command on a fresh clone (`python3 install.py`) produces a fully self-contained, login-auto-starting auto2fa install on any Mac, with zero manual path editing, idempotently.

**Architecture:** A thin stdlib-only `install.py` at the repo root creates a dedicated `.venv`, `pip install -e .`s the package, then hands off to a new `auto2fa install` subcommand. The real logic lives in `auto2fa/installer.py` (detect paths → write the two `~/.ssh2fa` pointer files the Mac app reads → render+load the LaunchAgent via a per-OS dispatch seam). Non-macOS writes pointers and skips service load without failing (P3 fills the seam).

**Tech Stack:** Python 3.13 (project venv), stdlib only for the installer (`venv`, `subprocess`, `platform`, `xml`), `launchctl`, pytest/unittest.

**Spec:** `docs/superpowers/specs/2026-06-06-p0-bootstrapping-installer-design.md`

---

## File Structure

- Create: `auto2fa/installer.py` — install logic: `InstallError`, `InstallPaths`, `detect()`, `render_plist()`, `write_pointers()`, `render_service()`, `_install_launchagent()`, `verify()`, `install()`.
- Create: `install.py` (repo root) — stdlib bootstrap (venv + pip + hand-off).
- Modify: `auto2fa/cli.py` — add the `install` subcommand.
- Create: `tests/test_installer.py` — unit tests for the installer.
- Delete: `LaunchAgents/com.ssh2fa.daemon.plist` — stale template; installer is now the source of truth.
- Modify: `README.md` — installation section points at `python3 install.py`.

All installer artifact-generation functions take an injectable `_run=subprocess.run` and resolve `platform.system()` at call time, so tests can mock `launchctl` and the OS without spawning anything.

---

### Task 1: installer.py — InstallPaths + detect()

**Files:**
- Create: `auto2fa/installer.py`
- Test: `tests/test_installer.py`

- [ ] **Step 1: Write the failing test**

```python
# tests/test_installer.py
from __future__ import annotations

import os
import sys
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from auto2fa import installer  # noqa: E402


class TestDetect(unittest.TestCase):
    def test_paths_are_anchored_to_repo_and_venv(self):
        p = installer.detect()
        repo = os.path.dirname(os.path.dirname(os.path.abspath(installer.__file__)))
        self.assertEqual(p.repo_dir, repo)
        self.assertEqual(p.venv_dir, os.path.join(repo, ".venv"))
        self.assertEqual(p.python_bin, os.path.join(repo, ".venv", "bin", "python"))
        self.assertEqual(p.daemon_bin, os.path.join(repo, ".venv", "bin", "ssh2fa-daemon"))
        self.assertEqual(p.config_dir, os.path.expanduser("~/.ssh2fa"))
        self.assertTrue(p.plist_path.endswith(
            "Library/LaunchAgents/com.ssh2fa.daemon.plist"))
        # ssh_config is whatever credentials.config_dir() resolves to (abs path)
        self.assertTrue(os.path.isabs(p.ssh_config))


if __name__ == "__main__":
    unittest.main()
```

- [ ] **Step 2: Run test to verify it fails**

Run: `.venv/bin/python -m pytest tests/test_installer.py -q`
Expected: FAIL with `ModuleNotFoundError: No module named 'auto2fa.installer'` (or AttributeError on `detect`).

- [ ] **Step 3: Write minimal implementation**

```python
# auto2fa/installer.py
"""Self-bootstrapping installer for auto2fa.

Generates this machine's deployment artifacts from the live environment so a
fresh clone needs zero manual path editing:

  - ~/.ssh2fa/project-dir.txt   (repo path; read by the Mac app)
  - ~/.ssh2fa/python-path.txt   (venv interpreter; read by the Mac app)
  - ~/Library/LaunchAgents/com.ssh2fa.daemon.plist  (macOS service)

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

LAUNCHD_LABEL = "com.ssh2fa.daemon"


class InstallError(Exception):
    """A deployment step failed in a way the user must act on."""


@dataclass
class InstallPaths:
    repo_dir: str
    venv_dir: str
    venv_bin: str
    python_bin: str
    daemon_bin: str
    config_dir: str    # ~/.ssh2fa
    ssh_config: str    # where passwords.json/tunnels.json live (~/.ssh by default)
    plist_path: str    # ~/Library/LaunchAgents/com.ssh2fa.daemon.plist


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
        daemon_bin=os.path.join(venv_bin, "ssh2fa-daemon"),
        config_dir=os.path.expanduser("~/.ssh2fa"),
        ssh_config=credentials.config_dir(),
        plist_path=os.path.expanduser(
            f"~/Library/LaunchAgents/{LAUNCHD_LABEL}.plist"),
    )
```

- [ ] **Step 4: Run test to verify it passes**

Run: `.venv/bin/python -m pytest tests/test_installer.py -q`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add auto2fa/installer.py tests/test_installer.py
git commit -m "feat: installer detect() + InstallPaths"
```

---

### Task 2: installer.py — render_plist()

**Files:**
- Modify: `auto2fa/installer.py`
- Test: `tests/test_installer.py`

- [ ] **Step 1: Write the failing test**

```python
# add to tests/test_installer.py
import xml.dom.minidom


class TestRenderPlist(unittest.TestCase):
    def _paths(self):
        return installer.InstallPaths(
            repo_dir="/Users/x/auto2fa_dev",
            venv_dir="/Users/x/auto2fa_dev/.venv",
            venv_bin="/Users/x/auto2fa_dev/.venv/bin",
            python_bin="/Users/x/auto2fa_dev/.venv/bin/python",
            daemon_bin="/Users/x/auto2fa_dev/.venv/bin/ssh2fa-daemon",
            config_dir="/Users/x/.ssh2fa",
            ssh_config="/Users/x/.ssh",
            plist_path="/Users/x/Library/LaunchAgents/com.ssh2fa.daemon.plist",
        )

    def test_plist_is_valid_xml_with_detected_paths(self):
        xmlstr = installer.render_plist(self._paths())
        # Parses as XML (catches an unescaped/broken template)
        xml.dom.minidom.parseString(xmlstr)
        self.assertIn("/Users/x/auto2fa_dev/.venv/bin/ssh2fa-daemon", xmlstr)
        self.assertIn("<string>/Users/x/auto2fa_dev</string>", xmlstr)   # WorkingDirectory
        self.assertIn("/Users/x/.ssh", xmlstr)                           # SSH_CONFIG_PATH
        self.assertIn("com.ssh2fa.daemon", xmlstr)
        self.assertIn("/Users/x/auto2fa_dev/.venv/bin:", xmlstr)         # PATH prefix
```

- [ ] **Step 2: Run test to verify it fails**

Run: `.venv/bin/python -m pytest tests/test_installer.py::TestRenderPlist -q`
Expected: FAIL with `AttributeError: module 'auto2fa.installer' has no attribute 'render_plist'`.

- [ ] **Step 3: Write minimal implementation**

Add to `auto2fa/installer.py` (after the imports / before `detect`):

```python
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
    <string>/tmp/ssh2fa_daemon.log</string>
    <key>StandardErrorPath</key>
    <string>/tmp/ssh2fa_daemon.log</string>
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
    via the venv's ssh2fa-daemon console script (a concrete interpreter, never
    a login shell) and SSH_CONFIG_PATH is pinned to the resolved config dir."""
    return _PLIST_TEMPLATE.format(
        label=LAUNCHD_LABEL,
        daemon_bin=paths.daemon_bin,
        repo_dir=paths.repo_dir,
        venv_bin=paths.venv_bin,
        ssh_config=paths.ssh_config,
    )
```

- [ ] **Step 4: Run test to verify it passes**

Run: `.venv/bin/python -m pytest tests/test_installer.py::TestRenderPlist -q`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add auto2fa/installer.py tests/test_installer.py
git commit -m "feat: installer render_plist()"
```

---

### Task 3: installer.py — write_pointers()

**Files:**
- Modify: `auto2fa/installer.py`
- Test: `tests/test_installer.py`

- [ ] **Step 1: Write the failing test**

```python
# add to tests/test_installer.py
import tempfile


class TestWritePointers(unittest.TestCase):
    def test_writes_both_pointer_files(self):
        tmp = tempfile.mkdtemp()
        paths = installer.InstallPaths(
            repo_dir="/Users/x/auto2fa_dev",
            venv_dir="/Users/x/auto2fa_dev/.venv",
            venv_bin="/Users/x/auto2fa_dev/.venv/bin",
            python_bin="/Users/x/auto2fa_dev/.venv/bin/python",
            daemon_bin="/Users/x/auto2fa_dev/.venv/bin/ssh2fa-daemon",
            config_dir=os.path.join(tmp, ".ssh2fa"),
            ssh_config="/Users/x/.ssh",
            plist_path="/ignored",
        )
        installer.write_pointers(paths)
        with open(os.path.join(paths.config_dir, "project-dir.txt")) as f:
            self.assertEqual(f.read(), "/Users/x/auto2fa_dev")
        with open(os.path.join(paths.config_dir, "python-path.txt")) as f:
            self.assertEqual(f.read(), "/Users/x/auto2fa_dev/.venv/bin/python")
```

- [ ] **Step 2: Run test to verify it fails**

Run: `.venv/bin/python -m pytest tests/test_installer.py::TestWritePointers -q`
Expected: FAIL with `AttributeError: ... has no attribute 'write_pointers'`.

- [ ] **Step 3: Write minimal implementation**

Add to `auto2fa/installer.py`:

```python
def write_pointers(paths: "InstallPaths") -> None:
    """Write the two files the Mac app reads to discover the daemon:
    project-dir.txt (repo) and python-path.txt (interpreter). No trailing
    newline — the Swift side trims whitespace, but keep it exact."""
    os.makedirs(paths.config_dir, exist_ok=True)
    with open(os.path.join(paths.config_dir, "project-dir.txt"), "w") as f:
        f.write(paths.repo_dir)
    with open(os.path.join(paths.config_dir, "python-path.txt"), "w") as f:
        f.write(paths.python_bin)
```

- [ ] **Step 4: Run test to verify it passes**

Run: `.venv/bin/python -m pytest tests/test_installer.py::TestWritePointers -q`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add auto2fa/installer.py tests/test_installer.py
git commit -m "feat: installer write_pointers()"
```

---

### Task 4: installer.py — render_service() dispatch + non-macOS no-op

**Files:**
- Modify: `auto2fa/installer.py`
- Test: `tests/test_installer.py`

- [ ] **Step 1: Write the failing test**

```python
# add to tests/test_installer.py
class _FakeRun:
    def __init__(self):
        self.calls = []
    def __call__(self, argv, **kw):
        self.calls.append(argv)
        class _R:
            returncode = 0
            stdout = ""
            stderr = ""
        return _R()


class TestRenderServiceDispatch(unittest.TestCase):
    def _paths(self, tmp):
        return installer.InstallPaths(
            repo_dir="/r", venv_dir="/r/.venv", venv_bin="/r/.venv/bin",
            python_bin="/r/.venv/bin/python", daemon_bin="/r/.venv/bin/ssh2fa-daemon",
            config_dir=os.path.join(tmp, ".ssh2fa"), ssh_config="/s",
            plist_path=os.path.join(tmp, "com.ssh2fa.daemon.plist"),
        )

    def test_non_macos_writes_no_plist_and_does_not_call_launchctl(self):
        tmp = tempfile.mkdtemp()
        paths = self._paths(tmp)
        fake = _FakeRun()
        import unittest.mock as mock
        with mock.patch.object(installer.platform, "system", return_value="Linux"):
            status = installer.render_service(paths, _run=fake)
        self.assertEqual(fake.calls, [])                      # no launchctl
        self.assertFalse(os.path.exists(paths.plist_path))    # no plist on Linux
        self.assertIn("not yet supported", status.lower())
```

- [ ] **Step 2: Run test to verify it fails**

Run: `.venv/bin/python -m pytest tests/test_installer.py::TestRenderServiceDispatch -q`
Expected: FAIL with `AttributeError: ... has no attribute 'render_service'`.

- [ ] **Step 3: Write minimal implementation**

Add to `auto2fa/installer.py`:

```python
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
```

(`_install_launchagent` is added in Task 5; this task only needs the non-macOS path to pass.)

- [ ] **Step 4: Run test to verify it passes**

Run: `.venv/bin/python -m pytest tests/test_installer.py::TestRenderServiceDispatch -q`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add auto2fa/installer.py tests/test_installer.py
git commit -m "feat: installer render_service() per-OS dispatch (Linux seam)"
```

---

### Task 5: installer.py — _install_launchagent() (macOS, idempotent, backup)

**Files:**
- Modify: `auto2fa/installer.py`
- Test: `tests/test_installer.py`

- [ ] **Step 1: Write the failing test**

```python
# add to tests/test_installer.py
class TestInstallLaunchAgent(unittest.TestCase):
    def _paths(self, tmp):
        return installer.InstallPaths(
            repo_dir="/r", venv_dir="/r/.venv", venv_bin="/r/.venv/bin",
            python_bin="/r/.venv/bin/python", daemon_bin="/r/.venv/bin/ssh2fa-daemon",
            config_dir=os.path.join(tmp, ".ssh2fa"), ssh_config="/s",
            plist_path=os.path.join(tmp, "com.ssh2fa.daemon.plist"),
        )

    def test_writes_plist_and_loads_in_bootout_bootstrap_kickstart_order(self):
        tmp = tempfile.mkdtemp()
        paths = self._paths(tmp)
        fake = _FakeRun()
        import unittest.mock as mock
        with mock.patch.object(installer.platform, "system", return_value="Darwin"):
            status = installer.render_service(paths, _run=fake)
        self.assertTrue(os.path.exists(paths.plist_path))
        subcmds = [c[1] for c in fake.calls]  # argv[1] is the launchctl verb
        self.assertEqual(subcmds, ["bootout", "bootstrap", "kickstart"])
        self.assertIn("loaded", status.lower())

    def test_backs_up_existing_plist_once(self):
        tmp = tempfile.mkdtemp()
        paths = self._paths(tmp)
        os.makedirs(os.path.dirname(paths.plist_path), exist_ok=True)
        with open(paths.plist_path, "w") as f:
            f.write("OLD")
        fake = _FakeRun()
        import unittest.mock as mock
        with mock.patch.object(installer.platform, "system", return_value="Darwin"):
            installer.render_service(paths, _run=fake)
        backups = [n for n in os.listdir(os.path.dirname(paths.plist_path))
                   if ".bak-" in n]
        self.assertEqual(len(backups), 1)
        with open(os.path.join(os.path.dirname(paths.plist_path), backups[0])) as f:
            self.assertEqual(f.read(), "OLD")  # backup preserves the old content
```

- [ ] **Step 2: Run test to verify it fails**

Run: `.venv/bin/python -m pytest tests/test_installer.py::TestInstallLaunchAgent -q`
Expected: FAIL with `AttributeError: ... has no attribute '_install_launchagent'`.

- [ ] **Step 3: Write minimal implementation**

Add to `auto2fa/installer.py`:

```python
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
    # bootout first so a re-run isn't rejected with "already loaded"; ignore
    # "no such process" on a clean machine.
    _run(["launchctl", "bootout", target], capture_output=True)
    r = _run(["launchctl", "bootstrap", domain, paths.plist_path],
             capture_output=True, text=True)
    if r.returncode != 0:
        raise InstallError(
            f"launchctl bootstrap failed ({r.returncode}): "
            f"{(r.stderr or '').strip()}")
    _run(["launchctl", "kickstart", "-k", target], capture_output=True)
    return f"LaunchAgent installed at {paths.plist_path} and loaded"
```

- [ ] **Step 4: Run test to verify it passes**

Run: `.venv/bin/python -m pytest tests/test_installer.py::TestInstallLaunchAgent -q`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add auto2fa/installer.py tests/test_installer.py
git commit -m "feat: installer _install_launchagent() — idempotent load + backup"
```

---

### Task 6: installer.py — verify() + install() entry point

**Files:**
- Modify: `auto2fa/installer.py`
- Test: `tests/test_installer.py`

- [ ] **Step 1: Write the failing test**

```python
# add to tests/test_installer.py
class TestInstallEntry(unittest.TestCase):
    def test_verify_reports_not_responding_when_socket_absent(self):
        # Point ipc.SOCKET_PATH at a nonexistent socket; verify must return a
        # message, not hang or raise.
        from auto2fa import ipc
        import unittest.mock as mock
        with mock.patch.object(ipc, "SOCKET_PATH", "/tmp/auto2fa-nope.sock"):
            msg = installer.verify(installer.detect(), timeout=0.3)
        self.assertIn("not responding", msg.lower())

    def test_install_runs_steps_and_returns_zero(self):
        import unittest.mock as mock
        calls = []
        with mock.patch.object(installer, "write_pointers",
                               side_effect=lambda p: calls.append("pointers")), \
             mock.patch.object(installer, "render_service",
                               side_effect=lambda p: calls.append("service") or "ok"), \
             mock.patch.object(installer, "verify",
                               side_effect=lambda p, timeout=10.0: "checked"), \
             mock.patch.object(installer.platform, "system", return_value="Darwin"):
            rc = installer.install()
        self.assertEqual(rc, 0)
        self.assertIn("pointers", calls)
        self.assertIn("service", calls)
```

- [ ] **Step 2: Run test to verify it fails**

Run: `.venv/bin/python -m pytest tests/test_installer.py::TestInstallEntry -q`
Expected: FAIL with `AttributeError: ... has no attribute 'verify'`.

- [ ] **Step 3: Write minimal implementation**

Add to `auto2fa/installer.py` (add `import socket` and `import time` to the imports at the top of the file):

```python
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
    return "daemon socket not responding yet — check /tmp/ssh2fa_daemon.log"


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
```

Add to the top-of-file imports:

```python
import socket
import time
```

- [ ] **Step 4: Run test to verify it passes**

Run: `.venv/bin/python -m pytest tests/test_installer.py::TestInstallEntry -q`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add auto2fa/installer.py tests/test_installer.py
git commit -m "feat: installer verify() + install() entry point"
```

---

### Task 7: cli.py — wire the `install` subcommand

**Files:**
- Modify: `auto2fa/cli.py` (add `cmd_install` near the other `cmd_*` funcs; add a subparser in `main()` after the `raw` subparser at line 215)
- Test: `tests/test_installer.py`

- [ ] **Step 1: Write the failing test**

```python
# add to tests/test_installer.py
class TestCliWiring(unittest.TestCase):
    def test_install_subcommand_dispatches_to_installer(self):
        from auto2fa import cli
        import unittest.mock as mock
        with mock.patch.object(cli, "sys") as fake_sys, \
             mock.patch("auto2fa.installer.install", return_value=0) as inst:
            fake_sys.argv = ["auto2fa", "install"]
            # main() calls args.func(args); cmd_install calls sys.exit(installer.install())
            cli.main()
        inst.assert_called_once()
        fake_sys.exit.assert_called_once_with(0)
```

- [ ] **Step 2: Run test to verify it fails**

Run: `.venv/bin/python -m pytest tests/test_installer.py::TestCliWiring -q`
Expected: FAIL — `argparse` errors on unknown command `install` (SystemExit) or `cmd_install` undefined.

- [ ] **Step 3: Write minimal implementation**

In `auto2fa/cli.py`, add this function alongside the other `cmd_*` functions (e.g. right after `cmd_raw`):

```python
def cmd_install(args):
    from . import installer
    sys.exit(installer.install())
```

In `main()`, immediately after the `raw` subparser block (after line 215, before `args = p.parse_args()`), add:

```python
    sub.add_parser(
        "install",
        help="generate + load this machine's deploy artifacts (no daemon needed)"
    ).set_defaults(func=cmd_install)
```

- [ ] **Step 4: Run test to verify it passes**

Run: `.venv/bin/python -m pytest tests/test_installer.py::TestCliWiring -q`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add auto2fa/cli.py tests/test_installer.py
git commit -m "feat: wire `auto2fa install` subcommand"
```

---

### Task 8: install.py — root bootstrap script

**Files:**
- Create: `install.py` (repo root)
- Test: `tests/test_installer.py`

- [ ] **Step 1: Write the failing test**

```python
# add to tests/test_installer.py
class TestBootstrap(unittest.TestCase):
    def test_creates_venv_installs_and_hands_off(self):
        import importlib, unittest.mock as mock
        boot = importlib.import_module("install")  # repo-root install.py
        repo = os.path.dirname(os.path.dirname(os.path.abspath(installer.__file__)))
        recorded = []

        def fake_run(argv, **kw):
            recorded.append(argv)
            class _R:
                returncode = 0
            return _R()

        # Pretend the venv does not exist yet so the venv-create branch runs.
        with mock.patch.object(boot.subprocess, "run", side_effect=fake_run), \
             mock.patch.object(boot.os.path, "isdir", return_value=False):
            rc = boot.main()

        self.assertEqual(rc, 0)
        # venv creation, pip upgrade, pip install -e ., then hand-off to auto2fa install
        joined = [" ".join(a) for a in recorded]
        self.assertTrue(any("-m venv" in j for j in joined), joined)
        self.assertTrue(any("pip install -e" in j or "install\n-e" in j
                            or ("install" in a and "-e" in a)
                            for a, j in zip(recorded, joined)), joined)
        self.assertTrue(any(j.endswith("auto2fa install") for j in joined), joined)
        self.assertTrue(all(repo in " ".join(a) or "launchctl" not in " ".join(a)
                            for a in recorded))
```

- [ ] **Step 2: Run test to verify it fails**

Run: `.venv/bin/python -m pytest tests/test_installer.py::TestBootstrap -q`
Expected: FAIL with `ModuleNotFoundError: No module named 'install'`.

- [ ] **Step 3: Write minimal implementation**

```python
# install.py  (repo root)
#!/usr/bin/env python3
"""Bootstrap auto2fa on a fresh clone.

Run with the system Python (no dependencies required):

    python3 install.py

Creates a dedicated .venv next to this file, installs auto2fa into it, then
hands off to `auto2fa install` to generate this machine's deployment artifacts
(LaunchAgent plist + ~/.ssh2fa pointer files) and load the daemon at login.
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
```

- [ ] **Step 4: Run test to verify it passes**

Run: `.venv/bin/python -m pytest tests/test_installer.py::TestBootstrap -q`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add install.py tests/test_installer.py
git commit -m "feat: install.py root bootstrap (venv + pip + hand-off)"
```

---

### Task 9: Remove stale template, update README, full-suite + manual end-to-end

**Files:**
- Delete: `LaunchAgents/com.ssh2fa.daemon.plist`
- Modify: `README.md` (Installation section)

- [ ] **Step 1: Remove the stale checked-in plist template**

```bash
git rm LaunchAgents/com.ssh2fa.daemon.plist
```

- [ ] **Step 2: Update README Installation section**

In `README.md`, replace the `## Installation` code block (the `git clone … && pip install -e .` block) with:

````markdown
## Installation

```bash
git clone <repo>
cd auto2fa
python3 install.py        # creates .venv, installs, generates + loads the daemon
```

`install.py` is idempotent — re-run it any time (after moving the repo, switching
machines, or pulling updates) and it regenerates this machine's deployment
artifacts. No manual path editing.
````

Also update the Daemon-mode row in the "Three frontends" table: change
`auto-start via LaunchAgents/com.ssh2fa.daemon.plist` to
`auto-start is configured by python3 install.py`.

- [ ] **Step 3: Run the full test suite**

Run: `.venv/bin/python -m pytest tests/ -q`
Expected: PASS (all prior tests + the new `tests/test_installer.py`).

- [ ] **Step 4: Manual end-to-end on this machine**

Run: `.venv/bin/auto2fa install`
Expected output includes:
- `LaunchAgent installed at /Users/<you>/Library/LaunchAgents/com.ssh2fa.daemon.plist and loaded`
- `daemon socket is responding`

Then verify the daemon is up and serving:

Run: `launchctl list | grep auto2fa && .venv/bin/auto2fa list`
Expected: a running PID (exit code 0) and the host list prints.

(If anything fails, do NOT commit — investigate with `superpowers:systematic-debugging`.)

- [ ] **Step 5: Commit**

```bash
git add README.md
git commit -m "feat: P0 self-bootstrapping installer — python3 install.py, drop stale plist template"
```

---

## Self-Review

**Spec coverage:**
- Two-layer architecture (install.py → installer.py) — Tasks 1-8. ✓
- detect / write_pointers / render_service / verify / install — Tasks 1,3,4,5,6. ✓
- Per-OS dispatch seam + non-macOS no-op — Task 4. ✓
- Idempotent load + plist backup — Task 5. ✓
- Inline plist template, paths from environment — Task 2. ✓
- `auto2fa install` subcommand — Task 7. ✓
- Stale repo template removed (installer = source of truth) — Task 9. ✓
- Tests for render/detect/pointers/dispatch/launchagent — Tasks 1-8. ✓
- README points at `python3 install.py` — Task 9. ✓
- Error handling (pip fail, bootstrap fail, non-macOS) — Tasks 4,5,8. ✓
- Success criteria (manual e2e: socket responds, `auto2fa list`) — Task 9 Step 4. ✓

**Placeholder scan:** No TBD/TODO; every code step contains complete code. ✓

**Type consistency:** `InstallPaths` fields (repo_dir, venv_dir, venv_bin, python_bin, daemon_bin, config_dir, ssh_config, plist_path) are used identically across Tasks 1-6 and tests. `render_service(paths, *, _run=subprocess.run)`, `_install_launchagent(paths, *, _run)`, `verify(paths, *, timeout)`, `install()` signatures consistent across definition (Tasks 4,5,6) and tests (Tasks 4,5,6). `LAUNCHD_LABEL` constant used in detect/render/launchagent. ✓
