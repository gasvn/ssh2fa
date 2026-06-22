"""auto2fa CLI — a small Unix-socket client for scripting the daemon.

Install (until packaged): symlink into PATH from your checkout, e.g.
    ln -s "$(pwd)/auto2fa/cli.py" ~/bin/auto2fa
    chmod +x ~/bin/auto2fa

Examples:
    auto2fa list                        # show hosts + tunnels
    auto2fa hosts                       # hosts only
    auto2fa tunnels                     # tunnels only
    auto2fa start jupyter               # start a tunnel
    auto2fa stop jupyter
    auto2fa node jupyter holygpu08      # set tunnel node and start
    auto2fa wake                        # fire wake_recover
    auto2fa logs                        # tail daemon log (--lines N)
    auto2fa raw list_hosts              # send a raw RPC

Exit codes: 0 on success, 1 on transport error, 2 on daemon error.
"""
from __future__ import annotations

import argparse
import json
import os
import socket
import sys
import uuid


SOCKET_PATH = os.path.expanduser("~/.auto2fa/auto2fa.sock")


def _rpc(method: str, params: dict | None = None) -> dict:
    """One-shot request/response over the Unix socket."""
    if not os.path.exists(SOCKET_PATH):
        print(f"daemon socket not found at {SOCKET_PATH} — is the daemon running?",
              file=sys.stderr)
        sys.exit(1)
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    # Bound every blocking socket op. A wedged daemon (e.g. event loop stuck
    # behind a long start() probe) would otherwise hang the CLI forever with
    # no feedback. 30s is well above any legitimate request.
    s.settimeout(30)
    try:
        s.connect(SOCKET_PATH)
    except OSError as e:
        print(f"connect failed: {e}", file=sys.stderr)
        sys.exit(1)
    req_id = uuid.uuid4().hex
    req = json.dumps({"id": req_id, "method": method, "params": params or {}}) + "\n"
    try:
        s.sendall(req.encode("utf-8"))
        # read until newline
        buf = b""
        while b"\n" not in buf:
            chunk = s.recv(65536)
            if not chunk:
                break
            buf += chunk
    except socket.timeout:
        print("daemon did not respond within 30s — it may be wedged; "
              "try `auto2fa` again or restart the app", file=sys.stderr)
        sys.exit(1)
    except OSError as e:
        # sendall/recv can raise BrokenPipeError/OSError (not just timeout)
        # if the daemon dies mid-exchange — report cleanly, don't traceback.
        print(f"lost connection to daemon: {e}", file=sys.stderr)
        sys.exit(1)
    finally:
        s.close()
    line, _, _ = buf.partition(b"\n")
    if not line:
        print("daemon closed connection without responding", file=sys.stderr)
        sys.exit(1)
    try:
        resp = json.loads(line.decode("utf-8"))
    except (json.JSONDecodeError, UnicodeDecodeError) as e:
        print(f"daemon sent a malformed response: {e}", file=sys.stderr)
        sys.exit(1)
    if "error" in resp:
        print(f"daemon error: {resp['error'].get('message', 'unknown')}",
              file=sys.stderr)
        sys.exit(2)
    return resp.get("result")


def _color(s: str, code: str) -> str:
    if not sys.stdout.isatty():
        return s
    return f"\x1b[{code}m{s}\x1b[0m"


def _status_glyph(state: str) -> str:
    if state in ("alive", "connected"): return _color("●", "32")
    if state in ("starting", "connecting"): return _color("◐", "33")
    if state in ("failed", "stale", "port_busy"): return _color("●", "31")
    return _color("○", "37")


def cmd_list(args):
    cmd_hosts(args)
    print()
    cmd_tunnels(args)


def cmd_hosts(args):
    hosts = _rpc("list_hosts")
    print(_color("HOSTS", "1"))
    for h in hosts:
        # status field is rich-markup; strip naively
        status = h.get("status", "").replace("[", "").replace("]", "")
        print(f"  {_status_glyph('connected' if h.get('is_master_ready') else 'stopped')} "
              f"{h['host']:<40} pool={h.get('pool_index')}/{h.get('pool_alive')}  "
              f"{h.get('last_msg', '')[:50]}")


