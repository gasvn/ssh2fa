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
from dataclasses import dataclass
from typing import Any, Dict, List, Optional

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
