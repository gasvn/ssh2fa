"""Tests for daemon IPC robustness fixes (M10, M12) and the free-port
probe (L4). The daemon uses relative imports, so it must be imported as the
`auto2fa.daemon` package — we put the repo root on sys.path for that."""
from __future__ import annotations

import asyncio
import json
import os
import socket
import sys
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from auto2fa import daemon as daemon_mod  # noqa: E402
from auto2fa import ipc  # noqa: E402


class _FakeWriter:
    def __init__(self):
        self.buf = b""
    def write(self, b):
        self.buf += b
    async def drain(self):
        pass
    def close(self):
        pass
    async def wait_closed(self):
        pass
    def get_extra_info(self, _k):
        return "<test>"


def _run_client_with(line: bytes, limit: int = 64 * 1024) -> list[dict]:
    """Feed one raw line into _handle_client and return decoded responses."""
    async def go():
        d = daemon_mod.Auto2FADaemon()
        reader = asyncio.StreamReader(limit=limit)
        reader.feed_data(line)
        reader.feed_eof()
        w = _FakeWriter()
        await d._handle_client(reader, w)
        return w.buf

    buf = asyncio.run(go())
    out = []
    for chunk in buf.split(b"\n"):
        if chunk.strip():
            out.append(json.loads(chunk.decode("utf-8")))
    return out


class TestNonObjectRequest(unittest.TestCase):
    """M10: a valid-JSON but non-object request must get an error, not crash
    the connection silently."""

    def test_bare_number_gets_invalid_request(self):
        resps = _run_client_with(b"5\n")
        self.assertEqual(len(resps), 1)
        self.assertIn("error", resps[0])
        self.assertEqual(resps[0]["error"]["code"], ipc.ErrCode.INVALID_REQUEST)

    def test_json_array_gets_invalid_request(self):
        resps = _run_client_with(b"[1, 2, 3]\n")
        self.assertEqual(len(resps), 1)
        self.assertIn("error", resps[0])
        self.assertEqual(resps[0]["error"]["code"], ipc.ErrCode.INVALID_REQUEST)

    def test_bad_json_still_handled(self):
        resps = _run_client_with(b"{not json\n")
        self.assertEqual(len(resps), 1)
        self.assertIn("error", resps[0])
        self.assertEqual(resps[0]["error"]["code"], ipc.ErrCode.INVALID_REQUEST)

    def test_invalid_utf8_gets_invalid_request(self):
        """Invalid UTF-8 bytes raise UnicodeDecodeError (NOT JSONDecodeError);
        the handler must reply with an error instead of crashing the
        connection silently (regression)."""
        resps = _run_client_with(b"\xff\xfe{\"id\":\"1\"}\n")
        self.assertEqual(len(resps), 1)
        self.assertIn("error", resps[0])
        self.assertEqual(resps[0]["error"]["code"], ipc.ErrCode.INVALID_REQUEST)


class TestOversizedRequest(unittest.TestCase):
    """M12: a line larger than the stream limit must yield a clean error,
    not crash the whole connection with no reply."""

    def test_oversized_line_gets_error(self):
        big = b'{"id":"x","method":"ping","params":{"junk":"' + b"A" * 70000 + b'"}}\n'
        resps = _run_client_with(big, limit=64 * 1024)
        self.assertTrue(resps, "must reply with an error, not silently drop")
        self.assertIn("error", resps[0])
        self.assertEqual(resps[0]["error"]["code"], ipc.ErrCode.INVALID_REQUEST)


class TestFindFreePort(unittest.TestCase):
    """L4: the probe must report a port actually free for exclusive bind —
    a currently-bound port must be skipped."""

    def test_skips_a_bound_port(self):
        d = daemon_mod.Auto2FADaemon()
        # Bind a port and hold it; the probe must not return it.
        s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        s.bind(("127.0.0.1", 0))
        s.listen(1)
        held = s.getsockname()[1]
        try:
            got = d._find_free_port(held, taken=set())
            self.assertNotEqual(got, held,
                                "a bound port must not be reported as free")
        finally:
            s.close()

    def test_skips_taken_set(self):
        d = daemon_mod.Auto2FADaemon()
        base = 49000
        got = d._find_free_port(base, taken={base, base + 1})
        self.assertNotIn(got, {base, base + 1})


class TestHostNameValidation(unittest.TestCase):
    """A host name flows into a Keychain key, ssh alias, /tmp log path, and
    the ~/Mounts/<host> sshfs path — names with '/' or '..' must be rejected
    so they can't traverse out of those locations."""

    def test_accepts_normal_names(self):
        for h in ("k6", "k7", "b8", "login01", "gpu-node_1", "a.b.c"):
            self.assertTrue(daemon_mod._valid_host_name(h), h)

    def test_rejects_traversal_and_separators(self):
        for h in ("../../../tmp/pwned", "a/b", "..", ".", "", "a..b",
                  "/etc", "x/../y", "-leading-dash"):
            self.assertFalse(daemon_mod._valid_host_name(h), h)

    def test_rejects_non_string(self):
        self.assertFalse(daemon_mod._valid_host_name(None))
        self.assertFalse(daemon_mod._valid_host_name(5))


class TestListTunnelsNoneFilter(unittest.TestCase):
    """list_tunnels snapshots each tunnel once; a concurrent remove between
    the old filter-call and body-call used to leak a None into the result."""

    def test_list_tunnels_never_returns_none(self):
        d = daemon_mod.Auto2FADaemon()

        class _TM:
            tunnels = {"a": object(), "b": object()}

        d.tunnel_mgr = _TM()
        # Simulate tunnel 'b' having just been removed: its snapshot is None.
        d._tunnel_snapshot = lambda n: None if n == "b" else {"name": n}
        out = d.list_tunnels()
        self.assertNotIn(None, out)
        self.assertEqual(out, [{"name": "a"}])


if __name__ == "__main__":
    unittest.main()
