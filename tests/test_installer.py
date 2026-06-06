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
        self.assertEqual(p.venv_bin, os.path.join(repo, ".venv", "bin"))
        self.assertEqual(p.daemon_bin, os.path.join(repo, ".venv", "bin", "auto2fa-daemon"))
        self.assertEqual(p.config_dir, os.path.expanduser("~/.auto2fa"))
        self.assertTrue(p.plist_path.endswith(
            "Library/LaunchAgents/com.auto2fa.daemon.plist"))
        from auto2fa import credentials
        self.assertEqual(p.ssh_config, credentials.config_dir())
        self.assertTrue(os.path.isabs(p.ssh_config))


if __name__ == "__main__":
    unittest.main()
