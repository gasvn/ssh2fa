"""IPC protocol shared between auto2fa-daemon and clients (Mac app, future TUI).

Wire format: line-delimited JSON over a Unix domain socket. Each line is a
complete JSON object terminated by \\n.

See docs/superpowers/specs/2026-05-24-mac-app-design.md.
"""
from __future__ import annotations

import json
import os
from typing import Any, Optional


SOCKET_PATH = os.path.expanduser("~/.auto2fa/auto2fa.sock")
LOCK_PATH = os.path.expanduser("~/.auto2fa/lock")
SOCKET_DIR = os.path.expanduser("~/.auto2fa")


# --- Method names (canonical) -------------------------------------------------

class Method:
    PING = "ping"
    LIST_HOSTS = "list_hosts"
    LIST_TUNNELS = "list_tunnels"
    HOST_TOGGLE = "host_toggle"
    HOST_MOUNT_TOGGLE = "host_mount_toggle"
    HOST_ROTATE = "host_rotate"
    TUNNEL_ADD = "tunnel_add"
    TUNNEL_REMOVE = "tunnel_remove"
    TUNNEL_TOGGLE = "tunnel_toggle"
    TUNNEL_START = "tunnel_start"
    TUNNEL_STOP = "tunnel_stop"
    TUNNEL_SET_NODE = "tunnel_set_node"
    TUNNEL_SET_AUTOSTART = "tunnel_set_autostart"
    TUNNEL_SET_JUMP_CANDIDATES = "tunnel_set_jump_candidates"
    DISCOVER_NODES = "discover_nodes"
    SUBSCRIBE_EVENTS = "subscribe_events"
    WAKE_RECOVER = "wake_recover"
    HOST_ADD = "host_add"
    HOST_TEST_CREDENTIALS = "host_test_credentials"
    PORT_SUGGEST = "port_suggest"
    LOG_TAIL = "log_tail"
    TUNNEL_EVENTS = "tunnel_events"
    TUNNEL_SET_POST_CONNECT = "tunnel_set_post_connect"
    TUNNEL_SET_TAGS = "tunnel_set_tags"
    TUNNEL_RENAME = "tunnel_rename"
    TUNNELS_BATCH = "tunnels_batch"
    RESET_ALL = "reset_all"


class Event:
    HOST_STATUS_CHANGED = "host_status_changed"
    TUNNEL_STATUS_CHANGED = "tunnel_status_changed"
    NOTIFICATION = "notification"


# --- Error codes --------------------------------------------------------------

class ErrCode:
    INVALID_REQUEST = "invalid_request"
    UNKNOWN_METHOD = "unknown_method"
    BAD_PARAMS = "bad_params"
    NOT_FOUND = "not_found"
    PORT_IN_USE = "port_in_use"
    DUPLICATE = "duplicate"
    DISCOVERY_FAILED = "discovery_failed"
    INTERNAL = "internal"


# --- Encode / decode ---------------------------------------------------------

def encode(obj: Any) -> bytes:
    """JSON-serialise an object and append a newline."""
    return (json.dumps(obj, default=str) + "\n").encode("utf-8")


def decode(line: bytes) -> Any:
    return json.loads(line.decode("utf-8"))


def make_request(req_id: str, method: str, params: Optional[dict] = None) -> dict:
    return {"id": req_id, "method": method, "params": params or {}}


def make_response(req_id: str, result: Any = None) -> dict:
    return {"id": req_id, "result": result}


def make_error(req_id: str, code: str, message: str) -> dict:
    return {"id": req_id, "error": {"code": code, "message": message}}


def make_event(name: str, data: dict) -> dict:
    return {"event": name, "data": data}
