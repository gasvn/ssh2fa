import unittest
from unittest.mock import MagicMock
import unittest.mock
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


class TestNodeDiscoveryDiscover(unittest.TestCase):
    def setUp(self):
        mock_pexpect.reset_mock()
        mock_subprocess.reset_mock()

    def _fake_mgr(self, host="k8", ready=True, active_index=0):
        m = MagicMock()
        m.host = host
        m.is_master_ready.return_value = ready
        m.active_index = active_index
        m.pool_control_paths = {0: f"/tmp/cm-{host}-0", 1: f"/tmp/cm-{host}-1"}
        return m

    def test_discover_invokes_squeue_via_master(self):
        from tunnels import NodeDiscovery
        import tunnels as t
        completed = MagicMock(returncode=0,
                              stdout="14246008|kempner_h|h100x1|RUNNING|23:58:16|holygpu8a11103\n",
                              stderr="")
        with unittest.mock.patch.object(t.subprocess, "run", return_value=completed) as p_run:
            jobs = NodeDiscovery.discover(self._fake_mgr())
            self.assertEqual(len(jobs), 1)
            self.assertEqual(jobs[0].node, "holygpu8a11103")
            # Inspect the command
            args, kwargs = p_run.call_args
            cmd = args[0]
            self.assertEqual(cmd[0], "ssh")
            self.assertIn("-o", cmd)
            self.assertTrue(any("ControlPath=/tmp/cm-k8-0" in a for a in cmd))
            self.assertEqual(cmd[-2], "k8")
            self.assertIn("squeue", cmd[-1])

    def test_discover_raises_on_nonzero_exit(self):
        from tunnels import NodeDiscovery, DiscoveryError
        import tunnels as t
        completed = MagicMock(returncode=1, stdout="", stderr="squeue: command not found")
        with unittest.mock.patch.object(t.subprocess, "run", return_value=completed):
            with self.assertRaises(DiscoveryError) as ctx:
                NodeDiscovery.discover(self._fake_mgr())
            self.assertIn("squeue", str(ctx.exception))


if __name__ == "__main__":
    unittest.main()
