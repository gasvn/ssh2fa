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
        j = Job(jobid="123", partition="login01", name="run", state="RUNNING",
                time="01:00:00", node="gpunode01")
        self.assertEqual(j.jobid, "123")
        self.assertEqual(j.node, "gpunode01")

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
            "14246008|login01_h|h100x1|RUNNING|23:58:16|gpunode8a11103\n"
            "13756572|login01_h|h100x1|RUNNING|1-21:29:48|gpunode8a15203\n"
            "12975569|login01|a100x1|RUNNING|5-16:13:17|gpunode8a19403\n"
        )
        jobs = NodeDiscovery.parse(raw)
        self.assertEqual(len(jobs), 3)
        self.assertEqual(jobs[0].jobid, "14246008")
        self.assertEqual(jobs[0].partition, "login01_h")
        self.assertEqual(jobs[0].name, "h100x1")
        self.assertEqual(jobs[0].state, "RUNNING")
        self.assertEqual(jobs[0].time, "23:58:16")
        self.assertEqual(jobs[0].node, "gpunode8a11103")

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
                              stdout="14246008|login01_h|h100x1|RUNNING|23:58:16|gpunode8a11103\n",
                              stderr="")
        with unittest.mock.patch.object(t.subprocess, "run", return_value=completed) as p_run:
            jobs = NodeDiscovery.discover(self._fake_mgr())
            self.assertEqual(len(jobs), 1)
            self.assertEqual(jobs[0].node, "gpunode8a11103")
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
            jump_candidates=["k1", "k8"], last_node="gpunode01",
            last_user="alice", auto_start=True,
        )
        tm.save()

        tm2 = TunnelManager(host_managers={}, config_path=self.cfg)
        tm2.load()
        loaded = tm2.tunnels["jupyter"]
        self.assertEqual(loaded.local_port, 8888)
        self.assertEqual(loaded.jump_candidates, ["k1", "k8"])
        self.assertEqual(loaded.last_node, "gpunode01")
        self.assertEqual(loaded.last_user, "alice")
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

    def test_load_skips_entry_missing_local_port(self):
        """A single malformed entry must be skipped, not crash load() with a
        KeyError that wipes every other tunnel (regression)."""
        from tunnels import TunnelManager
        with open(self.cfg, "w") as f:
            json.dump({"tunnels": {
                "good": {"local_port": 8090, "remote_port": 8090},
                "broken": {"remote_port": 8888},          # no local_port
            }}, f)
        tm = TunnelManager(host_managers={}, config_path=self.cfg)
        tm.load()  # must NOT raise KeyError
        self.assertIn("good", tm.tunnels)
        self.assertNotIn("broken", tm.tunnels)
        self.assertEqual(tm.tunnels["good"].local_port, 8090)

    def test_load_skips_entry_with_non_int_port(self):
        """A non-integer port raises ValueError inside int(); that entry is
        skipped rather than crashing the whole load."""
        from tunnels import TunnelManager
        with open(self.cfg, "w") as f:
            json.dump({"tunnels": {
                "good": {"local_port": 8090},
                "bad": {"local_port": "not-a-number"},
            }}, f)
        tm = TunnelManager(host_managers={}, config_path=self.cfg)
        tm.load()
        self.assertEqual(set(tm.tunnels.keys()), {"good"})


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
        tm.set_node("a", "gpunode01", "alice")
        self.assertEqual(tm.tunnels["a"].last_node, "gpunode01")
        self.assertEqual(tm.tunnels["a"].last_user, "alice")
        with open(self.cfg) as f:
            data = json.load(f)
        self.assertEqual(data["tunnels"]["a"]["last_node"], "gpunode01")

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
        tm.set_node("x", "gpunode01", "alice")
        tm.start("x")
        self.assertEqual(tm.tunnels["x"].status, "idle")
        self.assertIn("waiting for jump", tm.tunnels["x"].last_msg.lower())

    def test_start_port_busy_marks_port_busy(self):
        from tunnels import TunnelManager
        hm = {"k8": self._mgr(ready=True)}
        tm = TunnelManager(host_managers=hm, config_path=self.cfg)
        port = self._free_port()
        tm.add("x", port)
        tm.set_node("x", "gpunode01", "alice")
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
        tm.set_node("x", "gpunode01", "alice")
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
            self.assertIn("alice@gpunode01", spawn_argv)

        self.assertEqual(tm.tunnels["x"].status, "alive")
        self.assertEqual(tm.tunnels["x"].active_jump, "k8")
        self.assertIn("via k8", tm.tunnels["x"].last_msg)
        self.assertEqual(tm.tunnels["x"].consecutive_squeue_misses, 0)

    def test_start_persists_wants_alive_on_first_success(self):
        """wants_alive used to be set in memory only — daemon restart
        loaded from disk without it, default False, tick refused to
        auto-recover, tunnel sat idle forever. start() must persist
        wants_alive to disk on the first successful connect."""
        from tunnels import TunnelManager
        import tunnels as t, json
        hm = {"k8": self._mgr(ready=True)}
        tm = TunnelManager(host_managers=hm, config_path=self.cfg)
        port = self._free_port()
        tm.add("x", port)
        tm.set_node("x", "gpunode01", "alice")

        child = MagicMock()
        child.isalive.return_value = True
        with unittest.mock.patch.object(t.pexpect, "spawn", return_value=child), \
             unittest.mock.patch.object(tm, "_probe_port_ready", return_value=True):
            tm.start("x")

        # In-memory flag set
        self.assertTrue(tm.tunnels["x"].wants_alive)
        # AND persisted to disk so the next daemon restart picks it up
        with open(self.cfg) as f:
            payload = json.load(f)
        self.assertTrue(
            payload["tunnels"]["x"].get("wants_alive"),
            "wants_alive must be persisted to tunnels.json after first successful start",
        )

    def test_start_probe_timeout_marks_failed(self):
        from tunnels import TunnelManager
        import tunnels as t
        hm = {"k8": self._mgr(ready=True)}
        tm = TunnelManager(host_managers=hm, config_path=self.cfg)
        port = self._free_port()
        tm.add("x", port)
        tm.set_node("x", "gpunode01", "alice")

        child = MagicMock()
        child.isalive.return_value = True
        child.before = "Permission denied (publickey)"
        with unittest.mock.patch.object(t.pexpect, "spawn", return_value=child), \
             unittest.mock.patch.object(tm, "_probe_port_ready", return_value=False):
            tm.start("x")
        self.assertEqual(tm.tunnels["x"].status, "failed")
        child.terminate.assert_called()

    def test_start_probe_raises_terminates_child(self):
        """If _probe_port_ready RAISES (e.g. OSError when out of fds), the
        freshly spawned child must still be terminated and not leak in
        ts.child (regression)."""
        from tunnels import TunnelManager
        import tunnels as t
        hm = {"k8": self._mgr(ready=True)}
        tm = TunnelManager(host_managers=hm, config_path=self.cfg)
        port = self._free_port()
        tm.add("x", port)
        tm.set_node("x", "gpunode01", "alice")

        child = MagicMock()
        child.isalive.return_value = True
        with unittest.mock.patch.object(t.pexpect, "spawn", return_value=child), \
             unittest.mock.patch.object(
                 tm, "_probe_port_ready",
                 side_effect=OSError("[Errno 24] Too many open files")):
            tm.start("x")  # must NOT propagate, must NOT leak the child
        self.assertEqual(tm.tunnels["x"].status, "failed")
        child.terminate.assert_called_with(force=True)
        self.assertIsNone(tm.tunnels["x"].child)

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
        ts.last_node = "node1"; ts.last_user = "alice"
        dead = MagicMock(); dead.isalive.return_value = False
        ts.child = dead
        # tick() must call stop() first to clear status="alive", THEN start() —
        # otherwise start() short-circuits on status check and the dead tunnel
        # is never actually respawned. stop() is called with
        # user_initiated=False so wants_alive survives — without that, a
        # subsequent start() failure (no ready jump) would orphan the tunnel
        # forever instead of letting auto-recovery retry.
        with unittest.mock.patch.object(tm, "start") as p_start, \
             unittest.mock.patch.object(tm, "stop") as p_stop:
            tm.tick()
            p_stop.assert_called_once_with("x", user_initiated=False)
            p_start.assert_called_once_with("x", _auto_recovery=True)

    def test_tick_does_not_failover_on_transient_jump_unready(self):
        """A multiplexed `ssh -L` child keeps working even if the master's
        probe momentarily fails (cooldown, MaxSessions full briefly, etc.).
        tick() must NOT tear down a working tunnel on these blips —
        previously this caused tunnels to spam-cycle into "idle"."""
        from tunnels import TunnelManager
        hm = {"k8": self._mgr(ready=False)}  # master unready but host enabled
        hm["k8"].active = True                # user has the host enabled
        tm = TunnelManager(host_managers=hm, config_path=self.cfg)
        tm.add("x", self._free_port())
        ts = tm.tunnels["x"]
        ts.status = "alive"
        ts.active_jump = "k8"
        ts.last_node = "node1"; ts.last_user = "alice"
        alive = MagicMock(); alive.isalive.return_value = True
        ts.child = alive

        # Mock _port_available -> False (port IS bound by the live ssh -L).
        # Without this, the new ghost-alive defense would fire and ask us
        # to respawn — but the *point* of this test is the transient-master
        # blip path, not the ghost defense.
        with unittest.mock.patch.object(tm, "start") as p_start, \
             unittest.mock.patch.object(tm, "stop") as p_stop, \
             unittest.mock.patch.object(tm, "_port_available", return_value=False):
            tm.tick()
            # Tunnel child is alive + port is bound + host is enabled → leave alone.
            p_stop.assert_not_called()
            p_start.assert_not_called()

    def test_tick_respawns_ghost_alive_tunnel(self):
        """Defense-in-depth: pexpect.isalive() can report True for an
        ssh -L child that has actually exited (we observed this in the
        wild — daemon reported tunnel alive via k6, ps showed no ssh -L
        process, browser saw Connection Refused). If the local forward
        port is not bound, the tunnel is broken regardless of what
        pexpect thinks — respawn it."""
        from tunnels import TunnelManager
        hm = {"k8": self._mgr(ready=True)}
        hm["k8"].active = True
        tm = TunnelManager(host_managers=hm, config_path=self.cfg)
        tm.add("x", self._free_port())
        ts = tm.tunnels["x"]
        ts.status = "alive"
        ts.active_jump = "k8"
        ts.last_node = "node1"; ts.last_user = "alice"
        # pexpect says alive (lying / stale)
        alive_liar = MagicMock(); alive_liar.isalive.return_value = True
        ts.child = alive_liar
        # Port is NOT bound (real-world: ssh -L exited, listener gone)
        with unittest.mock.patch.object(tm, "start") as p_start, \
             unittest.mock.patch.object(tm, "stop") as p_stop, \
             unittest.mock.patch.object(tm, "_port_available", return_value=True):
            tm.tick()
            p_stop.assert_called_once_with("x", user_initiated=False)
            p_start.assert_called_once_with("x", _auto_recovery=True)

    def test_tick_respawns_when_isalive_raises(self):
        """pexpect can raise PtyProcessError (ECHILD etc.) from isalive().
        That must NOT propagate; treat as dead and respawn."""
        from tunnels import TunnelManager
        hm = {"k8": self._mgr(ready=True)}
        hm["k8"].active = True
        tm = TunnelManager(host_managers=hm, config_path=self.cfg)
        tm.add("x", self._free_port())
        ts = tm.tunnels["x"]
        ts.status = "alive"
        ts.active_jump = "k8"
        ts.last_node = "node1"; ts.last_user = "alice"
        bad = MagicMock()
        bad.isalive.side_effect = RuntimeError("ECHILD or similar")
        ts.child = bad
        with unittest.mock.patch.object(tm, "start") as p_start, \
             unittest.mock.patch.object(tm, "stop") as p_stop, \
             unittest.mock.patch.object(tm, "_port_available", return_value=False):
            tm.tick()
            p_stop.assert_called_once_with("x", user_initiated=False)
            p_start.assert_called_once_with("x", _auto_recovery=True)

    def test_tick_stops_tunnel_when_host_disabled(self):
        """User toggled the jump host off → we should stop the tunnel
        instead of leaving it pointing at a host that's about to go away."""
        from tunnels import TunnelManager
        hm = {"k8": self._mgr(ready=False)}
        hm["k8"].active = False               # user disabled the host
        tm = TunnelManager(host_managers=hm, config_path=self.cfg)
        tm.add("x", self._free_port())
        ts = tm.tunnels["x"]
        ts.status = "alive"
        ts.active_jump = "k8"
        ts.last_node = "node1"; ts.last_user = "alice"
        alive = MagicMock(); alive.isalive.return_value = True
        ts.child = alive

        with unittest.mock.patch.object(tm, "start") as p_start, \
             unittest.mock.patch.object(tm, "stop") as p_stop, \
             unittest.mock.patch.object(tm, "_port_available", return_value=False):
            tm.tick()
            p_stop.assert_called_once_with("x")
            p_start.assert_not_called()       # no restart when host is off

    def test_tick_two_squeue_misses_marks_stale(self):
        from tunnels import TunnelManager, Job
        import tunnels as t
        hm = {"k8": self._mgr(ready=True)}
        tm = TunnelManager(host_managers=hm, config_path=self.cfg)
        tm.add("x", self._free_port())
        ts = tm.tunnels["x"]
        ts.status = "alive"
        ts.active_jump = "k8"
        ts.last_node = "node1"; ts.last_user = "alice"
        alive = MagicMock(); alive.isalive.return_value = True
        ts.child = alive
        # Force discovery check by setting last_probe_ts to long ago
        ts.last_probe_ts = 0.0

        with unittest.mock.patch.object(t.NodeDiscovery, "discover",
                                        return_value=[Job("1","p","n","RUNNING","1","other_node")]), \
             unittest.mock.patch.object(tm, "_port_available", return_value=False):
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
        ts.last_node = "node1"; ts.last_user = "alice"
        alive = MagicMock(); alive.isalive.return_value = True
        ts.child = alive
        ts.last_probe_ts = 0.0
        ts.consecutive_squeue_misses = 0

        with unittest.mock.patch.object(t.NodeDiscovery, "discover",
                                        side_effect=DiscoveryError("boom")), \
             unittest.mock.patch.object(tm, "_port_available", return_value=False):
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
        ts.last_node = "node1"; ts.last_user = "alice"
        alive = MagicMock(); alive.isalive.return_value = True
        ts.child = alive
        ts.last_probe_ts = 0.0
        ts.consecutive_squeue_misses = 1

        with unittest.mock.patch.object(t.NodeDiscovery, "discover",
                                        return_value=[Job("1","p","n","RUNNING","1","node1")]), \
             unittest.mock.patch.object(tm, "_port_available", return_value=False):
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
        # cleanup_orphans now does TWO subprocess calls per match: pgrep
        # to find candidates, then ps per-pid to confirm the cmdline
        # starts with ssh AND has -J (our characteristic auto2fa pattern).
        # This prevents killing user's own `ssh -L` tunnels that share a
        # local port. Side-effect: this test now has to mock ps too.
        def _run_side_effect(argv, **kwargs):
            if argv[0] == "pgrep":
                return MagicMock(returncode=0, stdout="12345\n12346\n", stderr="")
            if argv[0] == "ps":
                # Simulate `ps -o args= -p <pid>` for an auto2fa-style ssh
                return MagicMock(returncode=0,
                                 stdout="ssh -N -J k6 -L 8888:localhost:8888 user@node",
                                 stderr="")
            return MagicMock(returncode=1, stdout="", stderr="")

        with unittest.mock.patch.object(t.subprocess, "run", side_effect=_run_side_effect) as p_run, \
             unittest.mock.patch.object(t.os, "kill") as p_kill:
            tm.cleanup_orphans()
        self.assertTrue(p_run.called)
        kill_pids = [args[0] for args, _ in p_kill.call_args_list]
        self.assertIn(12345, kill_pids)
        self.assertIn(12346, kill_pids)

    def test_cleanup_orphans_skips_user_ssh_l_tunnels(self):
        """Bug fix: don't kill user-launched `ssh -L 8888:localhost:5000 host`
        that happens to share a local port. We require -J (jump) to confirm
        it's our process."""
        from tunnels import TunnelManager
        import tunnels as t
        tm = TunnelManager(host_managers={}, config_path=self.cfg)
        tm.add("a", 8888)

        def _run_side_effect(argv, **kwargs):
            if argv[0] == "pgrep":
                return MagicMock(returncode=0, stdout="99999\n", stderr="")
            if argv[0] == "ps":
                # User's own tunnel without -J — must NOT be killed
                return MagicMock(returncode=0,
                                 stdout="ssh -L 8888:localhost:5000 dev",
                                 stderr="")
            return MagicMock(returncode=1, stdout="", stderr="")

        with unittest.mock.patch.object(t.subprocess, "run", side_effect=_run_side_effect), \
             unittest.mock.patch.object(t.os, "kill") as p_kill:
            tm.cleanup_orphans()
        # Critical: did NOT kill the user's PID
        self.assertEqual([args[0] for args, _ in p_kill.call_args_list], [])

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
        self.assertEqual(expand_first_node("gpunode01"), ("gpunode01", False))

    def test_range(self):
        from tunnels import expand_first_node
        self.assertEqual(expand_first_node("gpunode[01-03]"), ("gpunode01", True))

    def test_comma_list(self):
        from tunnels import expand_first_node
        self.assertEqual(expand_first_node("gpunode[01,03,05]"), ("gpunode01", True))

    def test_with_suffix(self):
        from tunnels import expand_first_node
        # e.g., "gpunode[01-03].hpc.example.edu"
        first, is_range = expand_first_node("gpunode[01-03].hpc.example.edu")
        self.assertEqual(first, "gpunode01.hpc.example.edu")
        self.assertTrue(is_range)

    def test_malformed_returns_raw(self):
        from tunnels import expand_first_node
        self.assertEqual(expand_first_node("(Resources)"), ("(Resources)", False))


