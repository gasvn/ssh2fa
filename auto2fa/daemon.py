"""Auto2FA daemon — IPC server that owns the SSHHostManagers and TunnelManager.

Long-running process. Accepts JSON-RPC over a Unix domain socket. Serves the
Mac app (and eventually the TUI in daemon-client mode).

Start with: `auto2fa-daemon`  (entry point in setup.py)
Or programmatically: `python -m auto2fa.daemon`

See docs/superpowers/specs/2026-05-24-mac-app-design.md.
"""
from __future__ import annotations

import asyncio
import json
import logging
import os
import signal
import sys
import time
from typing import Any

from dotenv import load_dotenv

load_dotenv()

from .backend import SSHHostManager, extract_secret_from_url
from .tunnels import (
    DiscoveryError,
    NodeDiscovery,
    TunnelManager,
    expand_first_node,
)
from . import ipc
from . import credentials

def _rotate_log_if_huge(path: str, max_bytes: int = 10 * 1024 * 1024) -> None:
    """If the daemon log is larger than max_bytes, gzip it aside with a
    timestamp suffix and start fresh. Called once at daemon startup —
    keeps logs from accumulating to 80+ MB (which we've seen in the wild)
    without any third-party logging deps."""
    try:
        if not os.path.exists(path):
            return
        if os.path.getsize(path) < max_bytes:
            return
        import gzip
        import shutil as _shutil
        from datetime import datetime
        stamp = datetime.now().strftime("%Y%m%d-%H%M%S")
        rotated = f"{path}.{stamp}.gz"
        with open(path, "rb") as src, gzip.open(rotated, "wb") as dst:
            _shutil.copyfileobj(src, dst)
        os.truncate(path, 0)
    except Exception as e:  # never let logging-init failure crash daemon
        print(f"[daemon] log rotation failed (continuing): {e}")


_rotate_log_if_huge("/tmp/auto2fa_daemon.log")

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s - %(threadName)s - %(levelname)s - %(message)s",
    handlers=[
        logging.FileHandler("/tmp/auto2fa_daemon.log"),
    ],
)
logger = logging.getLogger(__name__)


# ----------------------------------------------------------------------------

def _live_uptime(ts) -> float:
    """Compute total uptime including the current alive-run as of right now.
    Snapshot both ts.total_uptime_sec and ts._alive_since into locals before
    doing the arithmetic so another thread accumulating into the tunnel
    can't race us into reporting a wildly wrong value.

    CPython attribute reads are atomic per-attribute under the GIL, so the
    worst observable case is a one-tick stale total."""
    base = ts.total_uptime_sec
    since = ts._alive_since
    if since > 0:
        return base + max(0.0, time.time() - since)
    return base


def _tail_file(path: str, n: int) -> list[str]:
    """Return the last n lines of `path`, or [] if it doesn't exist.
    Uses a backwards block read so it's cheap even for multi-MB logs."""
    if not os.path.exists(path):
        return []
    block_size = 4096
    lines: list[bytes] = []
    with open(path, "rb") as f:
        f.seek(0, os.SEEK_END)
        size = f.tell()
        offset = size
        carry = b""
        while offset > 0 and len(lines) <= n:
            read = min(block_size, offset)
            offset -= read
            f.seek(offset)
            chunk = f.read(read) + carry
            parts = chunk.split(b"\n")
            carry = parts[0]
            lines = parts[1:] + lines
        if offset == 0 and carry:
            lines = [carry] + lines
    decoded = [l.decode("utf-8", errors="replace") for l in lines if l]
    return decoded[-n:]


def load_hosts() -> dict:
    """Thin wrapper around credentials.load_config that exists for back-
    compat with anything still importing daemon.load_hosts. The real
    logic (Keychain fetch + auto-migrate) lives in credentials.py."""
    return credentials.load_config()


