#!/usr/bin/env python3
"""Auto2FA Dashboard — Textual TUI."""
from __future__ import annotations

import json
import logging
import os
import subprocess
import sys
import threading
import time
import urllib.parse

from dotenv import load_dotenv

load_dotenv()

import pyotp
from textual.app import App, ComposeResult
from textual.binding import Binding
from textual.containers import Horizontal, Vertical
from textual.screen import ModalScreen
from rich.text import Text
from textual.widgets import Button, DataTable, Footer, Header, Input, Label, Static
from textual.widgets.data_table import RowKey

from .backend import SSHHostManager, extract_secret_from_url
from .tunnels import DiscoveryError, NodeDiscovery, TunnelManager, expand_first_node

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s - %(threadName)s - %(levelname)s - %(message)s",
    handlers=[logging.FileHandler("/tmp/auto2fa_dashboard.log")],
)
logger = logging.getLogger(__name__)


# ---------- Config loading ----------

def load_hosts():
    try:
        config_path = os.environ.get("SSH_CONFIG_PATH")
        assert config_path, "SSH_CONFIG_PATH environment variable is not set"
        with open(f"{config_path}/passwords.json", "r") as f:
            return json.load(f)
    except Exception as e:
        print(f"Failed to load config: {e}")
        sys.exit(1)


def ssh_config_user(host: str) -> str:
    try:
        res = subprocess.run(["ssh", "-G", host], capture_output=True, text=True, timeout=2)
        for line in res.stdout.splitlines():
            if line.lower().startswith("user "):
                return line.split(" ", 1)[1].strip()
    except Exception:
        pass
    return ""


# ---------- Modals ----------

class NewTunnelScreen(ModalScreen[tuple[str, int] | None]):
    """Form: name + local port. Returns (name, port) or None on cancel."""

    DEFAULT_CSS = """
    NewTunnelScreen { align: center middle; }
    NewTunnelScreen > Vertical {
        width: 60; height: auto; padding: 1 2;
        border: thick $accent; background: $panel;
    }
    NewTunnelScreen Label.title { content-align: center middle; padding-bottom: 1; }
    NewTunnelScreen Label.field { padding-top: 1; color: $text-muted; }
    NewTunnelScreen Label.error { color: $error; padding-top: 1; }
    NewTunnelScreen Label.hint  { color: $text-muted; padding-top: 1; content-align: center middle; }
    NewTunnelScreen Horizontal.buttons {
        height: 3; align: center middle; padding-top: 1;
    }
    NewTunnelScreen Button { margin: 0 1; }
    """

    BINDINGS = [
        Binding("escape", "cancel", "Cancel"),
        Binding("ctrl+s", "submit", "Submit"),
    ]

    def compose(self) -> ComposeResult:
        with Vertical():
            yield Label("[b]New Tunnel[/b]", classes="title")
            yield Label("Name", classes="field")
            yield Input(placeholder="jupyter", id="name")
            yield Label("Local port", classes="field")
            yield Input(placeholder="8888", id="port")
            yield Label("", id="err", classes="error")
            with Horizontal(classes="buttons"):
                yield Button("Submit", variant="primary", id="submit_btn")
                yield Button("Cancel", id="cancel_btn")
            yield Label(
                "Tab: next field   Enter: submit   Ctrl+S: submit   Esc: cancel",
                classes="hint",
            )

    def on_mount(self) -> None:
        self.query_one("#name", Input).focus()

    def action_cancel(self) -> None:
        self.dismiss(None)

    def action_submit(self) -> None:
        self._submit()

    def on_input_submitted(self, event: Input.Submitted) -> None:
        # Enter on either field submits if both are filled; otherwise focus the next one.
        name = self.query_one("#name", Input).value.strip()
        port = self.query_one("#port", Input).value.strip()
        if name and port:
            self._submit()
        elif event.input.id == "name":
            self.query_one("#port", Input).focus()
        else:
            self.query_one("#name", Input).focus()

    def on_button_pressed(self, event: Button.Pressed) -> None:
        if event.button.id == "submit_btn":
            self._submit()
        elif event.button.id == "cancel_btn":
            self.dismiss(None)

    def _submit(self) -> None:
        name = self.query_one("#name", Input).value.strip()
        port_str = self.query_one("#port", Input).value.strip()
        err = self.query_one("#err", Label)
        if not name:
            err.update("Name cannot be empty.")
            self.query_one("#name", Input).focus()
            return
        if not port_str:
            err.update("Local port is required.")
            self.query_one("#port", Input).focus()
            return
        try:
            port = int(port_str)
        except ValueError:
            err.update("Local port must be an integer.")
            self.query_one("#port", Input).focus()
            return
        self.dismiss((name, port))


