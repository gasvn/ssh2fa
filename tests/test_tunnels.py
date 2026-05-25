import json
import os
import shutil
import socket
import sys
import tempfile
import time
import unittest
import unittest.mock
from unittest.mock import MagicMock

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




class TestTunnelManagerPersistence(unittest.TestCase):
    def setUp(self):
        mock_pexpect.reset_mock()
        mock_subprocess.reset_mock()
        self.tmp = tempfile.mkdtemp()
        self.cfg = os.path.join(self.tmp, "tunnels.json")

    def tearDown(self):
        shutil.rmtree(self.tmp)

    def test_load_missing_file_is_empty(self):
        from tunnels import TunnelManager
        tm = TunnelManager(host_managers={}, config_path=self.cfg)
        tm.load()
        self.assertEqual(tm.tunnels, {})

    def test_save_then_load_round_trip(self):
        from tunnels import TunnelManager, TunnelState
        tm = TunnelManager(host_managers={}, config_path=self.cfg)
        tm.tunnels["jupyter"] = TunnelState(
            name="jupyter", local_port=8888, remote_port=8888,
            jump_candidates=["k1", "k8"], last_node="holygpu01",
            last_user="shgao", auto_start=True,
        )
        tm.save()

        tm2 = TunnelManager(host_managers={}, config_path=self.cfg)
        tm2.load()
        loaded = tm2.tunnels["jupyter"]
        self.assertEqual(loaded.local_port, 8888)
        self.assertEqual(loaded.jump_candidates, ["k1", "k8"])
        self.assertEqual(loaded.last_node, "holygpu01")
        self.assertEqual(loaded.last_user, "shgao")
        self.assertTrue(loaded.auto_start)
        # Runtime fields are reset
        self.assertEqual(loaded.status, "idle")
        self.assertIsNone(loaded.active_jump)

    def test_save_is_atomic(self):
        """If os.replace fails mid-write, the original file must be intact."""
        from tunnels import TunnelManager, TunnelState
        tm = TunnelManager(host_managers={}, config_path=self.cfg)
        tm.tunnels["a"] = TunnelState(
            name="a", local_port=1000, remote_port=1000,
            jump_candidates=None, last_node=None, last_user=None, auto_start=False,
        )
        tm.save()
        original = open(self.cfg).read()

        with unittest.mock.patch("os.replace", side_effect=OSError("disk full")):
            tm.tunnels["a"].local_port = 9999
            with self.assertRaises(OSError):
                tm.save()
        # Original untouched
        self.assertEqual(open(self.cfg).read(), original)
        # And no leftover tmp
        self.assertFalse(os.path.exists(self.cfg + ".tmp"))

    def test_load_malformed_does_not_destroy_file(self):
        from tunnels import TunnelManager
        with open(self.cfg, "w") as f:
            f.write("{not valid json")
        tm = TunnelManager(host_managers={}, config_path=self.cfg)
        tm.load()  # Should not raise; should log error
        self.assertEqual(tm.tunnels, {})
        # File untouched
        self.assertEqual(open(self.cfg).read(), "{not valid json")


