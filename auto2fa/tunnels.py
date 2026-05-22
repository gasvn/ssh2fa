"""Tunnel management for auto2fa.

A "tunnel" is a named, persistent two-layer port forward from the local
machine, through a connected jump host (any host in passwords.json), to
a SLURM compute node selected from `squeue`.

See docs/superpowers/specs/2026-05-22-tunnels-design.md for design.
"""
from __future__ import annotations

import json
import logging
import os
import socket as _socket
import subprocess
import time
from dataclasses import dataclass
from typing import Any, Dict, List, Optional

import pexpect

logger = logging.getLogger(__name__)


class DiscoveryError(Exception):
    """Raised when `squeue` fails on the jump host."""


@dataclass
class Job:
    jobid: str
    partition: str
    name: str
    state: str
    time: str
    node: str


@dataclass
class TunnelState:
    # Persisted fields
    name: str
    local_port: int
    remote_port: int
    jump_candidates: Optional[List[str]]   # None => use every host in passwords.json
    last_node: Optional[str]
    last_user: Optional[str]
    auto_start: bool

    # Runtime-only fields
    status: str = "idle"                   # idle | starting | alive | stale | port_busy | failed
    active_jump: Optional[str] = None
    child: Optional[Any] = None            # pexpect.spawn instance
    last_msg: str = "Ready"
    last_probe_ts: float = 0.0
    consecutive_squeue_misses: int = 0


class NodeDiscovery:
    """Stateless helpers for discovering running SLURM jobs via an SSH master."""

    SQUEUE_FORMAT = "%i|%P|%j|%T|%M|%R"

    @staticmethod
    def parse(stdout: str) -> List[Job]:
        """Parse `squeue -h -o '%i|%P|%j|%T|%M|%R'` output.

        Filters to STATE == RUNNING. Silently skips malformed rows.
        """
        jobs: List[Job] = []
        for line in stdout.splitlines():
            line = line.strip()
            if not line:
                continue
            parts = line.split("|")
            if len(parts) != 6:
                logger.debug("Skipping malformed squeue row: %r", line)
                continue
            jobid, partition, name, state, time_str, node = parts
            if state != "RUNNING":
                continue
            jobs.append(Job(jobid=jobid, partition=partition, name=name,
                            state=state, time=time_str, node=node))
        return jobs

    @staticmethod
    def discover(host_manager) -> List[Job]:
        """Run squeue on the jump host via its existing SSH master.

        Raises DiscoveryError on non-zero exit. The control socket must already
        be live; this never opens a new SSH connection.
        """
        path = host_manager.pool_control_paths[host_manager.active_index]
        cmd = [
            "ssh",
            "-o", f"ControlPath={path}",
            host_manager.host,
            f"squeue -h -o '{NodeDiscovery.SQUEUE_FORMAT}' -u $USER",
        ]
        try:
            result = subprocess.run(cmd, capture_output=True, text=True, timeout=5)
        except subprocess.TimeoutExpired as e:
            raise DiscoveryError(f"squeue timed out on {host_manager.host}") from e
        if result.returncode != 0:
            raise DiscoveryError(
                f"squeue failed on {host_manager.host}: {result.stderr.strip()[:200]}"
            )
        return NodeDiscovery.parse(result.stdout)