class CustomNodeScreen(ModalScreen[tuple[str, str] | None]):
    """Form: custom node + user. Returns (node, user) or None."""

    DEFAULT_CSS = """
    CustomNodeScreen { align: center middle; }
    CustomNodeScreen > Vertical {
        width: 70; height: auto; padding: 1 2;
        border: thick $accent; background: $panel;
    }
    CustomNodeScreen Label.title { content-align: center middle; padding-bottom: 1; }
    CustomNodeScreen Label.field { padding-top: 1; color: $text-muted; }
    CustomNodeScreen Label.hint  { color: $text-muted; padding-top: 1; content-align: center middle; }
    CustomNodeScreen Horizontal.buttons {
        height: 3; align: center middle; padding-top: 1;
    }
    CustomNodeScreen Button { margin: 0 1; }
    """

    BINDINGS = [
        Binding("escape", "cancel", "Cancel"),
        Binding("ctrl+s", "submit", "Submit"),
    ]

    def __init__(self, default_user: str = ""):
        super().__init__()
        self.default_user = default_user

    def compose(self) -> ComposeResult:
        with Vertical():
            yield Label("[b]Custom node[/b]", classes="title")
            yield Label("Node", classes="field")
            yield Input(placeholder="holygpu8a11103.rc.fas.harvard.edu", id="node")
            yield Label("User", classes="field")
            yield Input(value=self.default_user, placeholder=os.environ.get("USER", ""), id="user")
            with Horizontal(classes="buttons"):
                yield Button("Submit", variant="primary", id="submit_btn")
                yield Button("Cancel", id="cancel_btn")
            yield Label("Tab: next   Enter: submit   Esc: cancel", classes="hint")

    def on_mount(self) -> None:
        self.query_one("#node", Input).focus()

    def action_cancel(self) -> None:
        self.dismiss(None)

    def action_submit(self) -> None:
        self._submit()

    def on_input_submitted(self, event: Input.Submitted) -> None:
        node = self.query_one("#node", Input).value.strip()
        if node:
            self._submit()
        else:
            self.query_one("#node", Input).focus()

    def on_button_pressed(self, event: Button.Pressed) -> None:
        if event.button.id == "submit_btn":
            self._submit()
        elif event.button.id == "cancel_btn":
            self.dismiss(None)

    def _submit(self) -> None:
        node = self.query_one("#node", Input).value.strip()
        user = self.query_one("#user", Input).value.strip() or os.environ.get("USER", "")
        if node:
            self.dismiss((node, user))