class TestTunnelManagerAdd(unittest.TestCase):
    def setUp(self):
        mock_pexpect.reset_mock()
        mock_subprocess.reset_mock()
        self.tmp = tempfile.mkdtemp()
        self.cfg = os.path.join(self.tmp, "tunnels.json")

    def tearDown(self):
        shutil.rmtree(self.tmp)

    def _free_port(self):
        s = socket.socket()
        s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]
        s.close()
        return port

    def test_add_persists_to_disk(self):
        from tunnels import TunnelManager
        tm = TunnelManager(host_managers={}, config_path=self.cfg)
        port = self._free_port()
        tm.add(name="jupyter", local_port=port)
        self.assertIn("jupyter", tm.tunnels)
        self.assertEqual(tm.tunnels["jupyter"].local_port, port)
        self.assertEqual(tm.tunnels["jupyter"].remote_port, port)  # defaults to local
        with open(self.cfg) as f:
            data = json.load(f)
        self.assertIn("jupyter", data["tunnels"])

    def test_add_remote_port_override(self):
        from tunnels import TunnelManager
        tm = TunnelManager(host_managers={}, config_path=self.cfg)
        port = self._free_port()
        tm.add(name="x", local_port=port, remote_port=7000)
        self.assertEqual(tm.tunnels["x"].remote_port, 7000)

    def test_add_rejects_duplicate_name(self):
        from tunnels import TunnelManager
        tm = TunnelManager(host_managers={}, config_path=self.cfg)
        tm.add(name="x", local_port=self._free_port())
        with self.assertRaises(ValueError) as ctx:
            tm.add(name="x", local_port=self._free_port())
        self.assertIn("already exists", str(ctx.exception).lower())

    def test_add_rejects_port_out_of_range(self):
        from tunnels import TunnelManager
        tm = TunnelManager(host_managers={}, config_path=self.cfg)
        with self.assertRaises(ValueError):
            tm.add(name="x", local_port=22)        # privileged
        with self.assertRaises(ValueError):
            tm.add(name="x", local_port=70000)     # > 65535

    def test_add_rejects_port_in_use(self):
        from tunnels import TunnelManager
        tm = TunnelManager(host_managers={}, config_path=self.cfg)
        # Hold a port open
        s = socket.socket()
        s.bind(("127.0.0.1", 0))
        s.listen(1)
        held_port = s.getsockname()[1]
        try:
            with self.assertRaises(ValueError) as ctx:
                tm.add(name="x", local_port=held_port)
            self.assertIn("in use", str(ctx.exception).lower())
        finally:
            s.close()

    def test_add_rejects_remote_port_out_of_range(self):
        from tunnels import TunnelManager
        tm = TunnelManager(host_managers={}, config_path=self.cfg)
        with self.assertRaises(ValueError):
            tm.add(name="x", local_port=self._free_port(), remote_port=22)
        with self.assertRaises(ValueError):
            tm.add(name="y", local_port=self._free_port(), remote_port=70000)


class TestTunnelManagerSimpleOps(unittest.TestCase):
    def setUp(self):
        mock_pexpect.reset_mock()
        mock_subprocess.reset_mock()
        self.tmp = tempfile.mkdtemp()
        self.cfg = os.path.join(self.tmp, "tunnels.json")

    def tearDown(self):
        shutil.rmtree(self.tmp)

    def _free_port(self):
        s = socket.socket()
        s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]
        s.close()
        return port

    def _mk_mgr(self, ready=True):
        m = MagicMock()
        m.is_master_ready.return_value = ready
        return m

    def test_remove(self):
        from tunnels import TunnelManager
        tm = TunnelManager(host_managers={}, config_path=self.cfg)
        tm.add("a", self._free_port())
        tm.remove("a")
        self.assertNotIn("a", tm.tunnels)
        with open(self.cfg) as f:
            data = json.load(f)
        self.assertEqual(data["tunnels"], {})

    def test_remove_unknown_is_noop(self):
        from tunnels import TunnelManager
        tm = TunnelManager(host_managers={}, config_path=self.cfg)
        tm.remove("nope")   # should not raise

    def test_set_node_updates_and_persists(self):
        from tunnels import TunnelManager
        tm = TunnelManager(host_managers={}, config_path=self.cfg)
        tm.add("a", self._free_port())
        tm.set_node("a", "holygpu01", "shgao")
        self.assertEqual(tm.tunnels["a"].last_node, "holygpu01")
        self.assertEqual(tm.tunnels["a"].last_user, "shgao")
        with open(self.cfg) as f:
            data = json.load(f)
        self.assertEqual(data["tunnels"]["a"]["last_node"], "holygpu01")

    def test_pick_active_jump_with_explicit_candidates(self):
        from tunnels import TunnelManager, TunnelState
        hm = {"k1": self._mk_mgr(ready=False), "k8": self._mk_mgr(ready=True)}
        tm = TunnelManager(host_managers=hm, config_path=self.cfg)
        ts = TunnelState(name="x", local_port=1, remote_port=1,
                         jump_candidates=["k1", "k8"],
                         last_node=None, last_user=None, auto_start=False)
        self.assertEqual(tm.pick_active_jump(ts), "k8")

    def test_pick_active_jump_returns_none_when_all_down(self):
        from tunnels import TunnelManager, TunnelState
        hm = {"k1": self._mk_mgr(ready=False), "k8": self._mk_mgr(ready=False)}
        tm = TunnelManager(host_managers=hm, config_path=self.cfg)
        ts = TunnelState(name="x", local_port=1, remote_port=1,
                         jump_candidates=["k1", "k8"],
                         last_node=None, last_user=None, auto_start=False)
        self.assertIsNone(tm.pick_active_jump(ts))

    def test_pick_active_jump_defaults_to_all_hosts(self):
        from tunnels import TunnelManager, TunnelState
        hm = {"a": self._mk_mgr(ready=False), "b": self._mk_mgr(ready=True)}
        tm = TunnelManager(host_managers=hm, config_path=self.cfg)
        ts = TunnelState(name="x", local_port=1, remote_port=1,
                         jump_candidates=None,
                         last_node=None, last_user=None, auto_start=False)
        self.assertEqual(tm.pick_active_jump(ts), "b")

    def test_pick_active_jump_skips_unknown_candidate(self):
        from tunnels import TunnelManager, TunnelState
        hm = {"k8": self._mk_mgr(ready=True)}
        tm = TunnelManager(host_managers=hm, config_path=self.cfg)
        ts = TunnelState(name="x", local_port=1, remote_port=1,
                         jump_candidates=["nonexistent", "k8"],
                         last_node=None, last_user=None, auto_start=False)
        self.assertEqual(tm.pick_active_jump(ts), "k8")


