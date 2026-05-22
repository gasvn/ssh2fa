import unittest
from unittest.mock import MagicMock
import os
import sys

mock_pexpect = MagicMock()
mock_subprocess = MagicMock()
sys.modules['pexpect'] = mock_pexpect
sys.modules['subprocess'] = mock_subprocess

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "auto2fa"))


class TestDataClasses(unittest.TestCase):
    def setUp(self):
        mock_pexpect.reset_mock()
        mock_subprocess.reset_mock()

    def test_tunnel_state_defaults(self):
        from tunnels import TunnelState
        ts = TunnelState(name="jupyter", local_port=8888, remote_port=8888,
                         jump_candidates=None, last_node=None, last_user=None,
                         auto_start=False)
        self.assertEqual(ts.status, "idle")
        self.assertIsNone(ts.active_jump)
        self.assertIsNone(ts.child)
        self.assertEqual(ts.last_msg, "Ready")
        self.assertEqual(ts.last_probe_ts, 0.0)
        self.assertEqual(ts.consecutive_squeue_misses, 0)

    def test_job_fields(self):
        from tunnels import Job
        j = Job(jobid="123", partition="kempner", name="run", state="RUNNING",
                time="01:00:00", node="holygpu01")
        self.assertEqual(j.jobid, "123")
        self.assertEqual(j.node, "holygpu01")

    def test_discovery_error_is_exception(self):
        from tunnels import DiscoveryError
        self.assertTrue(issubclass(DiscoveryError, Exception))


if __name__ == "__main__":
    unittest.main()