class NodePickerScreen(ModalScreen[tuple[str, str, bool] | None]):
    """Pick a compute node from `squeue`. Returns (node, user, is_range) or None."""

    DEFAULT_CSS = """
    NodePickerScreen { align: center middle; }
    NodePickerScreen > Vertical {
        width: 100; height: 30; padding: 1 2;
        border: thick $accent; background: $panel;
    }
    NodePickerScreen Label.title { content-align: center middle; padding-bottom: 1; }
    NodePickerScreen Label.hint  { color: $text-muted; padding-top: 1; content-align: center middle; }
    NodePickerScreen Label.err   { color: $error; padding-top: 1; content-align: center middle; }
    NodePickerScreen DataTable   { height: 1fr; }
    """

    BINDINGS = [
        Binding("escape", "cancel", "Cancel"),
        Binding("r", "refresh", "Refresh"),
        Binding("c", "custom", "Custom"),
    ]

    def __init__(self, tunnel_mgr, tunnel_name: str):
        super().__init__()
        self.tunnel_mgr = tunnel_mgr
        self.tunnel_name = tunnel_name
        self.jobs: list = []
        self.error_msg: str = ""
        self.jump_name: str | None = None
        self._loading: bool = False

    def compose(self) -> ComposeResult:
        with Vertical():
            yield Label(f"[b]Pick node for '{self.tunnel_name}'[/b]", classes="title", id="title")
            yield DataTable(id="jobs", cursor_type="row")
            yield Label("", id="err", classes="err")
            yield Label(
                "↑↓ move   Enter use   R refresh   C custom   Esc cancel",
                classes="hint",
            )

    def on_mount(self) -> None:
        table = self.query_one("#jobs", DataTable)
        table.add_columns("JobID", "Partition", "Name", "Time", "Node")
        table.focus()
        # Pick the jump immediately (fast: just reads in-memory state)
        ts = self.tunnel_mgr.tunnels[self.tunnel_name]
        self.jump_name = self.tunnel_mgr.pick_active_jump(ts)
        title = self.query_one("#title", Label)
        if self.jump_name is None:
            title.update(f"[b]Pick node for '{self.tunnel_name}'[/b]")
            self.query_one("#err", Label).update(
                "No connected jump host. Press Esc and start a host first."
            )
            return
        title.update(f"[b]Pick node for '{self.tunnel_name}' via {self.jump_name}[/b]")
        self._kick_off_refresh()

    def _kick_off_refresh(self) -> None:
        """Launch a background squeue. Keep the UI responsive."""
        if self._loading or self.jump_name is None:
            return
        self._loading = True
        self.query_one("#err", Label).update("[dim]Loading jobs from squeue…[/dim]")
        self.query_one("#jobs", DataTable).clear()
        mgr = self.tunnel_mgr.host_managers[self.jump_name]

        def worker():
            try:
                jobs = NodeDiscovery.discover(mgr)
                self.app.call_from_thread(self._on_jobs_loaded, jobs, None)
            except DiscoveryError as e:
                self.app.call_from_thread(self._on_jobs_loaded, [], str(e))
            except Exception as e:
                self.app.call_from_thread(self._on_jobs_loaded, [], f"unexpected: {e}")

        threading.Thread(target=worker, daemon=True).start()

    def _on_jobs_loaded(self, jobs, error_msg) -> None:
        self._loading = False
        self.jobs = jobs
        self.error_msg = error_msg or ""
        err = self.query_one("#err", Label)
        table = self.query_one("#jobs", DataTable)
        table.clear()
        if error_msg:
            err.update(f"[red]squeue failed:[/] {error_msg[:80]}   Press C for custom node.")
            return
        if not jobs:
            err.update("No running jobs. Press C to enter a node manually.")
            return
        err.update("")
        for j in jobs:
            table.add_row(j.jobid, j.partition, j.name, j.time, j.node)
        table.focus()

    def action_refresh(self) -> None:
        self._kick_off_refresh()

    def action_cancel(self) -> None:
        self.dismiss(None)

    def action_custom(self) -> None:
        default_user = (
            ssh_config_user(self.jump_name or "")
            or self.tunnel_mgr.tunnels[self.tunnel_name].last_user
            or os.environ.get("USER", "")
        )

        def on_custom(result: tuple[str, str] | None) -> None:
            if result:
                node, user = result
                self.dismiss((node, user, False))

        self.app.push_screen(CustomNodeScreen(default_user), on_custom)

    def on_data_table_row_selected(self, event: DataTable.RowSelected) -> None:
        if not self.jobs or self.jump_name is None:
            return
        row = self.query_one("#jobs", DataTable).cursor_row
        if not (0 <= row < len(self.jobs)):
            return
        job = self.jobs[row]
        node, is_range = expand_first_node(job.node)
        user = (
            ssh_config_user(self.jump_name)
            or self.tunnel_mgr.tunnels[self.tunnel_name].last_user
            or os.environ.get("USER", "")
        )
        self.dismiss((node, user, is_range))


class ConfirmScreen(ModalScreen[bool]):
    """Yes/No confirmation."""

    DEFAULT_CSS = """
    ConfirmScreen { align: center middle; }
    ConfirmScreen > Vertical {
        width: 60; height: auto; padding: 1 2;
        border: thick $error; background: $panel;
    }
    ConfirmScreen Label.q { content-align: center middle; padding: 1 0; }
    ConfirmScreen Label.hint { color: $text-muted; content-align: center middle; }
    """

    BINDINGS = [
        Binding("y", "confirm(True)", "Yes"),
        Binding("n,escape", "confirm(False)", "No"),
    ]

    def __init__(self, message: str):
        super().__init__()
        self.message = message

    def compose(self) -> ComposeResult:
        with Vertical():
            yield Label(self.message, classes="q")
            yield Label("[Y]es   [N]o / [Esc]", classes="hint")

    def action_confirm(self, value: bool) -> None:
        self.dismiss(value)