class TestTunnelManagerStart(unittest.TestCase):
    def setUp(self):
        mock_pexpect.reset_mock()
        mock_subprocess.reset_mock()
        self.tmp = tempfile.mkdtemp()
        self.cfg = os.path.join(self.tmp, "tunnels.json")

    def tearDown(self):
        shutil.rmtree(self.tmp)

    def _free_port(self):
        s = socket.socket()
        s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]
        s.close()
        return port

    def _mgr(self, ready=True):
        m = MagicMock()
        m.is_master_ready.return_value = ready
        return m

    def test_start_no_node_marks_idle_with_message(self):
        from tunnels import TunnelManager
        hm = {"k8": self._mgr(ready=True)}
        tm = TunnelManager(host_managers=hm, config_path=self.cfg)
        tm.add("x", self._free_port())
        # last_node is None
        tm.start("x")
        self.assertEqual(tm.tunnels["x"].status, "idle")
        self.assertIn("no node", tm.tunnels["x"].last_msg.lower())

    def test_start_no_jump_marks_idle_with_waiting(self):
        from tunnels import TunnelManager
        hm = {"k8": self._mgr(ready=False)}
        tm = TunnelManager(host_managers=hm, config_path=self.cfg)
        tm.add("x", self._free_port())
        tm.set_node("x", "holygpu01", "shgao")
        tm.start("x")
        self.assertEqual(tm.tunnels["x"].status, "idle")
        self.assertIn("waiting for jump", tm.tunnels["x"].last_msg.lower())

    def test_start_port_busy_marks_port_busy(self):
        from tunnels import TunnelManager
        hm = {"k8": self._mgr(ready=True)}
        tm = TunnelManager(host_managers=hm, config_path=self.cfg)
        port = self._free_port()
        tm.add("x", port)
        tm.set_node("x", "holygpu01", "shgao")
        # Hold the port between add() and start()
        s = socket.socket(); s.bind(("127.0.0.1", port)); s.listen(1)
        try:
            tm.start("x")
            self.assertEqual(tm.tunnels["x"].status, "port_busy")
        finally:
            s.close()

    def test_start_happy_path_spawns_and_probes(self):
        from tunnels import TunnelManager
        import tunnels as t
        hm = {"k8": self._mgr(ready=True)}
        tm = TunnelManager(host_managers=hm, config_path=self.cfg)
        port = self._free_port()
        tm.add("x", port)
        tm.set_node("x", "holygpu01", "shgao")
        tm.tunnels["x"].consecutive_squeue_misses = 5

        child = MagicMock()
        child.isalive.return_value = True
        with unittest.mock.patch.object(t.pexpect, "spawn", return_value=child) as p_spawn, \
             unittest.mock.patch.object(tm, "_probe_port_ready", return_value=True):
            tm.start("x")
            args, kwargs = p_spawn.call_args
            self.assertEqual(args[0], "ssh")
            spawn_argv = args[1]
            self.assertIn("-N", spawn_argv)
            self.assertIn("-J", spawn_argv)
            self.assertIn("k8", spawn_argv)
            self.assertIn("-L", spawn_argv)
            self.assertTrue(any(f"{port}:localhost:{port}" == a for a in spawn_argv))
            self.assertIn("shgao@holygpu01", spawn_argv)

        self.assertEqual(tm.tunnels["x"].status, "alive")
        self.assertEqual(tm.tunnels["x"].active_jump, "k8")
        self.assertIn("via k8", tm.tunnels["x"].last_msg)
        self.assertEqual(tm.tunnels["x"].consecutive_squeue_misses, 0)

    def test_start_probe_timeout_marks_failed(self):
        from tunnels import TunnelManager
        import tunnels as t
        hm = {"k8": self._mgr(ready=True)}
        tm = TunnelManager(host_managers=hm, config_path=self.cfg)
        port = self._free_port()
        tm.add("x", port)
        tm.set_node("x", "holygpu01", "shgao")

        child = MagicMock()
        child.isalive.return_value = True
        child.before = "Permission denied (publickey)"
        with unittest.mock.patch.object(t.pexpect, "spawn", return_value=child), \
             unittest.mock.patch.object(tm, "_probe_port_ready", return_value=False):
            tm.start("x")
        self.assertEqual(tm.tunnels["x"].status, "failed")
        child.terminate.assert_called()

    def test_start_is_noop_when_alive(self):
        from tunnels import TunnelManager
        hm = {"k8": self._mgr(ready=True)}
        tm = TunnelManager(host_managers=hm, config_path=self.cfg)
        tm.add("x", self._free_port())
        tm.tunnels["x"].status = "alive"
        old_active = tm.tunnels["x"].active_jump
        tm.start("x")
        self.assertEqual(tm.tunnels["x"].status, "alive")
        self.assertEqual(tm.tunnels["x"].active_jump, old_active)

    def test_start_is_noop_when_starting(self):
        from tunnels import TunnelManager
        hm = {"k8": self._mgr(ready=True)}
        tm = TunnelManager(host_managers=hm, config_path=self.cfg)
        tm.add("x", self._free_port())
        tm.tunnels["x"].status = "starting"
        # If start didn't no-op, it would try to pick a jump and proceed
        tm.start("x")
        self.assertEqual(tm.tunnels["x"].status, "starting")

    # --- Jump-demotion / cooldown signaling ---

    def test_start_success_clears_jump_cooldown(self):
        """A working port-forward proves the jump's remote TCP is alive →
        mark_remote_ok must fire on the jump's manager."""
        from tunnels import TunnelManager
        import tunnels as t
        mgr = self._mgr(ready=True)
        hm = {"k8": mgr}
        tm = TunnelManager(host_managers=hm, config_path=self.cfg)
        port = self._free_port()
        tm.add("x", port)
        # Wire node directly to avoid set_node's auto-start side effect, which
        # would also exercise (and trip) the failure path we're trying to test.
        tm.tunnels["x"].last_node = "holygpu01"
        tm.tunnels["x"].last_user = "shgao"

        child = MagicMock(); child.isalive.return_value = True
        with unittest.mock.patch.object(t.pexpect, "spawn", return_value=child), \
             unittest.mock.patch.object(tm, "_probe_port_ready", return_value=True):
            tm.start("x")

        mgr.mark_remote_ok.assert_called_once()
        mgr.mark_remote_failure.assert_not_called()

    def test_start_failure_jump_unreachable_demotes_jump(self):
        """ssh stderr matching 'connection refused' classifies as
        'jump unreachable' → mark_remote_failure on the jump."""
        from tunnels import TunnelManager
        import tunnels as t
        mgr = self._mgr(ready=True)
        hm = {"k8": mgr}
        tm = TunnelManager(host_managers=hm, config_path=self.cfg)
        port = self._free_port()
        tm.add("x", port)
        # Wire node directly to avoid set_node's auto-start side effect, which
        # would also exercise (and trip) the failure path we're trying to test.
        tm.tunnels["x"].last_node = "holygpu01"
        tm.tunnels["x"].last_user = "shgao"

        child = MagicMock(); child.isalive.return_value = True
        child.before = "ssh: connect to host k8: Connection refused"
        with unittest.mock.patch.object(t.pexpect, "spawn", return_value=child), \
             unittest.mock.patch.object(tm, "_probe_port_ready", return_value=False):
            tm.start("x")

        self.assertEqual(tm.tunnels["x"].status, "failed")
        mgr.mark_remote_failure.assert_called_once()
        self.assertIn("via k8", tm.tunnels["x"].last_msg)

    def test_start_failure_generic_ssh_failed_still_demotes_jump(self):
        """Generic 'ssh failed' is ambiguous but we conservatively demote so
        we don't keep retrying the same suspicious jump."""
        from tunnels import TunnelManager
        import tunnels as t
        mgr = self._mgr(ready=True)
        hm = {"k8": mgr}
        tm = TunnelManager(host_managers=hm, config_path=self.cfg)
        port = self._free_port()
        tm.add("x", port)
        # Wire node directly to avoid set_node's auto-start side effect, which
        # would also exercise (and trip) the failure path we're trying to test.
        tm.tunnels["x"].last_node = "holygpu01"
        tm.tunnels["x"].last_user = "shgao"

        child = MagicMock(); child.isalive.return_value = True
        child.before = "some unmatched ssh output"
        with unittest.mock.patch.object(t.pexpect, "spawn", return_value=child), \
             unittest.mock.patch.object(tm, "_probe_port_ready", return_value=False):
            tm.start("x")

        mgr.mark_remote_failure.assert_called_once()

    def test_start_failure_node_unreachable_does_not_demote_jump(self):
        """'open failed' = downstream forward (compute node) failed, jump was
        fine. We must NOT demote the jump in that case."""
        from tunnels import TunnelManager
        import tunnels as t
        mgr = self._mgr(ready=True)
        hm = {"k8": mgr}
        tm = TunnelManager(host_managers=hm, config_path=self.cfg)
        port = self._free_port()
        tm.add("x", port)
        # Wire node directly to avoid set_node's auto-start side effect, which
        # would also exercise (and trip) the failure path we're trying to test.
        tm.tunnels["x"].last_node = "holygpu01"
        tm.tunnels["x"].last_user = "shgao"

        child = MagicMock(); child.isalive.return_value = True
        child.before = "channel 0: open failed: connect failed"
        with unittest.mock.patch.object(t.pexpect, "spawn", return_value=child), \
             unittest.mock.patch.object(tm, "_probe_port_ready", return_value=False):
            tm.start("x")

        self.assertEqual(tm.tunnels["x"].status, "failed")
        # "open failed" maps to "node unreachable" — should NOT demote jump
        mgr.mark_remote_failure.assert_not_called()

    def test_extract_failure_reason_classifies_jump_vs_node(self):
        """Round-trip the classifier on representative strings."""
        from tunnels import TunnelManager
        for text, expected in [
            ("ssh: connect to host k8 port 22: Connection refused", "jump unreachable"),
            ("Connection reset by peer", "jump unreachable"),
            ("ssh_exchange_identification: Connection closed by remote host", "jump unreachable"),
            ("Broken pipe", "jump unreachable"),
            ("ssh: connect to host k8 port 22: Operation timed out", "jump unreachable"),
            ("ssh: connect to host: No route to host", "jump unreachable"),
            ("channel 0: open failed: connect failed", "node unreachable"),
            ("Permission denied (publickey)", "auth failed"),
            ("Host key verification failed.", "host key verification failed"),
            ("bind: Address already in use", "remote bind failed"),
            ("forward failed", "remote bind failed"),
            ("totally unrelated noise", "ssh failed"),
        ]:
            child = MagicMock(); child.before = text; child.after = ""
            self.assertEqual(TunnelManager._extract_failure_reason(child), expected,
                             f"misclassified: {text!r}")


