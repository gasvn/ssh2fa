import os
import time
import unittest
from unittest.mock import MagicMock
import sys

mock_pexpect = MagicMock()
mock_subprocess = MagicMock()
sys.modules['pexpect'] = mock_pexpect
sys.modules['subprocess'] = mock_subprocess

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "auto2fa"))
import backend  # noqa: E402 — sys.path mutation above
from backend import SSHHostManager, REMOTE_FAILURE_COOLDOWN


def _ready_mgr():
    """Build a manager that would pass is_master_ready without the cooldown."""
    mgr = SSHHostManager("test_host", "pw", "secret")
    mgr.active = True
    mgr.pool_status = {0: "Ready"}
    mgr.active_index = 0
    return mgr


class TestIsMasterReady(unittest.TestCase):
    def setUp(self):
        mock_pexpect.reset_mock()
        mock_subprocess.reset_mock()
        self.mgr = SSHHostManager("test_host", "pw", "secret")

    def test_false_when_inactive(self):
        self.mgr.active = False
        self.mgr.pool_status = {0: "Ready"}
        self.mgr.active_index = 0
        self.assertFalse(self.mgr.is_master_ready())

    def test_false_when_pool_not_ready(self):
        self.mgr.active = True
        self.mgr.pool_status = {0: "Failed"}
        self.mgr.active_index = 0
        self.assertFalse(self.mgr.is_master_ready())

    def test_false_when_active_index_missing(self):
        self.mgr.active = True
        self.mgr.pool_status = {}
        self.mgr.active_index = 0
        self.assertFalse(self.mgr.is_master_ready())

    def test_true_when_active_and_ready(self):
        self.mgr.active = True
        self.mgr.pool_status = {0: "Ready"}
        self.mgr.active_index = 0
        self.assertTrue(self.mgr.is_master_ready())


class TestRemoteFailureCooldown(unittest.TestCase):
    """Cover the new layer of is_master_ready: a recent observed remote
    failure should suppress the host even if pool_status still says Ready,
    so dead-but-locally-responsive masters don't get picked as jump hosts."""

    def setUp(self):
        mock_pexpect.reset_mock()
        mock_subprocess.reset_mock()

    def test_mark_remote_failure_suppresses_ready(self):
        mgr = _ready_mgr()
        self.assertTrue(mgr.is_master_ready())
        mgr.mark_remote_failure()
        self.assertFalse(mgr.is_master_ready(),
                         "freshly-stamped failure must suppress is_master_ready")

    def test_cooldown_expires_after_window(self):
        mgr = _ready_mgr()
        # Place the failure stamp safely outside the cooldown window.
        mgr.last_remote_failure_ts = time.time() - (REMOTE_FAILURE_COOLDOWN + 5)
        self.assertTrue(mgr.is_master_ready(),
                        "expired cooldown must not suppress is_master_ready")

    def test_mark_remote_ok_clears_failure(self):
        mgr = _ready_mgr()
        mgr.mark_remote_failure()
        self.assertFalse(mgr.is_master_ready())
        mgr.mark_remote_ok()
        self.assertTrue(mgr.is_master_ready(),
                        "mark_remote_ok must lift the cooldown immediately")

    def test_pool_status_required_even_during_cooldown(self):
        """An expired cooldown can't rescue a still-Failed pool_status."""
        mgr = _ready_mgr()
        mgr.pool_status[0] = "Failed"
        mgr.last_remote_failure_ts = time.time() - (REMOTE_FAILURE_COOLDOWN + 5)
        self.assertFalse(mgr.is_master_ready())


class TestDemoteMaster(unittest.TestCase):
    """demote_master tears down a wedged local master so the heartbeat loop
    rebuilds it. It must be safe to call when the pool entry is missing
    or already dead."""

    def setUp(self):
        mock_pexpect.reset_mock()
        mock_subprocess.reset_mock()
        self.mgr = _ready_mgr()

    def test_demote_kills_alive_child_and_marks_dead(self):
        child = MagicMock()
        child.isalive.return_value = True
        self.mgr.pool[0] = child
        self.mgr.pool_status[0] = "Ready"

        self.mgr.demote_master(0)

        child.close.assert_called_once_with(force=True)
        self.assertNotIn(0, self.mgr.pool)
        self.assertEqual(self.mgr.pool_status[0], "Dead")

    def test_demote_handles_missing_pool_entry(self):
        # No pool[0] at all — should be a no-op without raising.
        self.mgr.pool_status[0] = "Ready"
        try:
            self.mgr.demote_master(0)
        except Exception as e:
            self.fail(f"demote_master raised on missing pool entry: {e}")
        self.assertEqual(self.mgr.pool_status[0], "Dead")

    def test_demote_does_not_call_close_on_dead_child(self):
        child = MagicMock()
        child.isalive.return_value = False
        self.mgr.pool[0] = child
        self.mgr.demote_master(0)
        child.close.assert_not_called()