# ---------- Custom DataTables (own their keybindings) ----------

class HostTable(DataTable):
    """Host list. Keybindings only fire when this table is focused,
    so they never conflict with Input widgets in modals."""

    BINDINGS = [
        Binding("space", "host_toggle", "Start/Stop"),
        Binding("m", "host_mount", "Mount"),
        Binding("r", "host_rotate", "Rotate"),
    ]

    def action_host_toggle(self) -> None:
        self.app.action_toggle_host()

    def action_host_mount(self) -> None:
        self.app.action_mount_host()

    def action_host_rotate(self) -> None:
        self.app.action_rotate_host()


class TunnelTable(DataTable):
    """Tunnel list. T/D/space/Enter only active when this table is focused."""

    BINDINGS = [
        Binding("space", "tunnel_toggle", "Start/Stop"),
        Binding("t", "tunnel_new", "New tunnel"),
        Binding("d", "tunnel_delete", "Delete"),
    ]

    def action_tunnel_toggle(self) -> None:
        self.app.action_toggle_tunnel()

    def action_tunnel_new(self) -> None:
        self.app.action_new_tunnel()

    def action_tunnel_delete(self) -> None:
        self.app.action_delete_tunnel()


# ---------- Main app ----------

class Auto2FAApp(App):
    CSS = """
    Screen { layout: vertical; }
    #hosts_table, #tunnels_table { height: 1fr; }
    .section-title { background: $primary 30%; color: $text; padding: 0 1; }
    """

    # Only truly global bindings. Per-table keys live on HostTable / TunnelTable
    # so they don't interfere with Input widgets in modals.
    BINDINGS = [
        Binding("q", "quit", "Quit"),
        Binding("ctrl+n", "new_tunnel", "New tunnel"),  # global shortcut for T
    ]

    def __init__(self, managers, tunnel_mgr):
        super().__init__()
        self.managers = managers
        self.tunnel_mgr = tunnel_mgr
        self._host_row_keys: list[RowKey] = []
        self._tunnel_names: list[str] = []
        self._tick_stop = threading.Event()
        self._tick_thread: threading.Thread | None = None
        # Cache last-rendered "fingerprint" to skip redundant table rebuilds.
        self._last_host_fp: tuple = ()
        self._last_tunnel_fp: tuple = ()

    def compose(self) -> ComposeResult:
        yield Header(show_clock=True)
        yield Label("HOSTS", classes="section-title")
        yield HostTable(id="hosts_table", cursor_type="row")
        yield Label("TUNNELS  (Tab to focus)", classes="section-title")
        yield TunnelTable(id="tunnels_table", cursor_type="row")
        yield Footer()

    def on_mount(self) -> None:
        self.title = "Auto2FA"
        hosts = self.query_one("#hosts_table", HostTable)
        hosts.add_columns("Host", "Status", "Pool", "FS", "Last Message")
        tunnels = self.query_one("#tunnels_table", TunnelTable)
        tunnels.add_columns("Name", "Local→Remote", "Node", "Via", "Status")
        hosts.focus()
        self._refresh_tables()
        # Render-only timer (cheap, runs on UI thread)
        self.set_interval(0.5, self._safe_refresh_tables)
        # tick() can block (start() probes for up to 10s) — run it on its own thread
        self._tick_thread = threading.Thread(target=self._tick_loop, daemon=True)
        self._tick_thread.start()

    def on_unmount(self) -> None:
        self._tick_stop.set()

    def _tick_loop(self) -> None:
        """Background driver — never blocks the Textual event loop."""
        while not self._tick_stop.is_set():
            try:
                self.tunnel_mgr.tick()
            except Exception as e:
                logger.error(f"tunnel_mgr.tick failed: {e}")
            self._tick_stop.wait(0.5)

    def _safe_refresh_tables(self) -> None:
        # Don't churn the tables while a modal is up — it can steal focus.
        if len(self.screen_stack) > 1:
            return
        self._refresh_tables()

    # ---- Rendering ----

    def _refresh_tables(self) -> None:
        self._refresh_hosts()
        self._refresh_tunnels()

    def _refresh_hosts(self) -> None:
        # Compute a fingerprint of the rendered state. Skip the rebuild if nothing changed.
        rows = []
        for mgr in self.managers:
            try:
                pool_snapshot = list(mgr.pool.values())
                alive = sum(1 for c in pool_snapshot if c.isalive())
                pool = f"{mgr.active_index}/{alive}"
            except Exception:
                pool = "?"
            fs = "📂" if ("Mounted" in mgr.last_msg or "Mounting" in mgr.last_msg) else ""
            # Use raw status string in the fingerprint (markup-aware compare unnecessary)
            rows.append((mgr.host, mgr.status, pool, fs, mgr.last_msg))
        fp = tuple(rows)
        if fp == self._last_host_fp:
            return
        self._last_host_fp = fp

        table = self.query_one("#hosts_table", HostTable)
        prev_row = table.cursor_row
        table.clear()
        self._host_row_keys = []
        for mgr, (_, _, pool, fs, last_msg) in zip(self.managers, rows):
            status_text = self._render_host_status(mgr)
            key = table.add_row(mgr.host, status_text, pool, fs, last_msg)
            self._host_row_keys.append(key)
        if 0 <= prev_row < len(self.managers):
            table.move_cursor(row=prev_row)

    @staticmethod
    def _render_host_status(mgr) -> Text:
        """Build a colored Text cell for the Status column with a leading glyph."""
        raw = mgr.status
        # Strip any pre-existing rich markup so we can apply our own.
        import re
        plain = re.sub(r"\[/?[^\]]+\]", "", raw).strip() or ("Active" if mgr.active else "Stopped")
        lc = plain.lower()
        if "connected" in lc or "active" in lc and "init" not in lc and "fail" not in lc:
            glyph, color = "●", "green"
        elif "init" in lc or "connecting" in lc or "spawn" in lc or "starting" in lc:
            glyph, color = "◐", "yellow"
        elif "fail" in lc or "error" in lc or "crash" in lc:
            glyph, color = "●", "red"
        elif "stopped" in lc or "inactive" in lc:
            glyph, color = "○", "grey50"
        else:
            glyph, color = "•", "white"
        t = Text()
        t.append(f"{glyph} ", style=color)
        t.append(plain, style=color)
        return t

    def _refresh_tunnels(self) -> None:
        names = list(self.tunnel_mgr.tunnels.keys())
        rows = []
        for name in names:
            ts = self.tunnel_mgr.tunnels[name]
            rows.append((
                name, ts.local_port, ts.remote_port,
                ts.last_node, ts.active_jump, ts.status, ts.last_msg,
            ))
        fp = tuple(rows)
        if fp == self._last_tunnel_fp and self._tunnel_names == names:
            return
        self._last_tunnel_fp = fp
        self._tunnel_names = names

        table = self.query_one("#tunnels_table", TunnelTable)
        prev_row = table.cursor_row
        table.clear()
        if not names:
            table.add_row(
                Text("(no tunnels — press T to create one)", style="grey50"),
                "", "", "", "",
            )
            return
        for name, row in zip(names, rows):
            _, lp, rp, last_node, active_jump, status, last_msg = row
            ports = f":{lp}→:{rp}"
            node = last_node or Text("(no node yet)", style="grey50")
            via = active_jump or "—"
            glyph, color = {
                "alive": ("●", "green"),
                "starting": ("◐", "yellow"),
                "stale": ("○", "red"),
                "idle": ("○", "grey50"),
                "port_busy": ("●", "red"),
                "failed": ("●", "red"),
            }.get(status, ("?", "white"))
            status_cell = Text()
            status_cell.append(f"{glyph} {status}", style=color)
            status_cell.append(f"   {last_msg}", style="grey50")
            table.add_row(name, ports, node, via, status_cell)
        if 0 <= prev_row < len(names):
            table.move_cursor(row=prev_row)

    # ---- Helpers ----

    def _selected_host(self):
        table = self.query_one("#hosts_table", HostTable)
        row = table.cursor_row
        if 0 <= row < len(self.managers):
            return self.managers[row]
        return None

    def _selected_tunnel_name(self) -> str | None:
        table = self.query_one("#tunnels_table", TunnelTable)
        row = table.cursor_row
        if 0 <= row < len(self._tunnel_names):
            return self._tunnel_names[row]
        return None

    # ---- Actions (called by HostTable / TunnelTable bindings or events) ----

    def action_toggle_host(self) -> None:
        mgr = self._selected_host()
        if mgr:
            mgr.toggle()

    def action_toggle_tunnel(self) -> None:
        name = self._selected_tunnel_name()
        if name:
            self.tunnel_mgr.toggle(name)
            self._refresh_tunnels()

    def action_mount_host(self) -> None:
        mgr = self._selected_host()
        if mgr:
            threading.Thread(target=mgr.mount_host, daemon=True).start()

    def action_rotate_host(self) -> None:
        mgr = self._selected_host()
        if mgr and mgr.active:
            new_idx = (mgr.active_index + 1) % 2
            if hasattr(mgr, "update_symlink"):
                mgr.update_symlink(new_idx)
                mgr.last_msg = f"Manual Rotate -> {new_idx}"

    def action_new_tunnel(self) -> None:
        def on_done(result: tuple[str, int] | None) -> None:
            if not result:
                return
            name, port = result
            try:
                self.tunnel_mgr.add(name=name, local_port=port)
                self._refresh_tunnels()
                self.query_one("#tunnels_table", TunnelTable).focus()
                if name in self._tunnel_names:
                    self.query_one("#tunnels_table", TunnelTable).move_cursor(
                        row=self._tunnel_names.index(name)
                    )
                self.notify(f"Tunnel '{name}' created — press Enter to pick a node.")
            except (ValueError, KeyError) as e:
                self.notify(str(e), severity="error", timeout=5)

        self.push_screen(NewTunnelScreen(), on_done)

    def on_data_table_row_selected(self, event: DataTable.RowSelected) -> None:
        """Enter on a tunnel row opens the node picker."""
        if event.data_table.id != "tunnels_table":
            return
        self.action_pick_node()

    def action_pick_node(self) -> None:
        name = self._selected_tunnel_name()
        if not name:
            return

        def on_picked(result: tuple[str, str, bool] | None) -> None:
            if not result:
                return
            node, user, is_range = result
            # set_node may call start() which probes a port for up to 10s.
            # Run it off the UI thread so we don't freeze.
            def do_set():
                self.tunnel_mgr.set_node(name, node, user)
                if is_range:
                    self.tunnel_mgr.tunnels[name].last_msg += " (picked first of range)"
                # Refresh on the UI thread when done
                self.call_from_thread(self._refresh_tunnels)
            threading.Thread(target=do_set, daemon=True).start()

        self.push_screen(NodePickerScreen(self.tunnel_mgr, name), on_picked)

    def action_delete_tunnel(self) -> None:
        name = self._selected_tunnel_name()
        if not name:
            return

        def on_confirm(yes: bool) -> None:
            if yes:
                self.tunnel_mgr.stop(name)
                self.tunnel_mgr.remove(name)
                self._refresh_tunnels()

        self.push_screen(ConfirmScreen(f"Delete tunnel '{name}'?"), on_confirm)


