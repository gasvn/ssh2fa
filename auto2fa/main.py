#!/usr/bin/env python3
"""Auto2FA Dashboard — Textual TUI."""
from __future__ import annotations

import json
import logging
import os
import re
import subprocess
import sys
import threading
import time

from dotenv import load_dotenv

load_dotenv()

from rich.text import Text
from textual.app import App, ComposeResult
from textual.binding import Binding
from textual.containers import Horizontal, Vertical
from textual.screen import ModalScreen
from textual.widgets import Button, DataTable, Footer, Header, Input, Label

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


def _system_notify(title: str, message: str) -> None:
    """Best-effort native desktop notification (macOS via osascript).

    Runs on a daemon thread — osascript can take up to its timeout window
    and we must never block the Textual event loop.
    """
    def _run():
        try:
            safe_title = title.replace('"', '\\"')
            safe_msg = message.replace('"', '\\"')
            subprocess.run(
                ["osascript", "-e",
                 f'display notification "{safe_msg}" with title "{safe_title}"'],
                check=False, timeout=2,
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            )
        except Exception:
            pass
    threading.Thread(target=_run, daemon=True).start()


_SSH_USER_CACHE: dict[str, str] = {}


def ssh_config_user(host: str) -> str:
    """Resolve the User from `ssh -G <host>`. Cached so subprocess only runs once per host."""
    if host in _SSH_USER_CACHE:
        return _SSH_USER_CACHE[host]
    user = ""
    try:
        res = subprocess.run(["ssh", "-G", host], capture_output=True, text=True, timeout=2)
        for line in res.stdout.splitlines():
            if line.lower().startswith("user "):
                user = line.split(" ", 1)[1].strip()
                break
    except Exception:
        pass
    _SSH_USER_CACHE[host] = user
    return user


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
        # Catch q so it doesn't bubble to App.action_quit and kill the dashboard
        Binding("q", "cancel", show=False),
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
        Binding("q", "cancel", show=False),
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
            yield Label("Tab/Enter: next field   Ctrl+S: submit   Esc: cancel",
                        classes="hint")
            yield Label("(Paste with ⌘V / Ctrl+Shift+V works in both fields)",
                        classes="hint")

    def on_mount(self) -> None:
        self.query_one("#node", Input).focus()

    def action_cancel(self) -> None:
        self.dismiss(None)

    def action_submit(self) -> None:
        self._submit()

    def on_input_submitted(self, event: Input.Submitted) -> None:
        # Don't auto-submit on Enter — pasted hostnames often contain a
        # trailing newline which would trigger Input.Submitted mid-paste.
        # Instead move focus forward: node → user → click Submit (or Ctrl+S).
        if event.input.id == "node":
            self.query_one("#user", Input).focus()
        else:
            # On the user field, Enter does submit (rarely contains newlines)
            self._submit()

    def on_button_pressed(self, event: Button.Pressed) -> None:
        if event.button.id == "submit_btn":
            self._submit()
        elif event.button.id == "cancel_btn":
            self.dismiss(None)

    def _submit(self) -> None:
        # Strip leading/trailing whitespace AND any embedded newlines that may
        # have arrived via paste — defensive: hostnames never contain those.
        node = self.query_one("#node", Input).value.strip().replace("\n", "").replace("\r", "")
        user = (
            self.query_one("#user", Input).value.strip().replace("\n", "").replace("\r", "")
            or os.environ.get("USER", "")
        )
        if node:
            self.dismiss((node, user))