class Auto2FADaemon:
    def __init__(self):
        self.managers: list[SSHHostManager] = []
        self.tunnel_mgr: TunnelManager | None = None
        self.host_map: dict[str, SSHHostManager] = {}
        self._loop: asyncio.AbstractEventLoop | None = None
        self._clients: set[asyncio.StreamWriter] = set()
        self._subscribers: set[asyncio.StreamWriter] = set()
        self._tick_stop = False
        # Snapshot last-emitted state per host/tunnel for change detection
        self._last_host_snapshot: dict[str, dict] = {}
        self._last_tunnel_snapshot: dict[str, dict] = {}

    # ---- Bootstrap -------------------------------------------------------

    def init_managers(self) -> None:
        config = load_hosts()
        for host, creds in config.items():
            if "otpauthUrl" not in creds:
                continue
            secret = extract_secret_from_url(creds["otpauthUrl"])
            mgr = SSHHostManager(host, creds["password"], secret)
            mgr.daemon = True
            mgr.active = creds.get("autoConnect", creds.get("auto_connect", False))
            mgr.start()
            self.managers.append(mgr)
        self.host_map = {m.host: m for m in self.managers}

        config_path = os.environ.get("SSH_CONFIG_PATH")
        tunnels_cfg = os.path.join(config_path, "tunnels.json")
        self.tunnel_mgr = TunnelManager(
            host_managers=self.host_map, config_path=tunnels_cfg
        )
        self.tunnel_mgr.load()
        self.tunnel_mgr.cleanup_orphans()
        self.tunnel_mgr.startup_ts = time.time()
        logger.info(
            f"Daemon initialised: {len(self.managers)} hosts, "
            f"{len(self.tunnel_mgr.tunnels)} tunnels"
        )

    # ---- State snapshots --------------------------------------------------

    def _host_snapshot(self, mgr: SSHHostManager) -> dict:
        try:
            pool_alive = sum(1 for c in list(mgr.pool.values()) if c.isalive())
        except Exception:
            pool_alive = 0
        return {
            "host": mgr.host,
            "status": mgr.status,
            "active": mgr.active,
            "is_master_ready": mgr.is_master_ready(),
            "pool_index": mgr.active_index,
            "pool_alive": pool_alive,
            "is_mounted": getattr(mgr, "is_mounted", False),
            "last_msg": mgr.last_msg,
        }

    def _tunnel_snapshot(self, name: str) -> dict | None:
        ts = self.tunnel_mgr.tunnels.get(name)
        if ts is None:
            return None
        return {
            "name": ts.name,
            "local_port": ts.local_port,
            "remote_port": ts.remote_port,
            "jump_candidates": ts.jump_candidates,
            "last_node": ts.last_node,
            "last_user": ts.last_user,
            "auto_start": ts.auto_start,
            "post_connect_cmd": ts.post_connect_cmd,
            "tags": list(ts.tags),
            "url_path": ts.url_path,
            "active_jump": ts.active_jump,
            "status": ts.status,
            "last_msg": ts.last_msg,
            "last_alive_at": ts.last_alive_at,
            # Live-computed: total_uptime + current run if alive.
            # Snapshot BOTH attrs into locals first so an interleaving
            # _accumulate_uptime() from another thread can't zero
            # _alive_since between the two reads (which would briefly
            # report an underestimate). Worst case now: we use slightly
            # stale `total_uptime_sec` for one tick — close enough.
            "total_uptime_sec": _live_uptime(ts),
            "connect_count": ts.connect_count,
            "fail_count": ts.fail_count,
        }

    def list_hosts(self) -> list[dict]:
        return [self._host_snapshot(m) for m in self.managers]

    def list_tunnels(self) -> list[dict]:
        return [
            self._tunnel_snapshot(n)
            for n in list(self.tunnel_mgr.tunnels.keys())
            if self._tunnel_snapshot(n) is not None
        ]

    # ---- IPC handlers ----------------------------------------------------

    async def handle_request(self, req: dict) -> dict:
        req_id = req.get("id", "")
        method = req.get("method", "")
        params = req.get("params") or {}

        try:
            if method == ipc.Method.PING:
                return ipc.make_response(req_id, {"ok": True, "pid": os.getpid()})

            if method == ipc.Method.LIST_HOSTS:
                return ipc.make_response(req_id, self.list_hosts())

            if method == ipc.Method.LIST_TUNNELS:
                return ipc.make_response(req_id, self.list_tunnels())

            if method == ipc.Method.HOST_TOGGLE:
                host = params["host"]
                mgr = self.host_map.get(host)
                if mgr is None:
                    return ipc.make_error(req_id, ipc.ErrCode.NOT_FOUND, f"host {host}")
                mgr.toggle()
                return ipc.make_response(req_id, None)

            if method == ipc.Method.HOST_MOUNT_TOGGLE:
                host = params["host"]
                mgr = self.host_map.get(host)
                if mgr is None:
                    return ipc.make_error(req_id, ipc.ErrCode.NOT_FOUND, f"host {host}")
                # Long op — run in a thread so we don't block the event loop
                await asyncio.to_thread(mgr.toggle_mount)
                return ipc.make_response(req_id, None)

            if method == ipc.Method.HOST_ROTATE:
                host = params["host"]
                mgr = self.host_map.get(host)
                if mgr is None or not mgr.active:
                    return ipc.make_error(req_id, ipc.ErrCode.NOT_FOUND, "host not active")
                new_idx = (mgr.active_index + 1) % 2
                mgr.update_symlink(new_idx)
                mgr.last_msg = f"Manual Rotate -> {new_idx}"
                return ipc.make_response(req_id, None)

            if method == ipc.Method.TUNNEL_ADD:
                try:
                    name = params["name"]
                    lp = int(params["local_port"])
                    rp = params.get("remote_port")
                    self.tunnel_mgr.add(
                        name=name,
                        local_port=lp,
                        remote_port=int(rp) if rp else None,
                    )
                except ValueError as e:
                    code = ipc.ErrCode.DUPLICATE if "exists" in str(e).lower() \
                        else ipc.ErrCode.PORT_IN_USE if "in use" in str(e).lower() \
                        else ipc.ErrCode.BAD_PARAMS
                    return ipc.make_error(req_id, code, str(e))
                return ipc.make_response(req_id, self._tunnel_snapshot(name))

            if method == ipc.Method.TUNNEL_REMOVE:
                name = params["name"]
                # stop+remove can hold a lock briefly — thread it
                def _do():
                    try:
                        self.tunnel_mgr.stop(name)
                        self.tunnel_mgr.remove(name)
                    except Exception as e:
                        logger.error(f"remove({name}) failed: {e}")
                await asyncio.to_thread(_do)
                return ipc.make_response(req_id, None)

            if method == ipc.Method.TUNNEL_START:
                # Idempotent start. Safe to use from scripts that don't
                # know the current state.
                name = params["name"]
                if name not in self.tunnel_mgr.tunnels:
                    return ipc.make_error(req_id, ipc.ErrCode.NOT_FOUND, name)
                if self.tunnel_mgr.tunnels[name].status == "alive":
                    return ipc.make_response(req_id, None)
                await asyncio.to_thread(self.tunnel_mgr.start, name)
                return ipc.make_response(req_id, None)

            if method == ipc.Method.TUNNEL_STOP:
                # Idempotent stop. Safe to use from scripts.
                name = params["name"]
                if name not in self.tunnel_mgr.tunnels:
                    return ipc.make_error(req_id, ipc.ErrCode.NOT_FOUND, name)
                if self.tunnel_mgr.tunnels[name].status != "alive":
                    return ipc.make_response(req_id, None)
                await asyncio.to_thread(self.tunnel_mgr.stop, name)
                return ipc.make_response(req_id, None)

            if method == ipc.Method.TUNNEL_TOGGLE:
                name = params["name"]
                if name not in self.tunnel_mgr.tunnels:
                    return ipc.make_error(req_id, ipc.ErrCode.NOT_FOUND, name)
                # toggle() may block up to 10s — thread it
                await asyncio.to_thread(self.tunnel_mgr.toggle, name)
                return ipc.make_response(req_id, None)

            if method == ipc.Method.TUNNEL_SET_NODE:
                name = params["name"]
                node = params["node"]
                user = params.get("user") or os.environ.get("USER", "")
                # Handles SLURM ranges so the Mac app doesn't need to know about it
                node, _is_range = expand_first_node(node)
                if name not in self.tunnel_mgr.tunnels:
                    return ipc.make_error(req_id, ipc.ErrCode.NOT_FOUND, name)
                await asyncio.to_thread(self.tunnel_mgr.set_node, name, node, user)
                return ipc.make_response(req_id, None)

            if method == ipc.Method.DISCOVER_NODES:
                host = params["host"]
                mgr = self.host_map.get(host)
                if mgr is None:
                    return ipc.make_error(req_id, ipc.ErrCode.NOT_FOUND, host)
                if not mgr.is_master_ready():
                    return ipc.make_error(
                        req_id, ipc.ErrCode.DISCOVERY_FAILED,
                        f"{host} master not ready",
                    )
                try:
                    jobs = await asyncio.to_thread(NodeDiscovery.discover, mgr)
                except DiscoveryError as e:
                    return ipc.make_error(req_id, ipc.ErrCode.DISCOVERY_FAILED, str(e))
                return ipc.make_response(
                    req_id,
                    [
                        {
                            "jobid": j.jobid,
                            "partition": j.partition,
                            "name": j.name,
                            "state": j.state,
                            "time": j.time,
                            "node": j.node,
                        }
                        for j in jobs
                    ],
                )

            if method == ipc.Method.TUNNEL_SET_URL_PATH:
                # Save the URL path/query suffix used when "Open in browser"
                # fires. Empty / null clears it.
                name = params["name"]
                path = params.get("path")
                if isinstance(path, str) and not path.strip():
                    path = None
                ts = self.tunnel_mgr.tunnels.get(name)
                if ts is None:
                    return ipc.make_error(req_id, ipc.ErrCode.NOT_FOUND, name)
                ts.url_path = path
                self.tunnel_mgr.save()
                return ipc.make_response(req_id, self._tunnel_snapshot(name))

            if method == ipc.Method.TUNNEL_SET_TAGS:
                # Replace the tag list for a tunnel. Empty list clears tags.
                name = params["name"]
                tags = params.get("tags") or []
                if not isinstance(tags, list):
                    return ipc.make_error(req_id, ipc.ErrCode.BAD_PARAMS,
                                          "tags must be a list of strings")
                ts = self.tunnel_mgr.tunnels.get(name)
                if ts is None:
                    return ipc.make_error(req_id, ipc.ErrCode.NOT_FOUND, name)
                ts.tags = [str(t).strip() for t in tags if str(t).strip()]
                self.tunnel_mgr.save()
                return ipc.make_response(req_id, self._tunnel_snapshot(name))

            if method == ipc.Method.TUNNEL_RENAME:
                # Rename a tunnel. Stops it first if alive (since the ssh
                # child references the old name in last_msg etc.), renames,
                # then restarts it.
                old = params["old"]
                new = params["new"]
                if not new or not isinstance(new, str):
                    return ipc.make_error(req_id, ipc.ErrCode.BAD_PARAMS, "new name required")
                new = new.strip()
                if new == old:
                    return ipc.make_response(req_id, self._tunnel_snapshot(old))
                if new in self.tunnel_mgr.tunnels:
                    return ipc.make_error(req_id, ipc.ErrCode.DUPLICATE,
                                          f"tunnel '{new}' already exists")
                ts = self.tunnel_mgr.tunnels.get(old)
                if ts is None:
                    return ipc.make_error(req_id, ipc.ErrCode.NOT_FOUND, old)
                was_alive = ts.status == "alive"
                if was_alive:
                    await asyncio.to_thread(self.tunnel_mgr.stop, old)
                # Reseat under the new key.
                ts.name = new
                self.tunnel_mgr.tunnels[new] = ts
                del self.tunnel_mgr.tunnels[old]
                self.tunnel_mgr.save()
                if was_alive:
                    await asyncio.to_thread(self.tunnel_mgr.start, new)
                return ipc.make_response(req_id, self._tunnel_snapshot(new))

            if method == ipc.Method.TUNNELS_BATCH:
                # Apply an action (start/stop) to a set of tunnel names.
                # Returns {results: [{name, ok, error}]}. Errors per item
                # don't abort the batch — best-effort.
                action = params.get("action", "")
                names = params.get("names") or []
                if action not in ("start", "stop"):
                    return ipc.make_error(req_id, ipc.ErrCode.BAD_PARAMS,
                                          "action must be 'start' or 'stop'")
                results = []
                for name in names:
                    if name not in self.tunnel_mgr.tunnels:
                        results.append({"name": name, "ok": False, "error": "not found"})
                        continue
                    try:
                        if action == "start":
                            await asyncio.to_thread(self.tunnel_mgr.start, name)
                        else:
                            await asyncio.to_thread(self.tunnel_mgr.stop, name)
                        results.append({"name": name, "ok": True})
                    except Exception as e:
                        results.append({"name": name, "ok": False, "error": str(e)})
                return ipc.make_response(req_id, {"results": results})

            if method == ipc.Method.TUNNEL_EVENTS:
                # Return the per-tunnel activity ring buffer for debugging.
                name = params["name"]
                ts = self.tunnel_mgr.tunnels.get(name)
                if ts is None:
                    return ipc.make_error(req_id, ipc.ErrCode.NOT_FOUND, name)
                return ipc.make_response(req_id, {"events": list(ts.events)})

            if method == ipc.Method.TUNNEL_SET_POST_CONNECT:
                # Set / clear the per-tunnel post-connect shell command.
                # An empty string or null clears it. Persisted.
                name = params["name"]
                cmd = params.get("cmd")
                if isinstance(cmd, str) and not cmd.strip():
                    cmd = None
                ts = self.tunnel_mgr.tunnels.get(name)
                if ts is None:
                    return ipc.make_error(req_id, ipc.ErrCode.NOT_FOUND, name)
                ts.post_connect_cmd = cmd
                self.tunnel_mgr.save()
                return ipc.make_response(req_id, self._tunnel_snapshot(name))

            if method == ipc.Method.RESET_ALL:
                # Nuclear option: stop every tunnel, force-rebuild every
                # master. The user-visible escape hatch when things wedge.
                affected = await asyncio.to_thread(self._reset_all)
                return ipc.make_response(req_id, affected)

            if method == ipc.Method.TUNNEL_SET_JUMP_CANDIDATES:
                # Set the per-tunnel jump-host whitelist. null/None means
                # "auto, any ready host"; a list pins this tunnel to the
                # given hosts in priority order. If the tunnel is currently
                # alive, restart it through the new candidates so the change
                # takes effect immediately.
                name = params["name"]
                cands = params.get("candidates")  # null OR list[str]
                if cands is not None and not isinstance(cands, list):
                    return ipc.make_error(req_id, ipc.ErrCode.BAD_PARAMS,
                                          "candidates must be list or null")
                if cands is not None:
                    # Defensive: drop unknown host names so the tunnel can't
                    # be wedged by typos in a list editor.
                    cands = [c for c in cands if c in self.host_map]
                ts = self.tunnel_mgr.tunnels.get(name)
                if ts is None:
                    return ipc.make_error(req_id, ipc.ErrCode.NOT_FOUND, name)
                was_alive = ts.status == "alive"
                if was_alive:
                    await asyncio.to_thread(self.tunnel_mgr.stop, name)
                ts.jump_candidates = cands
                self.tunnel_mgr.save()
                if was_alive:
                    await asyncio.to_thread(self.tunnel_mgr.start, name)
                return ipc.make_response(req_id, self._tunnel_snapshot(name))

            if method == ipc.Method.TUNNEL_SET_AUTOSTART:
                name = params["name"]
                value = bool(params.get("value", False))
                ts = self.tunnel_mgr.tunnels.get(name)
                if ts is None:
                    return ipc.make_error(req_id, ipc.ErrCode.NOT_FOUND, name)
                ts.auto_start = value
                self.tunnel_mgr.save()
                return ipc.make_response(req_id, self._tunnel_snapshot(name))

            if method == ipc.Method.PORT_SUGGEST:
                # Find next free local port starting from 8888 that isn't
                # used by any existing tunnel and isn't currently in use.
                taken = {ts.local_port for ts in self.tunnel_mgr.tunnels.values()}
                base = int(params.get("base", 8888))
                free = await asyncio.to_thread(self._find_free_port, base, taken)
                return ipc.make_response(req_id, {"port": free})

            if method == ipc.Method.HOST_TEST_CREDENTIALS:
                # Dry-run a single ssh login with the supplied creds. Used by
                # the Add Host wizard to refuse a save when password/OTP are
                # wrong — which is what produced the 17 failed-login rate-
                # limit incident before. Returns {"ok": bool, "reason": str}.
                host = params["host"]
                password = params.get("password", "")
                otpauth_url = params.get("otpauth_url", "")
                try:
                    secret = extract_secret_from_url(otpauth_url)
                except Exception as e:
                    return ipc.make_response(req_id, {
                        "ok": False, "reason": f"invalid otpauth URL: {e}"
                    })
                ok, reason = await asyncio.to_thread(
                    self._test_credentials, host, password, secret
                )
                return ipc.make_response(req_id, {"ok": ok, "reason": reason})

            if method == ipc.Method.HOST_ADD:
                # Add a host to passwords.json AND start a manager for it.
                host = params["host"]
                password = params.get("password", "")
                otpauth_url = params.get("otpauth_url", "")
                auto_connect = bool(params.get("auto_connect", False))
                try:
                    secret = extract_secret_from_url(otpauth_url)
                except Exception as e:
                    return ipc.make_error(req_id, ipc.ErrCode.BAD_PARAMS,
                                          f"invalid otpauth URL: {e}")
                added = await asyncio.to_thread(
                    self._add_host_persistent,
                    host, password, otpauth_url, auto_connect, secret
                )
                if not added:
                    return ipc.make_error(req_id, ipc.ErrCode.DUPLICATE,
                                          f"host {host} already exists")
                return ipc.make_response(req_id, self._host_snapshot(self.host_map[host]))

            if method == ipc.Method.LOG_TAIL:
                # Return the last N lines of the daemon log file.
                n = int(params.get("lines", 200))
                try:
                    lines = await asyncio.to_thread(_tail_file, "/tmp/auto2fa_daemon.log", n)
                except Exception as e:
                    return ipc.make_error(req_id, ipc.ErrCode.INTERNAL, str(e))
                return ipc.make_response(req_id, {"lines": lines})

            if method == ipc.Method.WAKE_RECOVER:
                # Mac woke from sleep — every SSH master's underlying TCP is
                # almost certainly dead. Rebuild masters + restart tunnels
                # that were alive at sleep time.
                affected = await asyncio.to_thread(self._wake_recover)
                return ipc.make_response(req_id, {"tunnels_restarting": affected})

            # SUBSCRIBE_EVENTS is handled in the connection loop directly
            # (it needs the writer to add to self._subscribers).
            return ipc.make_error(
                req_id, ipc.ErrCode.UNKNOWN_METHOD, f"unknown method {method}"
            )
        except KeyError as e:
            return ipc.make_error(req_id, ipc.ErrCode.BAD_PARAMS, f"missing param: {e}")
        except Exception as e:
            logger.exception(f"handler {method} failed")
            return ipc.make_error(req_id, ipc.ErrCode.INTERNAL, str(e))

    # ---- Connection loop -------------------------------------------------

    async def _handle_client(
        self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter
    ) -> None:
        self._clients.add(writer)
        peer = writer.get_extra_info("peername") or "<unix>"
        logger.info(f"client connected: {peer}")
        try:
            while not reader.at_eof():
                line = await reader.readline()
                if not line:
                    break
                try:
                    msg = ipc.decode(line)
                except json.JSONDecodeError:
                    writer.write(ipc.encode(
                        ipc.make_error("", ipc.ErrCode.INVALID_REQUEST, "bad JSON")
                    ))
                    await writer.drain()
                    continue

                # Special-case subscribe — needs the writer reference
                if msg.get("method") == ipc.Method.SUBSCRIBE_EVENTS:
                    self._subscribers.add(writer)
                    writer.write(ipc.encode(ipc.make_response(msg.get("id", ""), {"subscribed": True})))
                    await writer.drain()
                    continue

                resp = await self.handle_request(msg)
                writer.write(ipc.encode(resp))
                await writer.drain()
        except (ConnectionResetError, BrokenPipeError):
            pass
        except Exception:
            logger.exception("client handler crashed")
        finally:
            self._clients.discard(writer)
            self._subscribers.discard(writer)
            try:
                writer.close()
                await writer.wait_closed()
            except Exception:
                pass
            logger.info(f"client disconnected: {peer}")

    # ---- Change-detection key ------------------------------------------

    # Snapshot fields whose mutation should trigger a TUNNEL_STATUS_CHANGED
    # event. Excludes total_uptime_sec (advances every tick), connect_count
    # and fail_count (advance on real transitions which other fields already
    # capture). Including just these keeps the UI quiet while a tunnel is
    # steady-state alive.
    _TUNNEL_STABLE_FIELDS = (
        "name", "local_port", "remote_port", "jump_candidates",
        "last_node", "last_user", "auto_start", "post_connect_cmd",
        "tags", "active_jump", "status", "last_msg", "last_alive_at",
    )

    # Host snapshot: include the fields a UI would visibly change on, but
    # NOT last_msg — that's free-form daemon-internal text that changes
    # for things like cool-down countdowns ("298s left" → "297s left" → …),
    # which would otherwise spam HOST_STATUS_CHANGED every tick.
    _HOST_STABLE_FIELDS = (
        "host", "status", "active", "is_master_ready",
        "pool_index", "pool_alive", "is_mounted",
    )

    def _tunnel_change_key(self, snap: dict | None) -> tuple | None:
        if snap is None:
            return None
        return tuple(snap.get(k) if not isinstance(snap.get(k), list)
                     else tuple(snap.get(k) or [])
                     for k in self._TUNNEL_STABLE_FIELDS)

    def _host_change_key(self, snap: dict | None) -> tuple | None:
        if snap is None:
            return None
        return tuple(snap.get(k) for k in self._HOST_STABLE_FIELDS)

    # ---- Helpers used by new client methods ------------------------------

    def _find_free_port(self, base: int, taken: set[int]) -> int:
        """Return the lowest port >= base that isn't in `taken` AND isn't
        currently bound on 127.0.0.1. Falls back to base+1000 if nothing's
        free, which would indicate a broken system."""
        import socket as _s
        for port in range(max(base, 1024), min(base + 1000, 65535)):
            if port in taken:
                continue
            sk = _s.socket(_s.AF_INET, _s.SOCK_STREAM)
            sk.setsockopt(_s.SOL_SOCKET, _s.SO_REUSEADDR, 1)
            try:
                sk.bind(("127.0.0.1", port))
                sk.close()
                return port
            except OSError:
                continue
            finally:
                try: sk.close()
                except Exception: pass
        return base

    def _test_credentials(self, host: str, password: str, secret: str) -> tuple[bool, str]:
        """Run a one-shot, isolated SSH login attempt. Returns (ok, reason).
        On failure the reason is a short human string ("Wrong password",
        "OTP rejected", "Host unreachable"). The point is to fail FAST
        without (a) writing to passwords.json, (b) spawning a long-lived
        manager, or (c) producing the cascade of retried failed-login
        attempts that triggers server-side rate-limiting."""
        import pexpect
        import tempfile
        from .backend import generate_passcode_from_secret
        # Use mkstemp (not mktemp — deprecated, symlink-attack vulnerable).
        # We close the fd immediately; ssh -E will reopen the path for writing.
        # We're not racing because we own /tmp/auto2fa-* by convention.
        fd, log_path = tempfile.mkstemp(prefix=f"auto2fa-testlogin-{host}-",
                                        suffix=".log")
        os.close(fd)
        argv = [
            "-v", "-E", log_path,
            "-o", "StrictHostKeyChecking=no",
            "-o", "UserKnownHostsFile=/dev/null",
            "-o", "ConnectTimeout=10",
            "-o", "PreferredAuthentications=keyboard-interactive,password",
            # CRITICAL: disable ControlMaster reuse. Without these flags the
            # test would silently multiplex onto the daemon's existing master
            # for this host and return success WITHOUT actually testing the
            # supplied password+OTP — letting bad creds into passwords.json.
            "-o", "ControlMaster=no",
            "-o", "ControlPath=none",
            host,
            "echo __auto2fa_login_ok__",  # one-shot remote command
        ]
        try:
            child = pexpect.spawn("ssh", argv, encoding="utf-8", timeout=30)
        except Exception as e:
            return (False, f"Could not spawn ssh: {e}")

        try:
            # Walk the login dialog. Same patterns the real start_master uses
            # but condensed because we only need one login here.
            password_sent = False
            otp_sent = False
            for _ in range(6):  # bounded loop to avoid infinite expect
                idx = child.expect([
                    r"[Pp]assword:",              # 0
                    r"[Vv]erification[Cc]ode:",   # 1
                    r"[Tt]oken:",                 # 2
                    r"Verification code:",        # 3
                    r"__auto2fa_login_ok__",      # 4 = success!
                    r"Permission denied",         # 5
                    r"Connection refused",        # 6
                    r"No route to host",          # 7
                    pexpect.TIMEOUT,              # 8
                    pexpect.EOF,                  # 9
                ], timeout=15)
                if idx == 4:
                    return (True, "")
                if idx == 5:
                    return (False, "Permission denied — wrong password or OTP")
                if idx == 6:
                    return (False, "Connection refused — sshd down or wrong port")
                if idx == 7:
                    return (False, "No route to host — wrong hostname or network")
                if idx in (8, 9):
                    return (False, "Timeout / EOF before any recognizable prompt — host unreachable?")
                if idx == 0:  # Password
                    if password_sent:
                        return (False, "Server looped back to Password — wrong password or OTP")
                    child.sendline(password)
                    password_sent = True
                    continue
                if idx in (1, 2, 3):
                    if otp_sent:
                        return (False, "Server asked for OTP twice — rejected")
                    child.sendline(generate_passcode_from_secret(secret))
                    otp_sent = True
                    continue
            return (False, "Login dialog stuck after 6 turns")
        finally:
            try:
                child.close(force=True)
            except Exception:
                pass
            try:
                os.remove(log_path)
            except Exception:
                pass

    def _add_host_persistent(self, host: str, password: str,
                             otpauth_url: str, auto_connect: bool,
                             secret: str) -> bool:
        """Write password+otpauth to the macOS Keychain, append metadata
        to passwords.json, spin up a manager. Returns False if a host
        with that name already exists."""
        if host in self.host_map:
            return False
        # Refuse to clobber an existing JSON entry — that means user added
        # the same host from two sides.
        existing = credentials.load_config()
        if host in existing:
            return False
        credentials.set_credentials(host, password, otpauth_url)
        credentials.save_host_metadata(host, auto_connect)

        mgr = SSHHostManager(host, password, secret)
        mgr.daemon = True
        mgr.active = auto_connect
        mgr.start()
        self.managers.append(mgr)
        self.host_map[host] = mgr
        logger.info(f"host_add: registered {host} (autoConnect={auto_connect})")
        return True

    # ---- Wake recovery (called by clients from a Mac wake notification) --

    def _reset_all(self) -> dict:
        """User-triggered nuclear restart: stop every tunnel + force-
        rebuild every enabled master. Returns counts so the UI can show
        a small confirmation toast."""
        previously_active = [
            name for name, ts in self.tunnel_mgr.tunnels.items()
            if ts.status in ("alive", "starting", "stale")
        ]
        for name in previously_active:
            try:
                self.tunnel_mgr.stop(name)
            except Exception:
                logger.exception(f"reset_all stop({name}) failed")
        rebuilt = 0
        for mgr in self.managers:
            if mgr.active:
                try:
                    mgr.force_master_rebuild()
                    rebuilt += 1
                except Exception:
                    logger.exception(f"reset_all rebuild on {mgr.host} failed")
        return {"tunnels_stopped": len(previously_active), "masters_rebuilt": rebuilt}

    def _wake_recover(self) -> list[str]:
        """Restore connectivity after Mac wake / network change.

        Per-master probe: 5s timeout (was 2s — too aggressive for the
        post-wake network-warmup window). Masters that respond keep
        their user sessions intact.

        Per-tunnel decision: only stop the tunnel if the master it's
        pinned to actually failed. A surviving master means the
        multiplexed `ssh -L` channel is still live — tearing it down
        unconditionally caused the 'tunnels keep disconnecting'
        symptom on every Mac wake.

        Delayed restart: retries with back-off (10s, 20s, 30s, 60s,
        120s) instead of a single 20s attempt. Master logins take
        20-30s themselves; one shot was often too early and left the
        tunnel idle forever."""
        # Snapshot tunnels with their current jump assignment FIRST,
        # before we modify any state.
        alive_tunnels = [
            (name, ts.active_jump)
            for name, ts in self.tunnel_mgr.tunnels.items()
            if ts.status in ("alive", "starting", "stale")
        ]
        logger.info(f"wake_recover: probing {len(self.managers)} masters")

        # Probe each enabled master with a 5s round-trip. Build the set
        # of hosts whose master we need to rebuild.
        import subprocess as _sp
        masters_failed: set[str] = set()
        for mgr in self.managers:
            if not mgr.active:
                continue
            path = mgr.pool_control_paths[mgr.active_index]
            try:
                res = _sp.run(
                    ["ssh", "-o", f"ControlPath={path}", mgr.host, "true"],
                    stdout=_sp.DEVNULL, stderr=_sp.DEVNULL, timeout=5
                )
                if res.returncode == 0:
                    logger.info(f"wake_recover: {mgr.host} survived")
                    continue
            except Exception:
                pass
            masters_failed.add(mgr.host)
            try:
                logger.info(f"wake_recover: {mgr.host} master dead, rebuilding")
                mgr.force_master_rebuild()
            except Exception:
                logger.exception(f"wake_recover rebuild on {mgr.host} failed")

        # Only restart tunnels whose jump master actually failed. Tunnels
        # whose master survived keep their existing ssh -L child running.
        to_restart = [name for (name, jump) in alive_tunnels if jump in masters_failed]
        kept = len(alive_tunnels) - len(to_restart)
        logger.info(
            f"wake_recover: {len(to_restart)} tunnels need restart, "
            f"{kept} kept (master survived)"
        )
        for name in to_restart:
            try:
                self.tunnel_mgr.stop(name)
            except Exception:
                logger.exception(f"wake_recover stop({name}) failed")
        # Record set on self so _delayed_restart can read it
        previously_active = to_restart

        # Schedule a backoff-retried restart on the asyncio loop. Master
        # logins can take 20-30s each; a single 20s wait was often too
        # early — start() would find no ready jump, leave the tunnel
        # idle, and never retry. Now we keep trying at 10/20/30/60/120s
        # marks until the tunnel transitions to alive (or we give up
        # after ~4 minutes).
        loop = self._loop
        if loop is not None and previously_active:
            async def _delayed_restart():
                delays = [10, 20, 30, 60, 120]
                remaining = list(previously_active)
                for delay in delays:
                    await asyncio.sleep(delay)
                    still_idle: list[str] = []
                    for name in remaining:
                        ts = self.tunnel_mgr.tunnels.get(name)
                        if ts is None:
                            continue
                        if ts.status == "alive":
                            logger.info(f"wake_recover: {name} already alive")
                            continue
                        try:
                            await asyncio.to_thread(self.tunnel_mgr.start, name)
                            if ts.status == "alive":
                                logger.info(f"wake_recover: restarted {name}")
                            else:
                                still_idle.append(name)
                        except Exception:
                            logger.exception(f"wake_recover restart({name}) failed")
                            still_idle.append(name)
                    remaining = still_idle
                    if not remaining:
                        return
                if remaining:
                    logger.warning(
                        f"wake_recover gave up after retries; still idle: {remaining}"
                    )
            asyncio.run_coroutine_threadsafe(_delayed_restart(), loop)
        return previously_active

    # ---- Event emitter (polls state, pushes deltas) ----------------------

    _LOG_ROTATE_CHECK_EVERY = 600  # seconds
    _last_log_rotate_check: float = 0.0

    async def _state_poll_loop(self) -> None:
        """Snapshot state every 0.5s; emit events for changes."""
        while not self._tick_stop:
            try:
                # tick() can block up to ~10s probing a port / spawning ssh -L,
                # so we MUST run it in a thread — otherwise the asyncio loop
                # freezes for the duration and no IPC client can be served.
                await asyncio.to_thread(self.tunnel_mgr.tick)
            except Exception:
                logger.exception("tunnel_mgr.tick failed")

            # Periodic runtime log rotation — startup-only rotation can't
            # protect against the daemon spamming for a day straight and
            # filling /tmp. Cheap to check (one os.stat per 10 min).
            now = time.time()
            if now - self._last_log_rotate_check > self._LOG_ROTATE_CHECK_EVERY:
                self._last_log_rotate_check = now
                try:
                    await asyncio.to_thread(_rotate_log_if_huge,
                                            "/tmp/auto2fa_daemon.log")
                except Exception:
                    logger.exception("runtime log rotation failed")

            try:
                # Host transitions. Compare on the stable-fields key so
                # noisy mutations of last_msg (cool-down "298s left" →
                # "297s left" etc.) don't fire HOST_STATUS_CHANGED on
                # every tick. Snapshot is still updated each pass so
                # list_hosts callers see fresh values.
                for mgr in self.managers:
                    snap = self._host_snapshot(mgr)
                    prev = self._last_host_snapshot.get(mgr.host)
                    if self._host_change_key(prev) != self._host_change_key(snap):
                        self._last_host_snapshot[mgr.host] = snap
                        await self._emit(ipc.Event.HOST_STATUS_CHANGED, snap)
                    else:
                        self._last_host_snapshot[mgr.host] = snap

                # Tunnel transitions (add / remove / status). We compare
                # on a STABLE subset of fields — total_uptime_sec is
                # computed live every poll (time.time() - _alive_since),
                # so the raw snapshot dict changes every 0.5s while alive
                # and would otherwise fire a "Connected" event forever.
                seen = set()
                for name in list(self.tunnel_mgr.tunnels.keys()):
                    seen.add(name)
                    snap = self._tunnel_snapshot(name)
                    if snap is None:
                        continue
                    prev = self._last_tunnel_snapshot.get(name)
                    if self._tunnel_change_key(prev) != self._tunnel_change_key(snap):
                        self._last_tunnel_snapshot[name] = snap
                        await self._emit(ipc.Event.TUNNEL_STATUS_CHANGED, snap)
                    else:
                        # Still record the latest stats so list_tunnels
                        # callers see fresh uptime, but DON'T emit.
                        self._last_tunnel_snapshot[name] = snap
                # Cleanup snapshots for removed tunnels
                for n in list(self._last_tunnel_snapshot.keys()):
                    if n not in seen:
                        del self._last_tunnel_snapshot[n]
                        await self._emit(
                            ipc.Event.TUNNEL_STATUS_CHANGED,
                            {"name": n, "status": "removed"},
                        )
            except Exception:
                logger.exception("state poll loop failed")

            await asyncio.sleep(0.5)

    async def _emit(self, event_name: str, data: dict) -> None:
        if not self._subscribers:
            return
        payload = ipc.encode(ipc.make_event(event_name, data))
        dead: list[asyncio.StreamWriter] = []
        for w in list(self._subscribers):
            try:
                w.write(payload)
                await w.drain()
            except Exception:
                dead.append(w)
        for w in dead:
            self._subscribers.discard(w)

    # ---- Server lifecycle ------------------------------------------------

    async def run(self) -> None:
        os.makedirs(ipc.SOCKET_DIR, exist_ok=True)
        # Remove any stale socket from a crashed previous run
        if os.path.exists(ipc.SOCKET_PATH):
            try:
                os.remove(ipc.SOCKET_PATH)
            except OSError as e:
                logger.error(f"could not remove stale socket: {e}")
                raise

        self._loop = asyncio.get_running_loop()
        server = await asyncio.start_unix_server(
            self._handle_client, path=ipc.SOCKET_PATH
        )
        # Tighten permissions — local user only
        try:
            os.chmod(ipc.SOCKET_PATH, 0o600)
        except OSError:
            pass

        self.init_managers()
        poll_task = asyncio.create_task(self._state_poll_loop())

        # Graceful shutdown on SIGINT/SIGTERM
        stop_event = asyncio.Event()
        for sig_name in ("SIGINT", "SIGTERM"):
            self._loop.add_signal_handler(getattr(signal, sig_name), stop_event.set)

        logger.info(f"daemon listening on {ipc.SOCKET_PATH}")
        async with server:
            await stop_event.wait()

        # Teardown
        self._tick_stop = True
        poll_task.cancel()
        # Tell each host manager thread to exit. They will run cleanup_all()
        # on the way out (final block in SSHHostManager.run). We then join
        # them with a generous deadline so the SSH master mux processes don't
        # outlive the daemon — daemon threads die immediately when the main
        # thread exits, and cleanup_all would be cut off mid-flight.
        for mgr in self.managers:
            mgr.running = False
            mgr.active = False
        try:
            self.tunnel_mgr.shutdown()
        except Exception:
            logger.exception("tunnel_mgr.shutdown failed")
        deadline = time.time() + 5.0
        for mgr in self.managers:
            remaining = max(0.0, deadline - time.time())
            mgr.join(timeout=remaining)
            if mgr.is_alive():
                logger.warning(f"[{mgr.host}] manager thread didn't exit within shutdown window")
        try:
            os.remove(ipc.SOCKET_PATH)
        except OSError:
            pass
        logger.info("daemon stopped")


def main() -> int:
    daemon = Auto2FADaemon()
    try:
        asyncio.run(daemon.run())
    except KeyboardInterrupt:
        pass
    except Exception:
        logger.exception("daemon crashed")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