class TestTunnelManagerStopToggle(unittest.TestCase):
    def setUp(self):
        mock_pexpect.reset_mock()
        mock_subprocess.reset_mock()
        self.tmp = tempfile.mkdtemp()
        self.cfg = os.path.join(self.tmp, "tunnels.json")

    def tearDown(self):
        shutil.rmtree(self.tmp)

    def _free_port(self):
        s = socket.socket()
        s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]
        s.close()
        return port

    def test_stop_terminates_child_and_sets_idle(self):
        from tunnels import TunnelManager
        tm = TunnelManager(host_managers={}, config_path=self.cfg)
        tm.add("x", self._free_port())
        ts = tm.tunnels["x"]
        ts.status = "alive"
        ts.active_jump = "k8"
        child = MagicMock()
        child.isalive.return_value = True
        ts.child = child

        tm.stop("x")
        child.terminate.assert_called()
        self.assertEqual(ts.status, "idle")
        self.assertIsNone(ts.child)
        self.assertIsNone(ts.active_jump)

    def test_stop_when_no_child_is_safe(self):
        from tunnels import TunnelManager
        tm = TunnelManager(host_managers={}, config_path=self.cfg)
        tm.add("x", self._free_port())
        tm.stop("x")   # no child, no crash
        self.assertEqual(tm.tunnels["x"].status, "idle")

    def test_toggle_idle_calls_start(self):
        from tunnels import TunnelManager
        tm = TunnelManager(host_managers={}, config_path=self.cfg)
        tm.add("x", self._free_port())
        with unittest.mock.patch.object(tm, "start") as p_start, \
             unittest.mock.patch.object(tm, "stop") as p_stop:
            tm.toggle("x")
            p_start.assert_called_once_with("x")
            p_stop.assert_not_called()

    def test_toggle_alive_calls_stop(self):
        from tunnels import TunnelManager
        tm = TunnelManager(host_managers={}, config_path=self.cfg)
        tm.add("x", self._free_port())
        tm.tunnels["x"].status = "alive"
        with unittest.mock.patch.object(tm, "start") as p_start, \
             unittest.mock.patch.object(tm, "stop") as p_stop:
            tm.toggle("x")
            p_stop.assert_called_once_with("x")
            p_start.assert_not_called()