class TestBugHuntFixes(unittest.TestCase):
    """Regression tests for the multi-bug-hunt fixes M1/M2/M4/M14."""

    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="a2f-tun-fix-")
        self.cfg = os.path.join(self.tmp, "tunnels.json")

    def tearDown(self):
        shutil.rmtree(self.tmp, ignore_errors=True)

    def _free_port(self):
        s = socket.socket()
        s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]
        s.close()
        return port

    def _ready_mgr(self):
        m = MagicMock()
        m.is_master_ready.return_value = True
        m.active = True
        return m

    # --- M1: stale / port_busy must auto-recover when wants_alive -----------
    def test_tick_recovers_stale_tunnel(self):
        from tunnels import TunnelManager
        tm = TunnelManager(host_managers={"k8": self._ready_mgr()}, config_path=self.cfg)
        tm.add("x", self._free_port())
        ts = tm.tunnels["x"]
        ts.status = "stale"; ts.wants_alive = True; ts.last_node = "n1"
        with unittest.mock.patch.object(tm, "start") as p_start:
            tm.tick()
            p_start.assert_called_once_with("x", _auto_recovery=True)

    def test_tick_recovers_port_busy_tunnel(self):
        from tunnels import TunnelManager
        tm = TunnelManager(host_managers={"k8": self._ready_mgr()}, config_path=self.cfg)
        tm.add("x", self._free_port())
        ts = tm.tunnels["x"]
        ts.status = "port_busy"; ts.wants_alive = True; ts.last_node = "n1"
        with unittest.mock.patch.object(tm, "start") as p_start:
            tm.tick()
            p_start.assert_called_once_with("x", _auto_recovery=True)

    # --- M2: auto-recovery must honor a concurrent user stop() --------------
    def test_auto_recovery_start_honors_user_stop(self):
        from tunnels import TunnelManager
        tm = TunnelManager(host_managers={"k8": self._ready_mgr()}, config_path=self.cfg)
        tm.add("x", self._free_port())
        ts = tm.tunnels["x"]
        ts.status = "idle"; ts.wants_alive = False  # user just stopped it
        with unittest.mock.patch.object(tm, "pick_active_jump") as p_jump:
            tm.start("x", _auto_recovery=True)
            p_jump.assert_not_called()
        self.assertEqual(ts.status, "idle")

    def test_normal_start_ignores_wants_alive_flag(self):
        # An explicit (non-auto) start must proceed even if wants_alive is
        # False — that is how the user turns a stopped tunnel back on.
        from tunnels import TunnelManager
        tm = TunnelManager(host_managers={"k8": self._ready_mgr()}, config_path=self.cfg)
        tm.add("x", self._free_port())
        ts = tm.tunnels["x"]
        ts.status = "idle"; ts.wants_alive = False; ts.last_node = "n1"
        with unittest.mock.patch.object(tm, "pick_active_jump", return_value="k8") as p_jump, \
             unittest.mock.patch.object(tm, "_port_available", return_value=True), \
             unittest.mock.patch.object(tm, "_probe_port_ready", return_value=False):
            tm.start("x")  # not auto-recovery
            p_jump.assert_called()  # proceeded past the wants_alive guard

    # --- M4: set_node on an ALIVE tunnel must re-target the forward ---------
    def test_set_node_retargets_alive_tunnel(self):
        from tunnels import TunnelManager
        tm = TunnelManager(host_managers={"k8": self._ready_mgr()}, config_path=self.cfg)
        tm.add("x", self._free_port())
        ts = tm.tunnels["x"]
        ts.status = "alive"; ts.last_node = "oldnode"
        with unittest.mock.patch.object(tm, "stop") as p_stop, \
             unittest.mock.patch.object(tm, "start") as p_start:
            tm.set_node("x", "newnode", "alice")
            p_stop.assert_called_once_with("x", user_initiated=False)
            p_start.assert_called_once_with("x")
        self.assertEqual(ts.last_node, "newnode")

    def test_set_node_same_node_alive_does_not_bounce(self):
        from tunnels import TunnelManager
        tm = TunnelManager(host_managers={"k8": self._ready_mgr()}, config_path=self.cfg)
        tm.add("x", self._free_port())
        ts = tm.tunnels["x"]
        ts.status = "alive"; ts.last_node = "samenode"
        with unittest.mock.patch.object(tm, "stop") as p_stop, \
             unittest.mock.patch.object(tm, "start") as p_start:
            tm.set_node("x", "samenode", "alice")
            p_stop.assert_not_called()
            p_start.assert_not_called()

    # --- M14: save() must fsync the file before the rename ------------------
    def test_save_fsyncs_before_rename(self):
        from tunnels import TunnelManager
        tm = TunnelManager(host_managers={}, config_path=self.cfg)
        with unittest.mock.patch("tunnels.os.fsync") as p_fsync:
            tm.add("x", self._free_port())  # add() calls save()
            self.assertTrue(p_fsync.called, "save() must fsync before rename")
        with open(self.cfg) as f:
            data = json.load(f)
        self.assertIn("x", data["tunnels"])

    # --- M9/M15: rename must migrate state + the per-tunnel lock atomically -
    def test_rename_migrates_state_and_lock(self):
        from tunnels import TunnelManager
        tm = TunnelManager(host_managers={}, config_path=self.cfg)
        tm.add("old", self._free_port())
        old_lock = tm._lock_for("old")
        tm.rename("old", "new")
        self.assertNotIn("old", tm.tunnels)
        self.assertIn("new", tm.tunnels)
        self.assertEqual(tm.tunnels["new"].name, "new")
        # Same lock object carried over so blocked threads stay serialized.
        self.assertIs(tm._lock_for("new"), old_lock)
        self.assertNotIn("old", tm._tunnel_locks)
        # Persisted under the new name.
        tm2 = TunnelManager(host_managers={}, config_path=self.cfg)
        tm2.load()
        self.assertIn("new", tm2.tunnels)
        self.assertNotIn("old", tm2.tunnels)

    def test_rename_noop_on_missing_or_duplicate(self):
        from tunnels import TunnelManager
        tm = TunnelManager(host_managers={}, config_path=self.cfg)
        tm.add("a", self._free_port())
        tm.add("b", self._free_port())
        tm.rename("a", "b")          # duplicate target -> no-op
        self.assertIn("a", tm.tunnels)
        self.assertIn("b", tm.tunnels)
        tm.rename("ghost", "z")      # missing source -> no-op
        self.assertNotIn("z", tm.tunnels)

    # --- L3: update_fields sets under the lock and persists -----------------
    def test_update_fields_sets_and_persists(self):
        from tunnels import TunnelManager
        tm = TunnelManager(host_managers={}, config_path=self.cfg)
        tm.add("x", self._free_port())
        ok = tm.update_fields("x", url_path="/lab", tags=["a"], auto_start=True)
        self.assertTrue(ok)
        self.assertEqual(tm.tunnels["x"].url_path, "/lab")
        self.assertEqual(tm.tunnels["x"].tags, ["a"])
        self.assertTrue(tm.tunnels["x"].auto_start)
        self.assertFalse(tm.update_fields("ghost", url_path="/x"))
        tm2 = TunnelManager(host_managers={}, config_path=self.cfg)
        tm2.load()
        self.assertEqual(tm2.tunnels["x"].url_path, "/lab")


if __name__ == "__main__":
    unittest.main()