class NodePickerScreen(ModalScreen[tuple[str, str, bool] | None]):
    """Pick a compute node from `squeue`. Returns (node, user, is_range) or None."""

    DEFAULT_CSS = """
    NodePickerScreen { align: center middle; }
    NodePickerScreen > Vertical {
        width: 100; height: auto; max-height: 90%; padding: 1 2;
        border: thick $accent; background: $panel;
    }
    NodePickerScreen Label.title { content-align: center middle; padding-bottom: 1; }
    NodePickerScreen Label.hint  { color: $text-muted; padding-top: 1; content-align: center middle; }
    NodePickerScreen Label.err   { color: $error; padding-top: 1; content-align: center middle; }
    NodePickerScreen DataTable   { height: 1fr; }
    """

    BINDINGS = [
        Binding("escape", "cancel", "Cancel"),
        Binding("q", "cancel", show=False),  # Don't let q quit the app from here
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
        # Pre-warm the ssh_config_user cache on a background thread so the
        # first Enter to pick a node doesn't pay the 2s subprocess cost.
        jump = self.jump_name
        threading.Thread(
            target=lambda: ssh_config_user(jump), daemon=True
        ).start()
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
                err_msg = None
            except DiscoveryError as e:
                jobs, err_msg = [], str(e)
            except Exception as e:
                jobs, err_msg = [], f"unexpected: {e}"
            try:
                self.app.call_from_thread(self._on_jobs_loaded, jobs, err_msg)
            except RuntimeError:
                pass  # app shut down before worker finished

        threading.Thread(target=worker, daemon=True).start()

    def _on_jobs_loaded(self, jobs, error_msg) -> None:
        # Screen may have been dismissed while squeue was running in the
        # background. Discard the result silently.
        if not self.is_mounted:
            return
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

        # Smart default: if the previously-used node is still in the list,
        # pre-select it (saves the user from re-finding it).
        last_node = self.tunnel_mgr.tunnels[self.tunnel_name].last_node
        preselect = -1
        for i, j in enumerate(jobs):
            marker = ""
            if j.node == last_node:
                marker = "  ← previous"
                preselect = i
            table.add_row(j.jobid, j.partition, j.name, j.time,
                          Text(j.node) + Text(marker, style="yellow"))
        if preselect >= 0:
            err.update(Text("Previously used node is highlighted. Enter to use it again, or pick another.",
                            style="grey62"))
            table.move_cursor(row=preselect)
        else:
            err.update("")
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
        row = event.cursor_row
        if not (0 <= row < len(self.jobs)):
            return
        job = self.jobs[row]
        node, is_range = expand_first_node(job.node)
        user = (
            ssh_config_user(self.jump_name)
            or self.tunnel_mgr.tunnels[self.tunnel_name].last_user
            or os.environ.get("USER", "")
        )
        event.stop()
        self.dismiss((node, user, is_range))


class HelpScreen(ModalScreen[None]):
    """Help overlay listing all keybindings and workflow."""

    DEFAULT_CSS = """
    HelpScreen { align: center middle; }
    HelpScreen > Vertical {
        width: 78; height: auto; max-height: 90%;
        padding: 1 2;
        border: thick $accent; background: $panel;
    }
    HelpScreen Label.title { content-align: center middle; padding-bottom: 1; }
    HelpScreen Label.section { color: $accent; padding-top: 1; }
    HelpScreen Label.row { padding-left: 2; }
    HelpScreen Label.hint { color: $text-muted; content-align: center middle; padding-top: 1; }
    """

    BINDINGS = [Binding("escape,q,?", "dismiss_help", "Close")]

    def compose(self) -> ComposeResult:
        with Vertical():
            yield Label("[b]Auto2FA — Keyboard Reference[/b]", classes="title")

            yield Label("[b]Navigation[/b]", classes="section")
            yield Label("  Tab            Switch between HOSTS and TUNNELS", classes="row")
            yield Label("  ↑ / ↓          Move cursor within current section", classes="row")
            yield Label("  Mouse click    Focus a row directly", classes="row")

            yield Label("[b]Hosts[/b]", classes="section")
            yield Label("  Space          Start / stop the selected host", classes="row")
            yield Label("  M              Mount remote filesystem (sshfs)", classes="row")
            yield Label("  R              Rotate connection pool", classes="row")

            yield Label("[b]Tunnels[/b]", classes="section")
            yield Label("  T              New tunnel (works from any section)", classes="row")
            yield Label("  Enter          Pick a compute node for selected tunnel", classes="row")
            yield Label("  Space          Start / stop the selected tunnel", classes="row")
            yield Label("  Y              Copy 'localhost:<port>' to clipboard", classes="row")
            yield Label("  D              Delete the selected tunnel", classes="row")

            yield Label("[b]Global[/b]", classes="section")
            yield Label("  ?              Show this help", classes="row")
            yield Label("  Q              Quit", classes="row")

            yield Label("[b]Workflow[/b]", classes="section")
            yield Label("  1. Wait for a host to show ● Connected (green)", classes="row")
            yield Label("  2. Press T → enter name + port → Submit", classes="row")
            yield Label("  3. Cursor on the new tunnel → Enter → pick a job", classes="row")
            yield Label("  4. Tunnel shows ● alive — use localhost:<port>", classes="row")

            yield Label("Esc or Q to close", classes="hint")

    def action_dismiss_help(self) -> None:
        self.dismiss(None)


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
        Binding("n,escape,q", "confirm(False)", "No"),
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
        Binding("y", "tunnel_yank", "Copy URL"),
    ]

    def action_tunnel_toggle(self) -> None:
        self.app.action_toggle_tunnel()

    def action_tunnel_new(self) -> None:
        self.app.action_new_tunnel()

    def action_tunnel_delete(self) -> None:
        self.app.action_delete_tunnel()

    def action_tunnel_yank(self) -> None:
        self.app.action_yank_url()