class TestTunnelManagerTick(unittest.TestCase):
    def setUp(self):
        mock_pexpect.reset_mock()
        mock_subprocess.reset_mock()
        self.tmp = tempfile.mkdtemp()
        self.cfg = os.path.join(self.tmp, "tunnels.json")

    def tearDown(self):
        shutil.rmtree(self.tmp)

    def _free_port(self):
        s = socket.socket()
        s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]
        s.close()
        return port

    def _mgr(self, ready=True):
        m = MagicMock()
        m.is_master_ready.return_value = ready
        return m

    def test_tick_skips_idle_and_failed(self):
        from tunnels import TunnelManager
        tm = TunnelManager(host_managers={}, config_path=self.cfg)
        tm.add("x", self._free_port())
        tm.tunnels["x"].status = "idle"
        with unittest.mock.patch.object(tm, "start") as p_start:
            tm.tick()
            p_start.assert_not_called()

    def test_tick_alive_with_dead_child_respawns(self):
        from tunnels import TunnelManager
        hm = {"k8": self._mgr(ready=True)}
        tm = TunnelManager(host_managers=hm, config_path=self.cfg)
        tm.add("x", self._free_port())
        ts = tm.tunnels["x"]
        ts.status = "alive"
        ts.active_jump = "k8"
        ts.last_node = "node1"; ts.last_user = "shgao"
        dead = MagicMock(); dead.isalive.return_value = False
        ts.child = dead
        # tick() must call stop() first to clear status="alive", THEN start() —
        # otherwise start() short-circuits on status check and the dead tunnel
        # is never actually respawned.
        with unittest.mock.patch.object(tm, "start") as p_start, \
             unittest.mock.patch.object(tm, "stop") as p_stop:
            tm.tick()
            p_stop.assert_called_once_with("x")
            p_start.assert_called_once_with("x")

    def test_tick_failover_when_jump_master_gone(self):
        from tunnels import TunnelManager
        hm = {"k8": self._mgr(ready=False), "k1": self._mgr(ready=True)}
        tm = TunnelManager(host_managers=hm, config_path=self.cfg)
        tm.add("x", self._free_port())
        ts = tm.tunnels["x"]
        ts.status = "alive"
        ts.active_jump = "k8"            # was using k8, now down
        ts.last_node = "node1"; ts.last_user = "shgao"
        alive = MagicMock(); alive.isalive.return_value = True
        ts.child = alive
        with unittest.mock.patch.object(tm, "start") as p_start, \
             unittest.mock.patch.object(tm, "stop") as p_stop:
            tm.tick()
            # Should have killed the old child and triggered restart
            p_stop.assert_called_once_with("x")
            p_start.assert_called_once_with("x")

    def test_tick_two_squeue_misses_marks_stale(self):
        from tunnels import TunnelManager, Job
        import tunnels as t
        hm = {"k8": self._mgr(ready=True)}
        tm = TunnelManager(host_managers=hm, config_path=self.cfg)
        tm.add("x", self._free_port())
        ts = tm.tunnels["x"]
        ts.status = "alive"
        ts.active_jump = "k8"
        ts.last_node = "node1"; ts.last_user = "shgao"
        alive = MagicMock(); alive.isalive.return_value = True
        ts.child = alive
        # Force discovery check by setting last_probe_ts to long ago
        ts.last_probe_ts = 0.0

        with unittest.mock.patch.object(t.NodeDiscovery, "discover",
                                        return_value=[Job("1","p","n","RUNNING","1","other_node")]):
            tm.tick()   # miss 1
            self.assertEqual(ts.status, "alive")
            self.assertEqual(ts.consecutive_squeue_misses, 1)
            # Force another check
            ts.last_probe_ts = 0.0
            tm.tick()   # miss 2 → stale
            self.assertEqual(ts.status, "stale")
            alive.terminate.assert_called()

    def test_tick_discovery_error_does_not_bump_misses(self):
        from tunnels import TunnelManager, DiscoveryError
        import tunnels as t
        hm = {"k8": self._mgr(ready=True)}
        tm = TunnelManager(host_managers=hm, config_path=self.cfg)
        tm.add("x", self._free_port())
        ts = tm.tunnels["x"]
        ts.status = "alive"; ts.active_jump = "k8"
        ts.last_node = "node1"; ts.last_user = "shgao"
        alive = MagicMock(); alive.isalive.return_value = True
        ts.child = alive
        ts.last_probe_ts = 0.0
        ts.consecutive_squeue_misses = 0

        with unittest.mock.patch.object(t.NodeDiscovery, "discover",
                                        side_effect=DiscoveryError("boom")):
            tm.tick()
            self.assertEqual(ts.consecutive_squeue_misses, 0)
            self.assertEqual(ts.status, "alive")

    def test_tick_squeue_hit_resets_miss_counter(self):
        from tunnels import TunnelManager, Job
        import tunnels as t
        hm = {"k8": self._mgr(ready=True)}
        tm = TunnelManager(host_managers=hm, config_path=self.cfg)
        tm.add("x", self._free_port())
        ts = tm.tunnels["x"]
        ts.status = "alive"; ts.active_jump = "k8"
        ts.last_node = "node1"; ts.last_user = "shgao"
        alive = MagicMock(); alive.isalive.return_value = True
        ts.child = alive
        ts.last_probe_ts = 0.0
        ts.consecutive_squeue_misses = 1

        with unittest.mock.patch.object(t.NodeDiscovery, "discover",
                                        return_value=[Job("1","p","n","RUNNING","1","node1")]):
            tm.tick()
            self.assertEqual(ts.consecutive_squeue_misses, 0)


