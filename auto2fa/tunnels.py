"""Tunnel management for auto2fa.

A "tunnel" is a named, persistent two-layer port forward from the local
machine, through a connected jump host (any host in passwords.json), to
a SLURM compute node selected from `squeue`.

See docs/superpowers/specs/2026-05-22-tunnels-design.md for design.
"""
from __future__ import annotations

import logging
from dataclasses import dataclass
from typing import Any, List, Optional

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
