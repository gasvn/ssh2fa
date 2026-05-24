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

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s - %(threadName)s - %(levelname)s - %(message)s",
    handlers=[
        logging.FileHandler("/tmp/auto2fa_daemon.log"),
    ],
)
logger = logging.getLogger(__name__)


# ----------------------------------------------------------------------------

def load_hosts() -> dict:
    config_path = os.environ.get("SSH_CONFIG_PATH")
    if not config_path:
        raise RuntimeError("SSH_CONFIG_PATH not set")
    with open(f"{config_path}/passwords.json") as f:
        return json.load(f)


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
            "active_jump": ts.active_jump,
            "status": ts.status,
            "last_msg": ts.last_msg,
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

    # ---- Event emitter (polls state, pushes deltas) ----------------------

    async def _state_poll_loop(self) -> None:
        """Snapshot state every 0.5s; emit events for changes."""
        while not self._tick_stop:
            try:
                # Drive the tunnel manager's tick — it would normally be done
                # by main.py's background thread, but the daemon owns lifecycle
                self.tunnel_mgr.tick()
            except Exception:
                logger.exception("tunnel_mgr.tick failed")

            try:
                # Host transitions
                for mgr in self.managers:
                    snap = self._host_snapshot(mgr)
                    prev = self._last_host_snapshot.get(mgr.host)
                    if prev != snap:
                        self._last_host_snapshot[mgr.host] = snap
                        await self._emit(ipc.Event.HOST_STATUS_CHANGED, snap)

                # Tunnel transitions (add / remove / status)
                seen = set()
                for name in list(self.tunnel_mgr.tunnels.keys()):
                    seen.add(name)
                    snap = self._tunnel_snapshot(name)
                    if snap is None:
                        continue
                    prev = self._last_tunnel_snapshot.get(name)
                    if prev != snap:
                        self._last_tunnel_snapshot[name] = snap
                        await self._emit(ipc.Event.TUNNEL_STATUS_CHANGED, snap)
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
        for mgr in self.managers:
            mgr.running = False
            mgr.active = False
        try:
            self.tunnel_mgr.shutdown()
        except Exception:
            logger.exception("tunnel_mgr.shutdown failed")
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