class TestOrphansAndShutdown(unittest.TestCase):
    def setUp(self):
        mock_pexpect.reset_mock()
        mock_subprocess.reset_mock()
        self.tmp = tempfile.mkdtemp()
        self.cfg = os.path.join(self.tmp, "tunnels.json")

    def tearDown(self):
        shutil.rmtree(self.tmp)

    def _free_port(self):
        s = socket.socket(); s.bind(("127.0.0.1", 0)); p = s.getsockname()[1]; s.close(); return p

    def test_shutdown_stops_all_tunnels(self):
        from tunnels import TunnelManager
        tm = TunnelManager(host_managers={}, config_path=self.cfg)
        tm.add("a", self._free_port())
        tm.add("b", self._free_port())
        # Give each tunnel a live mock child so shutdown has something to kill
        for name in ("a", "b"):
            child = MagicMock()
            child.isalive.return_value = True
            tm.tunnels[name].child = child
            tm.tunnels[name].status = "alive"
            tm.tunnels[name].active_jump = "k8"
        tm.shutdown()
        for name in ("a", "b"):
            tm.tunnels[name].child  # already nulled
            self.assertIsNone(tm.tunnels[name].child)
            self.assertEqual(tm.tunnels[name].status, "idle")
            self.assertIsNone(tm.tunnels[name].active_jump)

    def test_shutdown_does_not_block_on_held_lock(self):
        """If start() is mid-probe holding the lock, shutdown shouldn't wait."""
        from tunnels import TunnelManager
        import threading as _th
        tm = TunnelManager(host_managers={}, config_path=self.cfg)
        tm.add("a", self._free_port())
        child = MagicMock(); child.isalive.return_value = True
        tm.tunnels["a"].child = child
        # Hold the lock from another thread
        lock = tm._lock_for("a")
        lock.acquire()
        try:
            t0 = time.time()
            tm.shutdown()  # should return promptly, not block 10s
            self.assertLess(time.time() - t0, 2.0)
        finally:
            lock.release()

    def test_cleanup_orphans_pgrep_and_kills(self):
        from tunnels import TunnelManager
        import tunnels as t
        tm = TunnelManager(host_managers={}, config_path=self.cfg)
        tm.add("a", 8888)
        tm.add("b", 8889)
        # pgrep returns two PIDs
        completed = MagicMock(returncode=0, stdout="12345\n12346\n", stderr="")
        with unittest.mock.patch.object(t.subprocess, "run", return_value=completed) as p_run, \
             unittest.mock.patch.object(t.os, "kill") as p_kill:
            tm.cleanup_orphans()
        # We expect at least one pgrep call and kill called for both PIDs
        self.assertTrue(p_run.called)
        kill_pids = [args[0] for args, _ in p_kill.call_args_list]
        self.assertIn(12345, kill_pids)
        self.assertIn(12346, kill_pids)

    def test_cleanup_orphans_no_match_is_noop(self):
        from tunnels import TunnelManager
        import tunnels as t
        tm = TunnelManager(host_managers={}, config_path=self.cfg)
        tm.add("a", 8888)
        completed = MagicMock(returncode=1, stdout="", stderr="")
        with unittest.mock.patch.object(t.subprocess, "run", return_value=completed), \
             unittest.mock.patch.object(t.os, "kill") as p_kill:
            tm.cleanup_orphans()
        p_kill.assert_not_called()


class TestExpandFirstNode(unittest.TestCase):
    def test_single_node(self):
        from tunnels import expand_first_node
        self.assertEqual(expand_first_node("holygpu01"), ("holygpu01", False))

    def test_range(self):
        from tunnels import expand_first_node
        self.assertEqual(expand_first_node("holygpu[01-03]"), ("holygpu01", True))

    def test_comma_list(self):
        from tunnels import expand_first_node
        self.assertEqual(expand_first_node("holygpu[01,03,05]"), ("holygpu01", True))

    def test_with_suffix(self):
        from tunnels import expand_first_node
        # e.g., "holygpu[01-03].rc.fas.harvard.edu"
        first, is_range = expand_first_node("holygpu[01-03].rc.fas.harvard.edu")
        self.assertEqual(first, "holygpu01.rc.fas.harvard.edu")
        self.assertTrue(is_range)

    def test_malformed_returns_raw(self):
        from tunnels import expand_first_node
        self.assertEqual(expand_first_node("(Resources)"), ("(Resources)", False))


if __name__ == "__main__":
    unittest.main()