# ---------- Entry point ----------

def main():
    config = load_hosts()
    managers = []
    for host, creds in config.items():
        if "otpauthUrl" in creds:
            secret = extract_secret_from_url(creds["otpauthUrl"])
            mgr = SSHHostManager(host, creds["password"], secret)
            mgr.daemon = True
            mgr.active = creds.get("autoConnect", creds.get("auto_connect", False))
            mgr.start()
            managers.append(mgr)

    if not managers:
        print("No hosts found in passwords.json")
        sys.exit(1)

    host_map = {m.host: m for m in managers}
    config_path = os.environ.get("SSH_CONFIG_PATH")
    tunnels_cfg = os.path.join(config_path, "tunnels.json")
    tunnel_mgr = TunnelManager(host_managers=host_map, config_path=tunnels_cfg)
    tunnel_mgr.load()
    tunnel_mgr.cleanup_orphans()
    tunnel_mgr.startup_ts = time.time()
    logger.info(f"TunnelManager loaded {len(tunnel_mgr.tunnels)} tunnels")

    app = Auto2FAApp(managers, tunnel_mgr)
    try:
        app.run()
    finally:
        for mgr in managers:
            mgr.running = False
            mgr.active = False
        try:
            tunnel_mgr.shutdown()
        except Exception as e:
            logger.error(f"tunnel_mgr.shutdown failed: {e}")
        time.sleep(0.3)


if __name__ == "__main__":
    main()