class TunnelManager:
    """Owns all tunnel state and lifecycle. Holds read-only refs to the
    existing SSHHostManager instances (provided as a dict)."""

    PERSISTED_FIELDS = ("local_port", "remote_port", "jump_candidates",
                        "last_node", "last_user", "auto_start")

    def __init__(self, host_managers: Dict[str, object], config_path: str):
        self.host_managers = host_managers
        self.config_path = config_path
        self.tunnels: Dict[str, TunnelState] = {}
        self.startup_ts: float = 0.0
        self.auto_started: bool = False

    def load(self) -> None:
        """Load tunnels.json into self.tunnels. Missing file is empty.
        Malformed file is logged and treated as empty (file is NOT overwritten)."""
        if not os.path.exists(self.config_path):
            self.tunnels = {}
            return
        try:
            with open(self.config_path, "r") as f:
                data = json.load(f)
        except (json.JSONDecodeError, OSError) as e:
            logger.error("Failed to load tunnels.json (file kept intact): %s", e)
            self.tunnels = {}
            return

        if not isinstance(data, dict):
            logger.error("tunnels.json root is not an object; treating as empty")
            self.tunnels = {}
            return

        loaded: Dict[str, TunnelState] = {}
        for name, cfg in (data.get("tunnels") or {}).items():
            loaded[name] = TunnelState(
                name=name,
                local_port=int(cfg["local_port"]),
                remote_port=int(cfg.get("remote_port", cfg["local_port"])),
                jump_candidates=cfg.get("jump_candidates"),
                last_node=cfg.get("last_node"),
                last_user=cfg.get("last_user"),
                auto_start=bool(cfg.get("auto_start", False)),
            )
        self.tunnels = loaded

    def save(self) -> None:
        """Atomic write: serialise to tmp file then os.replace."""
        payload = {"tunnels": {}}
        for name, ts in self.tunnels.items():
            payload["tunnels"][name] = {f: getattr(ts, f) for f in self.PERSISTED_FIELDS}

        tmp = self.config_path + ".tmp"
        try:
            with open(tmp, "w") as f:
                json.dump(payload, f, indent=2)
            os.replace(tmp, self.config_path)
        except Exception:
            # Make sure we don't leave a half-written tmp behind
            try:
                if os.path.exists(tmp):
                    os.remove(tmp)
            except OSError:
                pass
            raise

    def add(self, name: str, local_port: int,
            remote_port: Optional[int] = None,
            jump_candidates: Optional[List[str]] = None) -> TunnelState:
        """Validate, register, and persist a new tunnel. Returns the state.

        Raises ValueError on:
          - duplicate name
          - port out of range (must be 1024..65535)
          - port currently in use on 127.0.0.1
        """
        if name in self.tunnels:
            raise ValueError(f"Tunnel '{name}' already exists")
        if not (1024 <= int(local_port) <= 65535):
            raise ValueError(f"Port must be 1024..65535, got {local_port}")
        if remote_port is not None and not (1024 <= int(remote_port) <= 65535):
            raise ValueError(f"remote_port must be 1024..65535, got {remote_port}")
        if not self._port_available(int(local_port)):
            raise ValueError(f"Port {local_port} in use, try another")

        ts = TunnelState(
            name=name,
            local_port=int(local_port),
            remote_port=int(remote_port) if remote_port is not None else int(local_port),
            jump_candidates=jump_candidates,
            last_node=None,
            last_user=None,
            auto_start=False,
        )
        self.tunnels[name] = ts
        self.save()
        return ts

    def remove(self, name: str) -> None:
        """Remove a tunnel. Caller is responsible for stopping it first."""
        if name not in self.tunnels:
            return
        del self.tunnels[name]
        self.save()

    def set_node(self, name: str, node: str, user: str) -> None:
        """Update the saved compute-node target for a tunnel."""
        ts = self.tunnels[name]
        ts.last_node = node
        ts.last_user = user
        # Picking a fresh node clears stale-misses; if it was stale, it can be retried
        ts.consecutive_squeue_misses = 0
        self.save()

    def pick_active_jump(self, ts: TunnelState) -> Optional[str]:
        """Return the name of the first connected jump candidate, or None.

        Defaults to every host in host_managers when ts.jump_candidates is None.
        Unknown candidate names are silently skipped.
        """
        candidates = ts.jump_candidates if ts.jump_candidates is not None \
                     else list(self.host_managers.keys())
        for name in candidates:
            mgr = self.host_managers.get(name)
            if mgr is None:
                continue
            if mgr.is_master_ready():
                return name
        return None

    PROBE_TIMEOUT_SEC = 10.0
    PROBE_INTERVAL_SEC = 0.2

    def start(self, name: str) -> None:
        """Start (or restart) a tunnel. Idempotent: no-op if already alive or starting."""
        ts = self.tunnels[name]

        if ts.status in ("alive", "starting"):
            return

        if ts.last_node is None:
            ts.status = "idle"
            ts.last_msg = "no node — press Enter to pick"
            return

        jump = self.pick_active_jump(ts)
        if jump is None:
            ts.status = "idle"
            ts.last_msg = "waiting for jump"
            return

        if not self._port_available(ts.local_port):
            ts.status = "port_busy"
            ts.last_msg = f"port {ts.local_port} in use"
            return

        ts.status = "starting"
        ts.active_jump = jump
        ts.last_msg = f"starting via {jump}"

        argv = [
            "-N",
            "-J", jump,
            "-L", f"{ts.local_port}:localhost:{ts.remote_port}",
            "-o", "StrictHostKeyChecking=no",
            "-o", "UserKnownHostsFile=/dev/null",
            "-o", "ExitOnForwardFailure=yes",
            "-o", "ServerAliveInterval=15",
            f"{ts.last_user}@{ts.last_node}",
        ]
        child = pexpect.spawn("ssh", argv, encoding="utf-8", timeout=15)
        ts.child = child

        if self._probe_port_ready(ts.local_port, self.PROBE_TIMEOUT_SEC):
            ts.status = "alive"
            ts.last_msg = f"via {jump}"
            ts.consecutive_squeue_misses = 0
        else:
            try:
                child.terminate(force=True)
            except Exception:
                pass
            reason = self._extract_failure_reason(child)
            ts.status = "failed"
            ts.last_msg = reason
            ts.child = None
            ts.active_jump = None

    def _probe_port_ready(self, port: int, timeout: float) -> bool:
        """Poll 127.0.0.1:port until connect() succeeds or timeout."""
        deadline = time.time() + timeout
        while time.time() < deadline:
            s = _socket.socket(_socket.AF_INET, _socket.SOCK_STREAM)
            s.settimeout(0.5)
            try:
                s.connect(("127.0.0.1", port))
                return True
            except OSError:
                pass
            finally:
                s.close()
            time.sleep(self.PROBE_INTERVAL_SEC)
        return False

    @staticmethod
    def _extract_failure_reason(child) -> str:
        """Best-effort short hint from pexpect.child.before / after."""
        text = ""
        for attr in ("before", "after"):
            v = getattr(child, attr, None)
            if isinstance(v, str):
                text += v + " "
        text = text.lower()
        if "permission denied" in text:
            return "auth failed"
        if "host key" in text:
            return "host key verification failed"
        if "no route" in text or "open failed" in text:
            return "node unreachable"
        if "bind:" in text or "forward failed" in text:
            return "remote bind failed"
        return "ssh failed"

    def stop(self, name: str) -> None:
        """Terminate the tunnel's child process and mark idle. Safe if already stopped."""
        ts = self.tunnels[name]
        child = ts.child
        if child is not None:
            try:
                if child.isalive():
                    child.terminate(force=True)
            except Exception:
                pass
        ts.child = None
        ts.active_jump = None
        ts.status = "idle"
        ts.last_msg = "stopped"

    def toggle(self, name: str) -> None:
        """If alive/starting, stop. Otherwise, start."""
        ts = self.tunnels[name]
        if ts.status in ("alive", "starting"):
            self.stop(name)
        else:
            self.start(name)

    DISCOVERY_INTERVAL_SEC = 30.0
    STALE_MISS_THRESHOLD = 2

    def tick(self) -> None:
        """One pass over all tunnels. Cheap for idle states; throttled discovery
        for alive states. Called from the dashboard render loop."""
        now = time.time()
        # Auto-start (one-shot, after a short grace period to let masters come up)
        if not self.auto_started and self.startup_ts and now - self.startup_ts >= 3.0:
            self.auto_started = True
            for name, ts in self.tunnels.items():
                if ts.auto_start and ts.last_node is not None:
                    self.start(name)

        for name, ts in list(self.tunnels.items()):
            if ts.status in ("idle", "stale", "port_busy", "failed", "starting"):
                continue
            # status == "alive"

            # Case 1: child died
            child = ts.child
            if child is None or not child.isalive():
                logger.warning("[tunnel:%s] child died, respawning", name)
                self.start(name)
                continue

            # Case 2: jump master no longer ready → failover
            mgr = self.host_managers.get(ts.active_jump)
            if mgr is None or not mgr.is_master_ready():
                logger.info("[tunnel:%s] jump %s down, failing over", name, ts.active_jump)
                old_jump = ts.active_jump
                self.stop(name)
                self.start(name)
                if ts.active_jump and ts.active_jump != old_jump:
                    ts.last_msg = f"failover {old_jump}→{ts.active_jump}"
                continue

            # Case 3: throttled squeue check
            if now - ts.last_probe_ts < self.DISCOVERY_INTERVAL_SEC:
                continue
            ts.last_probe_ts = now
            try:
                jobs = NodeDiscovery.discover(mgr)
            except DiscoveryError as e:
                logger.warning("[tunnel:%s] discovery error: %s", name, e)
                ts.last_msg = f"squeue err: {str(e)[:30]}"
                continue
            node_alive = any(j.node == ts.last_node for j in jobs)
            if node_alive:
                ts.consecutive_squeue_misses = 0
            else:
                ts.consecutive_squeue_misses += 1
                if ts.consecutive_squeue_misses >= self.STALE_MISS_THRESHOLD:
                    logger.info("[tunnel:%s] node %s gone, marking stale", name, ts.last_node)
                    try:
                        if child.isalive():
                            child.terminate(force=True)
                    except Exception:
                        pass
                    ts.child = None
                    ts.active_jump = None
                    ts.status = "stale"
                    ts.last_msg = f"node {ts.last_node} no longer in squeue"

    def shutdown(self) -> None:
        """Stop every tunnel. Called on dashboard exit."""
        for name in list(self.tunnels.keys()):
            try:
                self.stop(name)
            except Exception as e:
                logger.error("[tunnel:%s] shutdown error: %s", name, e)

    def cleanup_orphans(self) -> None:
        """Reap stray `ssh -N -J ... -L <our_port>:...` processes left from a prior run.

        For each tunnel, pgrep -f for the unique '-L <port>:localhost:' fragment and
        SIGTERM whatever we find. Called once on dashboard startup.
        """
        for ts in self.tunnels.values():
            pattern = f"-L {ts.local_port}:localhost:"
            try:
                res = subprocess.run(
                    ["pgrep", "-f", pattern],
                    capture_output=True, text=True, timeout=2,
                )
            except Exception as e:
                logger.warning("pgrep failed: %s", e)
                continue
            if res.returncode != 0:
                continue
            for pid_str in res.stdout.split():
                try:
                    pid = int(pid_str)
                except ValueError:
                    continue
                try:
                    os.kill(pid, 15)
                except OSError:
                    pass

    @staticmethod
    def _port_available(port: int) -> bool:
        """True iff we can bind 127.0.0.1:port right now."""
        s = _socket.socket(_socket.AF_INET, _socket.SOCK_STREAM)
        try:
            s.setsockopt(_socket.SOL_SOCKET, _socket.SO_REUSEADDR, 1)
            s.bind(("127.0.0.1", port))
            return True
        except OSError:
            return False
        finally:
            s.close()
