"""Single-instance guard for the daemon.

Root cause this protects against: after a reboot the daemon can be started
by *two* independent launchers — the LaunchAgent (RunAtLoad) and the Mac app
(which spawns one when the socket doesn't yet respond). daemon.run() blindly
`os.remove()`s any existing socket and rebinds, so a second daemon clobbers
the first's socket and the two then fight over passwords.json / tunnels.json
/ ssh masters. An exclusive flock on ipc.LOCK_PATH makes the daemon enforce
single-instance itself, so it no longer matters how many launchers fire.
"""
from __future__ import annotations

import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from auto2fa import daemon as daemon_mod  # noqa: E402
from auto2fa import ipc  # noqa: E402


class TestSingletonLock(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp()
        self._old_dir = ipc.SOCKET_DIR
        self._old_lock = ipc.LOCK_PATH
        ipc.SOCKET_DIR = self.tmp
        ipc.LOCK_PATH = os.path.join(self.tmp, "lock")
        self._open = []

    def tearDown(self):
        for f in self._open:
            try:
                f.close()
            except Exception:
                pass
        ipc.SOCKET_DIR = self._old_dir
        ipc.LOCK_PATH = self._old_lock

    def _acquire(self):
        f = daemon_mod._acquire_singleton_lock()
        if f is not None:
            self._open.append(f)
        return f

    def test_first_holder_succeeds(self):
        self.assertIsNotNone(self._acquire())

    def test_second_holder_is_refused_while_first_alive(self):
        first = self._acquire()
        self.assertIsNotNone(first)
        # A second, independent open of the same lock must be refused — this is
        # the would-be second daemon at boot.
        second = self._acquire()
        self.assertIsNone(second)

    def test_lock_is_reusable_after_first_releases(self):
        first = self._acquire()
        self.assertIsNotNone(first)
        first.close()
        self._open.remove(first)
        # Releasing (process exit / crash drops the flock) lets the next
        # daemon start cleanly — no stale-lock wedge.
        self.assertIsNotNone(self._acquire())


if __name__ == "__main__":
    unittest.main()
