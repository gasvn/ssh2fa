from __future__ import annotations

import os
import sys
import tempfile
import unittest
import xml.dom.minidom

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from auto2fa import installer  # noqa: E402


class TestDetect(unittest.TestCase):
    def test_paths_are_anchored_to_repo_and_venv(self):
        p = installer.detect()
        repo = os.path.dirname(os.path.dirname(os.path.abspath(installer.__file__)))
        self.assertEqual(p.repo_dir, repo)
        self.assertEqual(p.venv_dir, os.path.join(repo, ".venv"))
        self.assertEqual(p.python_bin, os.path.join(repo, ".venv", "bin", "python"))
        self.assertEqual(p.venv_bin, os.path.join(repo, ".venv", "bin"))
        self.assertEqual(p.daemon_bin, os.path.join(repo, ".venv", "bin", "auto2fa-daemon"))
        self.assertEqual(p.config_dir, os.path.expanduser("~/.auto2fa"))
        self.assertTrue(p.plist_path.endswith(
            "Library/LaunchAgents/com.auto2fa.daemon.plist"))
        from auto2fa import credentials
        self.assertEqual(p.ssh_config, credentials.config_dir())
        self.assertTrue(os.path.isabs(p.ssh_config))


class TestRenderPlist(unittest.TestCase):
    def _paths(self):
        return installer.InstallPaths(
            repo_dir="/Users/x/auto2fa_dev",
            venv_dir="/Users/x/auto2fa_dev/.venv",
            venv_bin="/Users/x/auto2fa_dev/.venv/bin",
            python_bin="/Users/x/auto2fa_dev/.venv/bin/python",
            daemon_bin="/Users/x/auto2fa_dev/.venv/bin/auto2fa-daemon",
            config_dir="/Users/x/.auto2fa",
            ssh_config="/Users/x/.ssh",
            plist_path="/Users/x/Library/LaunchAgents/com.auto2fa.daemon.plist",
        )

    def test_plist_is_valid_xml_with_detected_paths(self):
        xmlstr = installer.render_plist(self._paths())
        xml.dom.minidom.parseString(xmlstr)  # must parse as valid XML
        self.assertIn("/Users/x/auto2fa_dev/.venv/bin/auto2fa-daemon", xmlstr)
        self.assertIn("<string>/Users/x/auto2fa_dev</string>", xmlstr)   # WorkingDirectory
        self.assertIn("/Users/x/.ssh", xmlstr)                           # SSH_CONFIG_PATH
        self.assertIn("com.auto2fa.daemon", xmlstr)
        self.assertIn("/Users/x/auto2fa_dev/.venv/bin:", xmlstr)         # PATH prefix


class TestWritePointers(unittest.TestCase):
    def test_writes_both_pointer_files(self):
        tmp = tempfile.mkdtemp()
        paths = installer.InstallPaths(
            repo_dir="/Users/x/auto2fa_dev",
            venv_dir="/Users/x/auto2fa_dev/.venv",
            venv_bin="/Users/x/auto2fa_dev/.venv/bin",
            python_bin="/Users/x/auto2fa_dev/.venv/bin/python",
            daemon_bin="/Users/x/auto2fa_dev/.venv/bin/auto2fa-daemon",
            config_dir=os.path.join(tmp, ".auto2fa"),
            ssh_config="/Users/x/.ssh",
            plist_path="/ignored",
        )
        installer.write_pointers(paths)
        with open(os.path.join(paths.config_dir, "project-dir.txt")) as f:
            self.assertEqual(f.read(), "/Users/x/auto2fa_dev")
        with open(os.path.join(paths.config_dir, "python-path.txt")) as f:
            self.assertEqual(f.read(), "/Users/x/auto2fa_dev/.venv/bin/python")


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
            python_bin="/r/.venv/bin/python", daemon_bin="/r/.venv/bin/auto2fa-daemon",
            config_dir=os.path.join(tmp, ".auto2fa"), ssh_config="/s",
            plist_path=os.path.join(tmp, "com.auto2fa.daemon.plist"),
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


class TestInstallLaunchAgent(unittest.TestCase):
    def _paths(self, tmp):
        return installer.InstallPaths(
            repo_dir="/r", venv_dir="/r/.venv", venv_bin="/r/.venv/bin",
            python_bin="/r/.venv/bin/python", daemon_bin="/r/.venv/bin/auto2fa-daemon",
            config_dir=os.path.join(tmp, ".auto2fa"), ssh_config="/s",
            plist_path=os.path.join(tmp, "com.auto2fa.daemon.plist"),
        )

    def test_writes_plist_and_loads_in_unload_bootout_bootstrap_kickstart_order(self):
        tmp = tempfile.mkdtemp()
        paths = self._paths(tmp)
        fake = _FakeRun()
        import unittest.mock as mock
        with mock.patch.object(installer.platform, "system", return_value="Darwin"):
            status = installer.render_service(paths, _run=fake)
        self.assertTrue(os.path.exists(paths.plist_path))
        subcmds = [c[1] for c in fake.calls]  # argv[1] is the launchctl verb
        # unload first (legacy, works reliably on macOS 15+), then bootout,
        # then bootstrap + kickstart.
        self.assertEqual(subcmds, ["unload", "bootout", "bootstrap", "kickstart"])
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

    def test_bootstrap_failure_raises_install_error(self):
        tmp = tempfile.mkdtemp()
        paths = self._paths(tmp)
        import unittest.mock as mock
        def fake_run(argv, **kw):
            class _R:
                returncode = 1 if argv[1] == "bootstrap" else 0
                stderr = "Bootstrap failed: 125: Domain does not exist"
                stdout = ""
            return _R()
        with mock.patch.object(installer.platform, "system", return_value="Darwin"):
            with self.assertRaises(installer.InstallError) as cm:
                installer.render_service(paths, _run=fake_run)
        self.assertIn("bootstrap failed", str(cm.exception).lower())
        self.assertIn("125", str(cm.exception))


class TestInstallEntry(unittest.TestCase):
    def test_verify_reports_not_responding_when_socket_absent(self):
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


class TestCliWiring(unittest.TestCase):
    def test_install_subcommand_dispatches_to_installer(self):
        from auto2fa import cli
        import unittest.mock as mock
        with mock.patch.object(cli, "sys") as fake_sys, \
             mock.patch("auto2fa.installer.install", return_value=0) as inst:
            fake_sys.argv = ["auto2fa", "install"]
            cli.main()
        inst.assert_called_once()
        fake_sys.exit.assert_called_once_with(0)


class TestBootstrap(unittest.TestCase):
    def test_creates_venv_installs_and_hands_off(self):
        import importlib, unittest.mock as mock
        boot = importlib.import_module("install")  # repo-root install.py
        recorded = []

        def fake_run(argv, **kw):
            recorded.append(argv)
            class _R:
                returncode = 0
            return _R()

        with mock.patch.object(boot.subprocess, "run", side_effect=fake_run), \
             mock.patch.object(boot.os.path, "isdir", return_value=False):
            rc = boot.main()

        self.assertEqual(rc, 0)
        joined = [" ".join(a) for a in recorded]
        self.assertTrue(any("-m venv" in j for j in joined), joined)
        self.assertTrue(any(("install" in a and "-e" in a) for a in recorded), joined)
        self.assertTrue(any(j.endswith("auto2fa install") for j in joined), joined)


if __name__ == "__main__":
    unittest.main()
