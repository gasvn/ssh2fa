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
import threading
import time
from dataclasses import dataclass, field
from typing import Any, Dict, List, Optional

import re

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


def expand_first_node(nodelist: str) -> tuple[str, bool]:
    """Given a SLURM NODELIST string like 'holygpu[01-03]' or 'holygpu01',
    return (first_node, is_range).

    - 'holygpu01' → ('holygpu01', False)
    - 'holygpu[01-03]' → ('holygpu01', True)
    - 'holygpu[01,03,05]' → ('holygpu01', True)
    - Anything malformed → (nodelist, False) as a safe fallback.
    """
    m = re.match(r"^([a-zA-Z0-9_.-]+)\[([^\]]+)\](.*)$", nodelist)
    if not m:
        return nodelist, False
    prefix, inside, suffix = m.group(1), m.group(2), m.group(3)
    # Take the first element (split on comma), then split on dash for ranges
    first_chunk = inside.split(",")[0].strip()
    first_num = first_chunk.split("-")[0].strip()
    return f"{prefix}{first_num}{suffix}", True


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
    # Optional shell command to run when this tunnel transitions to "alive".
    # Runs in /bin/sh -c. Environment includes AUTO2FA_TUNNEL_NAME,
    # AUTO2FA_LOCAL_PORT, AUTO2FA_NODE, AUTO2FA_JUMP, AUTO2FA_URL.
    post_connect_cmd: Optional[str] = None
    # User-defined tags for grouping. UI can filter by tag and batch-act
    # ("start all `jupyter`-tagged tunnels"). Free-form strings; lowercased
    # in the UI but stored verbatim.
    tags: List[str] = field(default_factory=list)
    # Optional URL path/query suffix appended when "Open in browser" fires.
    # Use cases: jupyter `?token=xxx`, tensorboard `/#scalars`, etc.
    # Stored verbatim; UI prepends "http://localhost:<port>".
    url_path: Optional[str] = None

    # Runtime-only fields
    status: str = "idle"                   # idle | starting | alive | stale | port_busy | failed
    active_jump: Optional[str] = None
    child: Optional[Any] = None            # pexpect.spawn instance
    last_msg: str = "Ready"
    last_probe_ts: float = 0.0
    consecutive_squeue_misses: int = 0
    # Ring buffer of human-readable activity events. Bounded so a long-
    # running daemon doesn't accumulate megabytes of history per tunnel.
    # Format: list of {"ts": epoch_seconds, "msg": str}.
    events: List[Dict[str, Any]] = field(default_factory=list)
    # Last time this tunnel reached "alive". 0 = never. Used by UI to
    # show "alive 2h" / "last alive 5m ago" — way more useful than the
    # opaque last_msg string for figuring out staleness.
    last_alive_at: float = 0.0
    # Lifetime stats (resets only on daemon restart). UI shows these in
    # the tunnel-details popover.
    total_uptime_sec: float = 0.0       # cumulative time spent in "alive"
    connect_count: int = 0              # successful alive transitions
    fail_count: int = 0                 # failed/stale transitions
    # Internal: when did we last enter alive? Used to accumulate uptime
    # when we leave alive.
    _alive_since: float = 0.0


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
                        "last_node", "last_user", "auto_start",
                        "post_connect_cmd", "tags", "url_path")
    EVENT_BUFFER_LIMIT = 200

    def __init__(self, host_managers: Dict[str, object], config_path: str):
        self.host_managers = host_managers
        self.config_path = config_path
        self.tunnels: Dict[str, TunnelState] = {}
        self.startup_ts: float = 0.0
        self.auto_started: bool = False
        # Per-tunnel locks so concurrent start/stop on DIFFERENT tunnels
        # don't serialise behind each other's 10-second port probes.
        # _locks_meta protects _tunnel_locks itself (creation / cleanup).
        self._tunnel_locks: Dict[str, threading.Lock] = {}
        self._locks_meta = threading.Lock()
        # Serialises save() — without this, two concurrent worker threads
        # could both write to the same .tmp file and produce corrupt JSON.
        self._save_lock = threading.Lock()
        # Track in-flight post_connect threads so a flapping tunnel doesn't
        # spawn duplicate hooks (which would e.g. double-fire webhooks).
        self._post_connect_running: set[str] = set()
        self._post_connect_lock = threading.Lock()
        # Serialise add()'s check-then-insert. Two concurrent tunnel_add IPC
        # calls with the same name could otherwise both pass the duplicate
        # check; the second write would clobber the first's state.
        self._add_lock = threading.Lock()

    def _lock_for(self, name: str) -> threading.Lock:
        """Return (and lazily create) the lock for one tunnel."""
        with self._locks_meta:
            lock = self._tunnel_locks.get(name)
            if lock is None:
                lock = threading.Lock()
                self._tunnel_locks[name] = lock
            return lock

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
                post_connect_cmd=cfg.get("post_connect_cmd"),
                tags=list(cfg.get("tags", []) or []),
                url_path=cfg.get("url_path"),
            )
        self.tunnels = loaded

    def save(self) -> None:
        """Atomic write: serialise to tmp file then os.replace.

        Thread-safe via self._save_lock so concurrent workers don't trash
        each other's writes to the same .tmp path.
        """
        with self._save_lock:
            # Snapshot the dict — another thread may add/remove tunnels mid-loop.
            snapshot = list(self.tunnels.items())
            payload = {"tunnels": {}}
            for name, ts in snapshot:
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
        # Hold _add_lock for the whole check-then-insert so two concurrent
        # tunnel_add IPCs with the same name can't both pass the duplicate
        # check (which would lose one of them on save).
        with self._add_lock:
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
            try:
                self.save()
            except Exception:
                # Roll back the in-memory insertion so the failed add doesn't
                # leave a phantom tunnel visible in the UI but absent from disk.
                self.tunnels.pop(name, None)
                with self._locks_meta:
                    self._tunnel_locks.pop(name, None)
                raise
            return ts

    def remove(self, name: str) -> None:
        """Remove a tunnel. Caller is responsible for stopping it first."""
        if name not in self.tunnels:
            return
        del self.tunnels[name]
        # Clean up the per-tunnel lock so it doesn't linger.
        with self._locks_meta:
            self._tunnel_locks.pop(name, None)
        self.save()

    def set_node(self, name: str, node: str, user: str) -> None:
        """Update the saved compute-node target for a tunnel.

        If the tunnel is idle/stale/failed, automatically (re)starts it.
        Silently returns if the tunnel was removed.
        """
        ts = self.tunnels.get(name)
        if ts is None:
            return
        ts.last_node = node
        ts.last_user = user
        # Picking a fresh node clears stale-misses; if it was stale, it can be retried
        ts.consecutive_squeue_misses = 0
        self.save()
        if ts.status in ("idle", "stale", "failed", "port_busy"):
            self.start(name)

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
        """Start (or restart) a tunnel.

        BLOCKS for up to PROBE_TIMEOUT_SEC (~10s) while probing the local port.
        Callers on the UI thread MUST invoke this from a worker thread.

        Idempotent: no-op if already alive or starting.
        Thread-safe via a per-tunnel lock — calls on DIFFERENT tunnels do
        NOT block each other (so two tunnels can probe in parallel).
        Calls on the SAME tunnel serialise.
        Silently returns if the tunnel has been removed (race with delete).
        """
        with self._lock_for(name):
            ts = self.tunnels.get(name)
            if ts is None:
                return

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

            # Defensive default for hand-edited tunnels.json missing last_user
            user = ts.last_user or os.environ.get("USER", "")
            if not user:
                ts.status = "failed"
                ts.last_msg = "no user (set last_user in tunnels.json)"
                ts.active_jump = None
                return

            argv = [
                "-N",
                "-J", jump,
                "-L", f"{ts.local_port}:localhost:{ts.remote_port}",
                "-o", "StrictHostKeyChecking=no",
                "-o", "UserKnownHostsFile=/dev/null",
                "-o", "ExitOnForwardFailure=yes",
                "-o", "ServerAliveInterval=15",
                f"{user}@{ts.last_node}",
            ]
            try:
                child = pexpect.spawn("ssh", argv, encoding="utf-8", timeout=15)
            except Exception as e:
                # Could not spawn (ssh missing, OS resource limit, etc.)
                ts.status = "failed"
                ts.last_msg = f"spawn failed: {str(e)[:60]}"
                ts.active_jump = None
                return
            ts.child = child

            if self._probe_port_ready(ts.local_port, self.PROBE_TIMEOUT_SEC):
                ts.status = "alive"
                ts.last_msg = f"via {jump}"
                ts.consecutive_squeue_misses = 0
                ts.last_alive_at = time.time()
                ts._alive_since = time.time()
                ts.connect_count += 1
                self._record(ts, f"connected via {jump} → {ts.last_node}:{ts.remote_port}")
                # Run the per-tunnel post-connect hook if any. Threaded so a
                # slow hook can't block us — we capture stderr to the event
                # log so the user can debug from the popover.
                # Refuse to spawn a second hook for the same tunnel if one
                # is still running (a flapping tunnel could otherwise stack
                # 30-second hook processes, doubling webhooks / browsers).
                if ts.post_connect_cmd:
                    with self._post_connect_lock:
                        if name in self._post_connect_running:
                            self._record(ts,
                                "post_connect: previous hook still running, skipping")
                        else:
                            self._post_connect_running.add(name)
                            threading.Thread(
                                target=self._run_post_connect, args=(name,),
                                daemon=True
                            ).start()
            else:
                try:
                    child.terminate(force=True)
                except Exception:
                    pass
                reason = self._extract_failure_reason(child)
                ts.fail_count += 1
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

    def _record(self, ts: TunnelState, msg: str) -> None:
        """Append a timestamped event to the per-tunnel ring buffer.
        Caller already holds the tunnel lock (or doesn't need to —
        appending to a list is GIL-protected enough for our needs)."""
        ts.events.append({"ts": time.time(), "msg": msg})
        if len(ts.events) > self.EVENT_BUFFER_LIMIT:
            del ts.events[: len(ts.events) - self.EVENT_BUFFER_LIMIT]

    def _accumulate_uptime(self, ts: TunnelState) -> None:
        """If this tunnel was in alive, fold its current run into total_uptime
        and clear the _alive_since marker. Idempotent — safe to call before
        any status transition out of alive."""
        if ts._alive_since > 0:
            ts.total_uptime_sec += max(0.0, time.time() - ts._alive_since)
            ts._alive_since = 0.0

    def _run_post_connect(self, name: str) -> None:
        """Run the user-supplied post-connect shell command. We deliberately
        DON'T do any sandboxing — the user supplied the command, it runs
        with the daemon's privileges. We DO sanitize env-var values: a
        malicious node name like `; rm -rf ~` could otherwise execute as
        part of `sh -c "echo $AUTO2FA_NODE"` after env expansion. We strip
        anything that isn't a safe identifier/hostname char from the
        externally-controlled values — paranoia, but cheap."""
        ts = self.tunnels.get(name)
        if ts is None or not ts.post_connect_cmd:
            with self._post_connect_lock:
                self._post_connect_running.discard(name)
            return

        def _sanitize(s: str) -> str:
            # Allow word chars, dot, dash, colon, slash, @, %  — anything
            # that legitimately appears in hostnames / paths / URLs.
            # Refuse anything with shell metachars; the user's hook can
            # always read the raw value via ts.events / log if it really
            # needs to.
            import re as _re
            return _re.sub(r"[^A-Za-z0-9._:/@%-]", "", s or "")

        env = os.environ.copy()
        env.update({
            "AUTO2FA_TUNNEL_NAME": _sanitize(ts.name),
            "AUTO2FA_LOCAL_PORT": str(ts.local_port),
            "AUTO2FA_NODE": _sanitize(ts.last_node or ""),
            "AUTO2FA_JUMP": _sanitize(ts.active_jump or ""),
            "AUTO2FA_URL": f"http://localhost:{ts.local_port}",
        })
        self._record(ts, f"post_connect: running `{ts.post_connect_cmd[:60]}`")
        try:
            r = subprocess.run(
                ["/bin/sh", "-c", ts.post_connect_cmd],
                env=env, capture_output=True, text=True, timeout=30,
            )
            out = (r.stdout + r.stderr).strip()
            if r.returncode == 0:
                tail = out[:120] if out else "ok"
                self._record(ts, f"post_connect: exit 0  {tail}")
            else:
                tail = out[:120] if out else "(no output)"
                self._record(ts, f"post_connect: exit {r.returncode}  {tail}")
        except subprocess.TimeoutExpired:
            self._record(ts, "post_connect: TIMEOUT after 30s")
        except Exception as e:
            self._record(ts, f"post_connect: error {e}")
        finally:
            with self._post_connect_lock:
                self._post_connect_running.discard(name)

    def stop(self, name: str) -> None:
        """Terminate the tunnel's child process and mark idle.

        Safe if already stopped or removed. Per-tunnel locking — doesn't
        block other tunnels' lifecycle operations.
        """
        with self._lock_for(name):
            ts = self.tunnels.get(name)
            if ts is None:
                return
            child = ts.child
            if child is not None:
                try:
                    if child.isalive():
                        child.terminate(force=True)
                except Exception:
                    pass
            ts.child = None
            ts.active_jump = None
            self._accumulate_uptime(ts)
            ts.status = "idle"
            ts.last_msg = "stopped"

    def toggle(self, name: str) -> None:
        """If alive/starting, stop. Otherwise, start.

        NOTE: like start(), this may BLOCK for up to PROBE_TIMEOUT_SEC.
        UI callers must invoke from a worker thread.
        Silently returns if the tunnel was removed.
        """
        # Read the status without holding the lock to decide the action.
        # The lock-protected operations themselves are idempotent: if status
        # changes between the read and the call, start()/stop() handle it.
        ts = self.tunnels.get(name)
        if ts is None:
            return
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
            # Snapshot first so a concurrent add() can't trip
            # "dictionary changed size during iteration". Set the flag
            # AFTER the loop so a mid-iteration crash doesn't permanently
            # skip the remaining tunnels.
            for name, ts in list(self.tunnels.items()):
                if ts.auto_start and ts.last_node is not None:
                    self.start(name)
            self.auto_started = True

        for name, ts in list(self.tunnels.items()):
            # Re-check existence — another thread may have called remove()
            # between snapshotting and reaching this iteration.
            if name not in self.tunnels:
                continue
            if ts.status in ("idle", "stale", "port_busy", "failed", "starting"):
                continue
            # status == "alive"

            # Case 1: child died → must clear status first so start() proceeds.
            # start() short-circuits on status in ("alive", "starting"), so
            # calling it directly would be a permanent no-op.
            child = ts.child
            if child is None or not child.isalive():
                logger.warning("[tunnel:%s] child died, respawning", name)
                self.stop(name)
                self.start(name)
                continue

            # Case 2: jump master no longer ready → failover, but ONLY
            # if the user explicitly disabled the host. The previous code
            # also failed over on transient `is_master_ready=False`
            # (cooldown after a probe blip, MaxSessions briefly full, etc.)
            # — but the multiplexed `ssh -L` child is independent of the
            # master's probe state: an existing forward keeps working
            # over the live channel even if a fresh ssh -O check fails.
            # Tearing it down on a transient blip just dropped the tunnel
            # to idle for no reason ("动不动就 idle 了").
            #
            # If the child has actually broken, Case 1 already caught it
            # above. By the time we get here, the child is alive — so the
            # tunnel IS still working. Only stop if the user has
            # explicitly disabled the host.
            mgr = self.host_managers.get(ts.active_jump)
            if mgr is None or not mgr.active:
                logger.info("[tunnel:%s] jump %s disabled, stopping", name, ts.active_jump)
                self.stop(name)
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
                    self._accumulate_uptime(ts)
                    ts.fail_count += 1
                    ts.status = "stale"
                    ts.last_msg = f"node {ts.last_node} no longer in squeue"

    def shutdown(self) -> None:
        """Stop every tunnel. Called on dashboard exit.

        Aggressive variant of stop(): does NOT wait for the per-tunnel lock,
        because a tunnel currently inside a 10-second start() probe would
        otherwise hang the app exit for that long. Kills children directly;
        the running start()'s probe will just fail when it next polls the
        (now-closed) port and return on its own.
        """
        for name in list(self.tunnels.keys()):
            try:
                lock = self._lock_for(name)
                acquired = lock.acquire(timeout=0.5)
                try:
                    ts = self.tunnels.get(name)
                    if ts is None:
                        continue
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
                finally:
                    if acquired:
                        lock.release()
            except Exception as e:
                logger.error("[tunnel:%s] shutdown error: %s", name, e)

    def cleanup_orphans(self) -> None:
        """Reap stray `ssh -N -J ... -L <our_port>:...` processes left from a prior run.

        For each tunnel, pgrep -f for the unique '-N -J' (auto2fa style — the
        user's own `ssh -L 8888:...` won't have `-J`) AND '-L <port>:localhost:'
        and SIGTERM whatever we find. Called once on dashboard startup.

        Scoping by BOTH `-N -J` and our local port stops us from killing
        unrelated user ssh tunnels that happen to share a local port
        (e.g. the user's own `ssh -L 8888:...` to a different host).
        """
        for ts in self.tunnels.values():
            # Match auto2fa's distinctive pattern. start() always uses
            # "-N", "-J", "<jump>", "-L", "<lp>:localhost:<rp>", "user@node".
            pattern = f"-N.*-L {ts.local_port}:localhost:"
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
                # Second guard: confirm via /proc-style cmdline (via ps)
                # that this is a `ssh -N -J` process, not a shell pipeline.
                try:
                    cmd = subprocess.run(["ps", "-o", "args=", "-p", pid_str],
                                         capture_output=True, text=True, timeout=2)
                    if cmd.returncode != 0:
                        continue
                    args = cmd.stdout.strip()
                    if not args.startswith("ssh"):
                        continue
                    if "-J" not in args.split():
                        continue
                except Exception:
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