def cmd_tunnels(args):
    tunnels = _rpc("list_tunnels")
    print(_color("TUNNELS", "1"))
    if not tunnels:
        print("  (none)")
        return
    for t in tunnels:
        auto = "⚡" if t.get("auto_start") else " "
        pinned = "📌" if t.get("jump_candidates") else " "
        print(f"  {_status_glyph(t['status'])} {auto}{pinned} "
              f"{t['name']:<20} :{t['local_port']}→:{t['remote_port']} "
              f"via {t.get('active_jump') or '—':<10} "
              f"node={t.get('last_node') or '—'}  "
              f"{t.get('last_msg', '')[:30]}")


def cmd_start(args):
    # Use the idempotent tunnel_start RPC, not tunnel_toggle (which would
    # STOP an already-alive tunnel — confusing in shutdown scripts).
    _rpc("tunnel_start", {"name": args.name})
    print(f"start({args.name}): OK")


def cmd_stop(args):
    _rpc("tunnel_stop", {"name": args.name})
    print(f"stop({args.name}): OK")


def cmd_toggle(args):
    _rpc("tunnel_toggle", {"name": args.name})
    print(f"toggle({args.name}): OK")


def cmd_node(args):
    res = _rpc("tunnel_set_node", {
        "name": args.name, "node": args.node, "user": args.user or os.environ.get("USER", "")
    })
    print(f"set_node({args.name}, {args.node}): OK")


def cmd_wake(args):
    res = _rpc("wake_recover")
    print(f"wake_recover: restarting {len(res.get('tunnels_restarting', []))} tunnels")


def cmd_logs(args):
    res = _rpc("log_tail", {"lines": args.lines})
    for line in res.get("lines", []):
        print(line)


def cmd_raw(args):
    try:
        params = json.loads(args.params) if args.params else {}
    except json.JSONDecodeError as e:
        print(f"invalid JSON for --params: {e}", file=sys.stderr)
        sys.exit(1)
    res = _rpc(args.method, params)
    print(json.dumps(res, indent=2))


def cmd_install(args):
    from . import installer
    sys.exit(installer.install())


def main():
    # When invoked with no arguments, launch the interactive TUI directly.
    if len(sys.argv) == 1:
        from .main import main as tui_main
        tui_main()
        return

    p = argparse.ArgumentParser(prog="auto2fa", description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)

    sub.add_parser("list", help="list hosts + tunnels").set_defaults(func=cmd_list)
    sub.add_parser("hosts", help="list hosts").set_defaults(func=cmd_hosts)
    sub.add_parser("tunnels", help="list tunnels").set_defaults(func=cmd_tunnels)

    sp = sub.add_parser("start", help="start a tunnel (idempotent — no-op if already alive)")
    sp.add_argument("name")
    sp.set_defaults(func=cmd_start)

    sp = sub.add_parser("stop", help="stop a tunnel (idempotent — no-op if already stopped)")
    sp.add_argument("name")
    sp.set_defaults(func=cmd_stop)

    sp = sub.add_parser("toggle", help="flip a tunnel between alive and stopped")
    sp.add_argument("name")
    sp.set_defaults(func=cmd_toggle)

    sp = sub.add_parser("node", help="set node for tunnel (starts it)")
    sp.add_argument("name")
    sp.add_argument("node")
    sp.add_argument("--user", default=None)
    sp.set_defaults(func=cmd_node)

    sub.add_parser("wake", help="trigger wake_recover").set_defaults(func=cmd_wake)

    sp = sub.add_parser("logs", help="tail daemon log")
    sp.add_argument("--lines", type=int, default=50)
    sp.set_defaults(func=cmd_logs)

    sp = sub.add_parser("raw", help="raw RPC: method + JSON params")
    sp.add_argument("method")
    sp.add_argument("params", nargs="?", default=None,
                    help='JSON object, e.g. \'{"host":"k6"}\'')
    sp.set_defaults(func=cmd_raw)

    sub.add_parser(
        "install",
        help="generate + load this machine's deploy artifacts (no daemon needed)"
    ).set_defaults(func=cmd_install)

    args = p.parse_args(sys.argv[1:])
    args.func(args)


if __name__ == "__main__":
    main()