# ---------- Main app ----------

class Auto2FAApp(App):
    CSS = """
    Screen { layout: vertical; }
    /* HOSTS gets only the space its rows need (+ header + a little padding);
       TUNNELS gets all remaining vertical space. Most users have 2-3 hosts
       and many tunnels, so this is the right balance. */
    #hosts_table { height: auto; max-height: 40%; }
    #tunnels_table { height: 1fr; }
    .section-title {
        background: $accent 40%;
        color: $text;
        padding: 0 1;
        text-style: bold;
    }
    .section-title.dim { background: $surface; color: $text-muted; }
    DataTable > .datatable--cursor { background: $accent 30%; }
    """

    # Only truly global bindings. Per-table keys live on HostTable / TunnelTable
    # so they don't interfere with Input widgets in modals.
    # T is also bound here so it works from the HOSTS table too (Input widgets
    # in modals still consume 't' first, so typing 't' in a name field never
    # triggers this).
    BINDINGS = [
        Binding("q", "quit", "Quit"),
        Binding("t", "new_tunnel", "New tunnel"),
        Binding("ctrl+n", "new_tunnel", show=False),  # alt shortcut
        Binding("question_mark", "help", "Help"),
        Binding("?", "help", show=False),
    ]

    def __init__(self, managers, tunnel_mgr):
        super().__init__()
        self.managers = managers
        self.tunnel_mgr = tunnel_mgr
        self._tunnel_names: list[str] = []
        self._tick_stop = threading.Event()
        self._tick_thread: threading.Thread | None = None
        # Cache last-rendered "fingerprint" to skip redundant table rebuilds.
        self._last_host_fp: tuple = ()
        self._last_tunnel_fp: tuple = ()
        # Per-tunnel lock to debounce rapid Space presses; while a toggle is
        # in flight for a tunnel, additional presses are ignored.
        self._toggle_in_flight: set[str] = set()
        self._toggle_in_flight_lock = threading.Lock()
        # Optimistic-status overlay: shows pending action immediately, cleared
        # once the real status changes. Maps tunnel name → display text.
        self._pending_status: dict[str, str] = {}
        # Track last-seen status per tunnel to notify on transitions like
        # alive → failed / stale (so the user knows when things break).
        self._last_seen_status: dict[str, str] = {}

    def compose(self) -> ComposeResult:
        yield Header(show_clock=True)
        yield Label("HOSTS", classes="section-title", id="hosts_title")
        yield HostTable(id="hosts_table", cursor_type="row")
        yield Label("TUNNELS  ·  Tab to switch  ·  T to create  ·  Enter to pick node",
                    classes="section-title", id="tunnels_title")
        yield TunnelTable(id="tunnels_table", cursor_type="row")
        yield Footer()

    def on_mount(self) -> None:
        self.title = "Auto2FA"
        self.sub_title = "Press ? for help"
        hosts = self.query_one("#hosts_table", HostTable)
        hosts.add_columns("Host", "Status", "Pool", "FS", "Last Message")
        tunnels = self.query_one("#tunnels_table", TunnelTable)
        tunnels.add_columns("Name", "Local→Remote", "Node", "Via", "Status")
        hosts.focus()
        self._update_section_focus_styles()
        self._refresh_tables()
        # Single combined timer to minimise event-loop wake-ups. Each tick:
        #   - refresh tables if state changed (fingerprint comparison)
        #   - refresh summary if numbers changed
        # All work is skipped when a modal is on top so input stays smooth.
        self.set_interval(1.0, self._tick_ui)
        # tick() can block (start() probes for up to 10s) — run it on its own thread
        self._tick_thread = threading.Thread(target=self._tick_loop, daemon=True)
        self._tick_thread.start()

    def on_descendant_focus(self, event) -> None:
        """Update section title styling whenever focus changes."""
        self._update_section_focus_styles()

    def _update_section_focus_styles(self) -> None:
        try:
            hosts_title = self.query_one("#hosts_title", Label)
            tunnels_title = self.query_one("#tunnels_title", Label)
        except Exception:
            return
        focused = self.focused
        focused_id = getattr(focused, "id", None) if focused is not None else None
        if focused_id == "hosts_table":
            hosts_title.remove_class("dim")
            tunnels_title.add_class("dim")
        elif focused_id == "tunnels_table":
            tunnels_title.remove_class("dim")
            hosts_title.add_class("dim")

    _last_summary: str = ""

    def _refresh_summary(self) -> None:
        connected = sum(1 for m in self.managers if m.is_master_ready())
        active_tunnels = sum(1 for t in self.tunnel_mgr.tunnels.values() if t.status == "alive")
        n_tunnels = len(self.tunnel_mgr.tunnels)
        summary = (
            f"hosts {connected}/{len(self.managers)} connected   "
            f"·   tunnels {active_tunnels}/{n_tunnels} alive   "
            f"·   ? for help"
        )
        # Only write if changed — avoids needless header re-renders
        if summary != self._last_summary:
            self._last_summary = summary
            self.sub_title = summary

    def action_help(self) -> None:
        # Don't stack help on top of other modals
        if len(self.screen_stack) > 1:
            return
        self.push_screen(HelpScreen())

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

    def _tick_ui(self) -> None:
        """Single combined UI tick. Hard-skips ALL work when a modal is open
        so input fields in CustomNodeScreen / NewTunnelScreen stay snappy."""
        if len(self.screen_stack) > 1:
            return
        self._refresh_tables()
        self._refresh_summary()

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
        for mgr, (_, _, pool, fs, last_msg) in zip(self.managers, rows):
            status_text = self._render_host_status(mgr)
            table.add_row(mgr.host, status_text, pool, fs, last_msg)
        if 0 <= prev_row < len(self.managers):
            table.move_cursor(row=prev_row)

    @staticmethod
    def _render_host_status(mgr) -> Text:
        """Build a colored Text cell for the Status column with a leading glyph."""
        raw = mgr.status
        # Strip any pre-existing rich markup so we can apply our own.
        plain = re.sub(r"\[/?[^\]]+\]", "", raw).strip() or ("Active" if mgr.active else "Stopped")
        lc = plain.lower()
        if ("connected" in lc or "active" in lc) and "init" not in lc and "fail" not in lc:
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
        # Snapshot via items() so a concurrent delete from a worker thread
        # can't make the names/values fall out of sync. Iterating items()
        # of a dict snapshot is safe even if the original dict mutates.
        names: list[str] = []
        rows = []
        for name, ts in list(self.tunnel_mgr.tunnels.items()):
            names.append(name)
            pending = self._pending_status.get(name)
            rows.append((
                name, ts.local_port, ts.remote_port,
                ts.last_node, ts.active_jump, ts.status, ts.last_msg, pending,
            ))

        # Detect transitions and notify the user about meaningful state changes.
        self._notify_status_transitions(rows)

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
                Text("✨  No tunnels yet. Press T (or Ctrl+N) to create one.",
                     style="bold yellow"),
                "", "", "", "",
            )
            return
        for name, row in zip(names, rows):
            _, lp, rp, last_node, active_jump, status, last_msg, pending = row
            ports = f":{lp}→:{rp}"
            node = last_node or Text("(no node yet)", style="grey50")
            via = active_jump or "—"

            status_cell = self._render_tunnel_status(status, pending, last_msg, active_jump, lp)
            table.add_row(name, ports, node, via, status_cell)
        if 0 <= prev_row < len(names):
            table.move_cursor(row=prev_row)

    def _notify_status_transitions(self, rows) -> None:
        """Compare the new rows to the last seen status and toast on transitions
        that the user cares about — e.g. alive → failed / stale / port_busy."""
        present_names = set()
        for row in rows:
            name = row[0]
            status = row[5]
            last_msg = row[6]
            present_names.add(name)
            prev = self._last_seen_status.get(name)
            self._last_seen_status[name] = status
            if prev is None or prev == status:
                continue
            # Bad transitions: tell the user loudly (in-app toast + macOS notification).
            if prev == "alive" and status == "stale":
                msg = f"{name}: compute node ended — press Enter to repick"
                self.notify("⚠  " + msg, severity="warning", timeout=10)
                _system_notify("Auto2FA: tunnel stale", msg)
            elif prev == "alive" and status == "failed":
                msg = f"{name} disconnected: {last_msg}"
                self.notify("✕  " + msg, severity="error", timeout=10)
                _system_notify("Auto2FA: tunnel failed", msg)
            elif prev == "alive" and status == "idle":
                msg = f"{name}: connection dropped — waiting to reconnect"
                self.notify("⚠  " + msg, severity="warning", timeout=8)
                _system_notify("Auto2FA: tunnel idle", msg)
            elif prev == "alive" and status == "starting":
                # ssh -N child died and tick() is respawning. Toast quietly.
                self.notify(f"↻  {name}: reconnecting…", severity="warning", timeout=4)
            # Good transitions
            elif prev in ("starting", "idle", "stale", "failed", "port_busy") \
                 and status == "alive":
                self.notify(f"✓  {name} connected", timeout=3)
        # Drop entries for tunnels that were removed
        for name in list(self._last_seen_status.keys()):
            if name not in present_names:
                self._last_seen_status.pop(name, None)

    def _render_tunnel_status(self, status: str, pending: str | None,
                              last_msg: str, jump: str | None,
                              local_port: int) -> Text:
        """Render the Status column cell with friendly human-readable text."""
        cell = Text()
        if pending:
            cell.append(f"◐ {pending}", style="bold yellow")
            return cell

        if status == "alive":
            cell.append("● ", style="green")
            cell.append("Connected", style="bold green")
            if jump:
                cell.append(f"  via {jump}", style="grey62")
            cell.append(f"   → localhost:{local_port}", style="grey50")
            return cell

        if status == "starting":
            cell.append("◐ ", style="yellow")
            cell.append("Connecting…", style="bold yellow")
            if jump:
                cell.append(f"  via {jump}", style="grey62")
            return cell

        if status == "stale":
            cell.append("○ ", style="red")
            cell.append("Job ended", style="bold red")
            cell.append("   Press Enter to repick", style="grey62")
            return cell

        if status == "idle":
            if last_msg and "waiting for jump" in last_msg.lower():
                cell.append("○ ", style="yellow")
                cell.append("Waiting for jump host", style="bold yellow")
                return cell
            if last_msg and "no node" in last_msg.lower():
                cell.append("○ ", style="grey50")
                cell.append("No node selected", style="bold")
                cell.append("   Press Enter to pick", style="grey62")
                return cell
            cell.append("○ ", style="grey50")
            cell.append("Stopped", style="bold")
            cell.append("   Press Space to start", style="grey62")
            return cell

        if status == "port_busy":
            cell.append("● ", style="red")
            cell.append(f"Port {local_port} in use", style="bold red")
            cell.append("   Free it or change the port", style="grey62")
            return cell

        if status == "failed":
            cell.append("● ", style="red")
            cell.append("Failed", style="bold red")
            if last_msg:
                cell.append(f"   {last_msg}", style="grey62")
            cell.append("   Press Space to retry", style="grey50")
            return cell

        # Fallback
        cell.append(f"? {status}", style="white")
        if last_msg:
            cell.append(f"   {last_msg}", style="grey50")
        return cell

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
        if not name:
            return
        # Debounce: if a toggle worker is already in flight for this tunnel,
        # ignore additional presses so rapid Space mashing doesn't queue
        # contradictory start/stop operations on the lock.
        with self._toggle_in_flight_lock:
            if name in self._toggle_in_flight:
                return
            self._toggle_in_flight.add(name)

        # Optimistic UI: show pending action immediately, so the user gets
        # instant feedback while the (possibly 10s) start probe runs.
        ts = self.tunnel_mgr.tunnels.get(name)
        if ts is not None:
            if ts.status in ("alive", "starting"):
                self._pending_status[name] = "stopping…"
            else:
                self._pending_status[name] = "starting…"
            self._refresh_tunnels()

        # toggle() may call start() which blocks for up to 10s on the port
        # probe. Run off the UI thread so the dashboard stays responsive.
        def _do_toggle():
            try:
                self.tunnel_mgr.toggle(name)
            except Exception as e:
                logger.error(f"toggle({name}) failed: {e}")
            finally:
                with self._toggle_in_flight_lock:
                    self._toggle_in_flight.discard(name)
                self._pending_status.pop(name, None)
            try:
                self.call_from_thread(self._refresh_tunnels)
            except RuntimeError:
                pass  # app shut down
        threading.Thread(target=_do_toggle, daemon=True).start()

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
            except (ValueError, KeyError, OSError) as e:
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
            self.notify(f"Connecting '{name}' to {node}…", timeout=3)
            # set_node may call start() which probes a port for up to 10s.
            # Run it off the UI thread so we don't freeze.
            def do_set():
                try:
                    self.tunnel_mgr.set_node(name, node, user)
                    if is_range and name in self.tunnel_mgr.tunnels:
                        self.tunnel_mgr.tunnels[name].last_msg += " (picked first of range)"
                except Exception as e:
                    logger.error(f"set_node({name}) failed: {e}")
                try:
                    self.call_from_thread(self._refresh_tunnels)
                except RuntimeError:
                    pass  # app shut down
            threading.Thread(target=do_set, daemon=True).start()

        self.push_screen(NodePickerScreen(self.tunnel_mgr, name), on_picked)

    def action_yank_url(self) -> None:
        """Copy localhost:<port> for the selected tunnel to the clipboard."""
        name = self._selected_tunnel_name()
        if not name:
            return
        ts = self.tunnel_mgr.tunnels.get(name)
        if ts is None:
            return
        url = f"localhost:{ts.local_port}"
        copied = False
        try:
            self.copy_to_clipboard(url)
            copied = True
        except Exception:
            pass
        if copied:
            self.notify(f"Copied  {url}  to clipboard", timeout=3)
        else:
            # Fallback (notifies the user itself based on result)
            self._fallback_clipboard(url)

    def _fallback_clipboard(self, text: str) -> None:
        """Best-effort system clipboard fallback via pbcopy / xclip / wl-copy.

        Runs on a worker thread because pbcopy/xclip can briefly hang on
        some systems; we never want to freeze the UI for a copy operation.
        """
        def _run():
            for cmd in (["pbcopy"], ["xclip", "-selection", "clipboard"], ["wl-copy"]):
                try:
                    p = subprocess.Popen(cmd, stdin=subprocess.PIPE)
                    try:
                        p.communicate(text.encode(), timeout=2)
                    except subprocess.TimeoutExpired:
                        p.kill()
                        p.communicate()
                        continue
                    if p.returncode == 0:
                        try:
                            self.call_from_thread(
                                lambda: self.notify(f"Copied  {text}  to clipboard", timeout=3)
                            )
                        except RuntimeError:
                            pass
                        return
                except FileNotFoundError:
                    continue
                except Exception:
                    continue
            try:
                self.call_from_thread(
                    lambda: self.notify(
                        f"Couldn't access clipboard. URL: {text}",
                        severity="warning", timeout=8,
                    )
                )
            except RuntimeError:
                pass
        threading.Thread(target=_run, daemon=True).start()

    def action_delete_tunnel(self) -> None:
        name = self._selected_tunnel_name()
        if not name:
            return

        def on_confirm(yes: bool) -> None:
            if not yes:
                return
            # stop() acquires the lifecycle lock which can be held by a
            # 10s start probe — must run off the UI thread.
            def _do_delete():
                try:
                    self.tunnel_mgr.stop(name)
                    self.tunnel_mgr.remove(name)
                except Exception as e:
                    logger.error(f"delete({name}) failed: {e}")
                try:
                    self.call_from_thread(self._refresh_tunnels)
                    self.call_from_thread(
                        lambda: self.notify(f"Deleted tunnel '{name}'", timeout=3)
                    )
                except RuntimeError:
                    pass  # app shut down
            threading.Thread(target=_do_delete, daemon=True).start()

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