class TestCheckAndRotate(unittest.TestCase):
    """check_and_rotate now classifies remote-probe failures: MaxSessions
    full → rotate symlink, master stays. Anything else → demote + stamp
    cooldown + rotate to other slot if available.

    The global mock_subprocess from sys.modules is shared with sibling test
    files, so this class swaps in a fresh mock for the duration of each test
    and fully restores afterward to avoid leaking state into
    test_pooling_logic.py's call-tracking assertions.
    """

    def setUp(self):
        mock_pexpect.reset_mock()
        # Replace backend's subprocess reference with a private mock so we
        # control its return value and don't disturb mock_subprocess.run
        # call tracking elsewhere.
        self._orig_subprocess = backend.subprocess
        self._fake_subprocess = MagicMock()
        self._fake_subprocess.TimeoutExpired = self._orig_subprocess.TimeoutExpired \
            if hasattr(self._orig_subprocess, "TimeoutExpired") \
            else type("TimeoutExpired", (Exception,), {})
        backend.subprocess = self._fake_subprocess
        self.mgr = _ready_mgr()
        self.mgr.update_symlink = MagicMock()
        self.mgr.demote_master = MagicMock()

    def tearDown(self):
        backend.subprocess = self._orig_subprocess

    def _set_run(self, returncode=0, stderr="", side_effect=None):
        if side_effect is not None:
            self._fake_subprocess.run.side_effect = side_effect
        else:
            self._fake_subprocess.run.return_value = MagicMock(
                returncode=returncode, stderr=stderr
            )

    def test_success_clears_cooldown(self):
        self.mgr.mark_remote_failure()
        self._set_run(returncode=0, stderr="")
        self.mgr.check_and_rotate()
        self.assertEqual(self.mgr.last_remote_failure_ts, 0.0)
        self.mgr.demote_master.assert_not_called()
        self.mgr.update_symlink.assert_not_called()

    def test_timeout_demotes_and_stamps_cooldown(self):
        import subprocess as real_subprocess  # for the exception type
        self._set_run(side_effect=real_subprocess.TimeoutExpired(cmd="ssh", timeout=3))
        self.mgr.check_and_rotate()
        self.assertGreater(self.mgr.last_remote_failure_ts, 0.0)
        self.mgr.demote_master.assert_called_once_with(0)

    def test_maxsessions_full_only_rotates_symlink(self):
        """'administratively prohibited' = server-side MaxSessions cap, master
        TCP is still alive — must NOT demote."""
        self.mgr.pool_status = {0: "Ready", 1: "Ready"}
        self._set_run(returncode=255,
                      stderr="channel 5: open failed: administratively prohibited")
        self.mgr.check_and_rotate()
        self.mgr.demote_master.assert_not_called()
        self.assertEqual(self.mgr.last_remote_failure_ts, 0.0)
        self.mgr.update_symlink.assert_called_once_with(1)

    def test_broken_connection_demotes_and_failovers(self):
        """Non-full failure (broken pipe, refused, etc.) demotes AND failovers."""
        self.mgr.pool_status = {0: "Ready", 1: "Ready"}
        self._set_run(returncode=255,
                      stderr="ssh: connect to host failed: Connection refused")
        self.mgr.check_and_rotate()
        self.mgr.demote_master.assert_called_once_with(0)
        self.assertGreater(self.mgr.last_remote_failure_ts, 0.0)
        self.mgr.update_symlink.assert_called_once_with(1)

    def test_broken_connection_no_other_ready_does_not_rotate(self):
        """If the spare slot isn't Ready, we still demote but don't update_symlink
        to a dead target."""
        self.mgr.pool_status = {0: "Ready", 1: "Failed"}
        self._set_run(returncode=255, stderr="broken pipe")
        self.mgr.check_and_rotate()
        self.mgr.demote_master.assert_called_once_with(0)
        self.mgr.update_symlink.assert_not_called()


if __name__ == "__main__":
    unittest.main()
