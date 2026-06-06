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


if __name__ == "__main__":
    unittest.main()
