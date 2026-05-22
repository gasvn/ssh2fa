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


class TestNodeDiscoveryParse(unittest.TestCase):
    def setUp(self):
        mock_pexpect.reset_mock()
        mock_subprocess.reset_mock()

    def test_parse_canonical(self):
        from tunnels import NodeDiscovery
        # squeue -h -o "%i|%P|%j|%T|%M|%R" -u $USER
        raw = (
            "14246008|kempner_h|h100x1|RUNNING|23:58:16|holygpu8a11103\n"
            "13756572|kempner_h|h100x1|RUNNING|1-21:29:48|holygpu8a15203\n"
            "12975569|kempner|a100x1|RUNNING|5-16:13:17|holygpu8a19403\n"
        )
        jobs = NodeDiscovery.parse(raw)
        self.assertEqual(len(jobs), 3)
        self.assertEqual(jobs[0].jobid, "14246008")
        self.assertEqual(jobs[0].partition, "kempner_h")
        self.assertEqual(jobs[0].name, "h100x1")
        self.assertEqual(jobs[0].state, "RUNNING")
        self.assertEqual(jobs[0].time, "23:58:16")
        self.assertEqual(jobs[0].node, "holygpu8a11103")

    def test_parse_filters_non_running(self):
        from tunnels import NodeDiscovery
        raw = (
            "1|p|n|PENDING|0:00|(Resources)\n"
            "2|p|n|RUNNING|1:00|node1\n"
            "3|p|n|COMPLETED|0:00|node2\n"
        )
        jobs = NodeDiscovery.parse(raw)
        self.assertEqual(len(jobs), 1)
        self.assertEqual(jobs[0].jobid, "2")

    def test_parse_empty(self):
        from tunnels import NodeDiscovery
        self.assertEqual(NodeDiscovery.parse(""), [])
        self.assertEqual(NodeDiscovery.parse("\n\n"), [])

    def test_parse_skips_malformed_rows(self):
        from tunnels import NodeDiscovery
        raw = (
            "1|p|n|RUNNING|1:00|node1\n"
            "this is not a valid line\n"
            "2|p|n|RUNNING|2:00|node2\n"
        )
        jobs = NodeDiscovery.parse(raw)
        self.assertEqual([j.jobid for j in jobs], ["1", "2"])


if __name__ == "__main__":
    unittest.main()
