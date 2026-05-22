#!/usr/bin/env python3
"""
Auto2FA Dashboard - Multi-Server SSH Manager
"""

import json
import pyotp
import urllib.parse
import os
import sys
from dotenv import load_dotenv

load_dotenv()
import pexpect
import time
import logging
import subprocess
import threading
import signal
import termios
import tty
from datetime import datetime
from rich.live import Live
from rich.table import Table
from rich.console import Console
from rich import box
from rich.layout import Layout
from rich.panel import Panel

# Configure logging to file only, as stdout is used for TUI
logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(threadName)s - %(levelname)s - %(message)s',
    handlers=[
        logging.FileHandler('/tmp/auto2fa_dashboard.log'),
    ]
)
logger = logging.getLogger(__name__)


from .backend import SSHHostManager, extract_secret_from_url
from .tunnels import TunnelManager

# --- Main Dashboard ---

def load_hosts():
    try:
        config_path = os.environ.get("SSH_CONFIG_PATH")
        assert config_path, "SSH_CONFIG_PATH environment variable is not set"
        
        with open(f"{config_path}/passwords.json", 'r') as f:
            data = json.load(f)
        return data
    except Exception as e:
        print(f"Failed to load config: {e}")
        sys.exit(1)


connection_lock = threading.Lock()

class RawMode:
    """Context manager for raw terminal mode"""
    def __init__(self):
        self.fd = sys.stdin.fileno()
        self.old_settings = None

    def __enter__(self):
        try:
            self.old_settings = termios.tcgetattr(self.fd)
            tty.setcbreak(self.fd)
        except Exception:
            pass # Maybe not a TTY
        return self

    def __exit__(self, exc_type, exc_val, exc_tb):
        if self.old_settings:
            termios.tcsetattr(self.fd, termios.TCSADRAIN, self.old_settings)


def _modal_input(live, console, prompt_lines, fields, terminal_raw_mode):
    """Render a modal and collect text input for each field.

    fields: list of (label, default_value) tuples.
    Returns: dict {label: value} or None if user pressed Esc.
    """
    from rich.text import Text
    values = {}
    for label, default in fields:
        # Re-render modal with current values + current prompt
        body_lines = list(prompt_lines)
        for l, v in values.items():
            body_lines.append(f"  {l}: {v}")
        body_lines.append(f"  {label}: _")
        body_lines.append("")
        body_lines.append("[dim][Enter] Submit  [Esc] Cancel[/dim]")
        panel = Panel("\n".join(body_lines), title="[bold blue]Auto2FA[/bold blue]",
                      border_style="cyan", padding=(1, 4))
        live.update(panel)

        # Temporarily exit raw mode so input() works
        terminal_raw_mode.__exit__(None, None, None)
        try:
            sys.stdout.write("\033[?25h")  # show cursor
            sys.stdout.flush()
            try:
                val = input(f"{label} [{default}]: ").strip()
            except (KeyboardInterrupt, EOFError):
                return None
            if val == "":
                val = default
            values[label] = val
        finally:
            sys.stdout.write("\033[?25l")  # hide cursor
            sys.stdout.flush()
            terminal_raw_mode.__enter__()
    return values


def _node_picker(live, console, tunnel_mgr, name, terminal_raw_mode):
    """Show running jobs from squeue and let the user pick a node.

    Returns (node, user) or None if cancelled.
    """
    from .tunnels import NodeDiscovery, DiscoveryError
    ts = tunnel_mgr.tunnels[name]
    jump_name = tunnel_mgr.pick_active_jump(ts)
    if jump_name is None:
        live.update(Panel("[red]No connected jump host available.[/red]",
                          title="[bold blue]Pick node[/bold blue]"))
        time.sleep(1.5)
        return None
    mgr = tunnel_mgr.host_managers[jump_name]

    def fetch():
        try:
            return NodeDiscovery.discover(mgr), None
        except DiscoveryError as e:
            return [], str(e)

    jobs, err = fetch()
    sel = 0

    while True:
        body = Table(box=box.SIMPLE, expand=True,
                     title=f"Pick node for '{name}' via [bold]{jump_name}[/bold]",
                     title_justify="left")
        body.add_column("#", width=3, justify="right")
        body.add_column("JobID", width=10)
        body.add_column("Partition", width=10)
        body.add_column("Name", width=12)
        body.add_column("Time", width=14)
        body.add_column("Node", ratio=1)
        if err:
            body.add_row("", "[red]squeue failed[/red]", err[:40], "", "", "")
        elif not jobs:
            body.add_row("", "[dim]No running jobs.[/dim]", "", "", "", "")
        else:
            for i, j in enumerate(jobs):
                cursor = "▶" if i == sel else " "
                style = "bold white" if i == sel else "white"
                body.add_row(f"{cursor}{i+1}", j.jobid, j.partition, j.name, j.time, j.node, style=style)

        footer = "[dim][↑↓] Move  [Enter] Use  [R] Refresh  [C] Custom  [Esc] Cancel[/dim]"
        live.update(Panel.fit(Layout(body, name="body"), title="[bold blue]Pick node[/bold blue]",
                              border_style="cyan", subtitle=footer))

        # Read one key (still in raw mode)
        k = sys.stdin.read(1)
        if k == '\x1b':
            seq = ""
            import fcntl
            fl = fcntl.fcntl(sys.stdin, fcntl.F_GETFL)
            fcntl.fcntl(sys.stdin, fcntl.F_SETFL, fl | os.O_NONBLOCK)
            try:
                seq = sys.stdin.read(2)
            except Exception:
                pass
            finally:
                fcntl.fcntl(sys.stdin, fcntl.F_SETFL, fl)
            if seq == '':
                return None  # bare Esc
            elif seq == '[A':
                sel = max(0, sel - 1)
            elif seq == '[B':
                sel = min(max(0, len(jobs) - 1), sel + 1)
        elif k == '\r' or k == '\n':
            if jobs:
                user = _ssh_config_user(jump_name) or ts.last_user or os.environ.get("USER", "")
                return jobs[sel].node, user
        elif k == 'r' or k == 'R':
            jobs, err = fetch()
            sel = 0
        elif k == 'c' or k == 'C':
            vals = _modal_input(
                live, console,
                prompt_lines=[f"[bold]Custom node for '{name}'[/bold]", ""],
                fields=[("Node", ""), ("User", os.environ.get("USER", ""))],
                terminal_raw_mode=terminal_raw_mode,
            )
            if vals:
                return vals["Node"], vals["User"]

def _ssh_config_user(host: str) -> str:
    """Return the User from `ssh -G <host>`, or '' if unknown."""
    try:
        res = subprocess.run(["ssh", "-G", host], capture_output=True, text=True, timeout=2)
        for line in res.stdout.splitlines():
            if line.lower().startswith("user "):
                return line.split(" ", 1)[1].strip()
    except Exception:
        pass
    return ""


def main():
    config = load_hosts()
    managers = []
    
    # Initialize Managers
    for host, creds in config.items():
        if "otpauthUrl" in creds:
            secret = extract_secret_from_url(creds["otpauthUrl"])
            mgr = SSHHostManager(host, creds["password"], secret)
            mgr.daemon = True
            # Auto-Connect Logic
            start_active = creds.get("autoConnect", creds.get("auto_connect", False))
            mgr.active = start_active 
            mgr.start()
            managers.append(mgr)

    if not managers:
        print("No hosts found in passwords.json")
        sys.exit(1)

    # Initialize TunnelManager
    host_map = {m.host: m for m in managers}
    config_path = os.environ.get("SSH_CONFIG_PATH")
    tunnels_cfg = os.path.join(config_path, "tunnels.json")
    tunnel_mgr = TunnelManager(host_managers=host_map, config_path=tunnels_cfg)
    tunnel_mgr.load()
    tunnel_mgr.cleanup_orphans()
    tunnel_mgr.startup_ts = time.time()
    logger.info(f"TunnelManager loaded {len(tunnel_mgr.tunnels)} tunnels")

    logger.info(f"Main Loop Starting. Managers: {len(managers)}")
    for i, mgr in enumerate(managers):
        logger.info(f"Manager {i}: {mgr.host}")

    console = Console()
    selected_host_idx = 0
    selected_tunnel_idx = 0
    focused_section = "hosts"   # "hosts" | "tunnels"

    # We need to clear screen first or rich might get confused with raw mode artifacts
    console.clear()

    raw = RawMode()
    with raw:
        with Live(console=console, refresh_per_second=10, screen=True, auto_refresh=True) as live:
            while True:
                # 0. Drive tunnel lifecycle
                try:
                    tunnel_mgr.tick()
                except Exception as e:
                    logger.error(f"tunnel_mgr.tick failed: {e}")

                # 1a. Hosts Table
                hosts_table = Table(box=box.ROUNDED, expand=True, title="HOSTS",
                                    title_justify="left", title_style="bold cyan")
                hosts_table.add_column("Select", width=3, justify="center")
                hosts_table.add_column("Host", ratio=1)
                hosts_table.add_column("Status", width=20)
                hosts_table.add_column("Pool", width=10, justify="center")
                hosts_table.add_column("FS", width=4, justify="center")
                hosts_table.add_column("Last Message", ratio=2)

                for idx, mgr in enumerate(managers):
                    cursor = ">" if (focused_section == "hosts" and idx == selected_host_idx) else " "
                    row_style = "bold white" if (focused_section == "hosts" and idx == selected_host_idx) else "white"
                    status_style = "dim"
                    if "Connected" in mgr.status:
                        status_style = "green"
                    elif "Connecting" in mgr.status:
                        status_style = "blue"
                    elif "Failed" in mgr.status or "Error" in mgr.status:
                        status_style = "red"
                    status_text = mgr.status
                    if "[" not in status_text:
                        status_text = f"[{status_style}]{status_text}[/{status_style}]"
                    fs_icon = "📂" if ("Mounted" in mgr.last_msg or "Mounting" in mgr.last_msg) else ""
                    pool_info = ""
                    try:
                        alive_count = sum(1 for c in mgr.pool.values() if c.isalive())
                        pool_info = f"{mgr.active_index}/{alive_count}"
                    except Exception:
                        pool_info = "?"
                    hosts_table.add_row(cursor, mgr.host, status_text, pool_info, fs_icon, mgr.last_msg,
                                        style=row_style)

                # 1b. Tunnels Table
                tunnels_table = Table(box=box.ROUNDED, expand=True, title="TUNNELS",
                                      title_justify="left", title_style="bold cyan")
                tunnels_table.add_column("Select", width=3, justify="center")
                tunnels_table.add_column("Name", ratio=1)
                tunnels_table.add_column("Local→Remote", width=18)
                tunnels_table.add_column("Node", ratio=2)
                tunnels_table.add_column("Via", width=8)
                tunnels_table.add_column("Status", width=22)

                tunnel_names = list(tunnel_mgr.tunnels.keys())
                if not tunnel_names:
                    tunnels_table.add_row("", "[dim]No tunnels.  Press T to create one.[/dim]", "", "", "", "")
                else:
                    for idx, name in enumerate(tunnel_names):
                        ts = tunnel_mgr.tunnels[name]
                        cursor = ">" if (focused_section == "tunnels" and idx == selected_tunnel_idx) else " "
                        row_style = "bold white" if (focused_section == "tunnels" and idx == selected_tunnel_idx) else "white"
                        ports = f":{ts.local_port}→:{ts.remote_port}"
                        node = ts.last_node or "[dim](no node yet)[/dim]"
                        via = ts.active_jump or "—"
                        glyph_color = {
                            "alive": ("●", "green"),
                            "starting": ("◐", "yellow"),
                            "stale": ("○", "red"),
                            "idle": ("○", "dim"),
                            "port_busy": ("●", "red"),
                            "failed": ("●", "red"),
                        }.get(ts.status, ("?", "white"))
                        glyph, color = glyph_color
                        status_cell = f"[{color}]{glyph} {ts.status}[/{color}]  [dim]{ts.last_msg}[/dim]"
                        tunnels_table.add_row(cursor, name, ports, node, via, status_cell, style=row_style)

                layout = Layout()
                layout.split_column(
                    Layout(hosts_table, name="hosts"),
                    Layout(tunnels_table, name="tunnels"),
                )
                panel = Panel(
                    layout,
                    title="[bold blue]Auto2FA Dashboard[/bold blue]",
                    subtitle="[Tab] Switch  [↑↓] Nav  [Space] Toggle  [T] New tunnel  [⏎] Pick node  [D] Delete  [R] Rotate  [Q] Quit"
                )
                live.update(panel)
                
                # 2. Handle Input
                # Since we are in persistent raw mode, we can read directly
                import select
                if select.select([sys.stdin], [], [], 0.05)[0]:
                    try:
                        key = sys.stdin.read(1)
                        if key == '\x1b':
                            # Read potential arrow keys
                            # Non-blocking read for remainder of sequence
                            import fcntl
                            fl = fcntl.fcntl(sys.stdin, fcntl.F_GETFL)
                            fcntl.fcntl(sys.stdin, fcntl.F_SETFL, fl | os.O_NONBLOCK)
                            try:
                                seq = sys.stdin.read(2)
                                key += seq
                            except Exception:
                                pass
                            finally:
                                fcntl.fcntl(sys.stdin, fcntl.F_SETFL, fl)
                        
                        if key == 'q' or key == 'Q' or key == '\x03':
                            break
                        elif key == '\x1b[A':  # Up
                            if focused_section == "hosts":
                                selected_host_idx = max(0, selected_host_idx - 1)
                            else:
                                selected_tunnel_idx = max(0, selected_tunnel_idx - 1)
                        elif key == '\x1b[B':  # Down
                            if focused_section == "hosts":
                                selected_host_idx = min(len(managers) - 1, selected_host_idx + 1)
                            else:
                                n = len(tunnel_mgr.tunnels)
                                if n > 0:
                                    selected_tunnel_idx = min(n - 1, selected_tunnel_idx + 1)
                        elif key == '\t':  # Tab
                            focused_section = "tunnels" if focused_section == "hosts" else "hosts"
                        elif key == ' ':  # Space
                            if focused_section == "hosts":
                                managers[selected_host_idx].toggle()
                            else:
                                names = list(tunnel_mgr.tunnels.keys())
                                if names:
                                    tunnel_mgr.toggle(names[selected_tunnel_idx])
                        elif key == 'm' or key == 'M':
                            if focused_section == "hosts":
                                threading.Thread(target=managers[selected_host_idx].mount_host, daemon=True).start()
                        elif key == 'r' or key == 'R':
                            if focused_section == "hosts":
                                mgr = managers[selected_host_idx]
                                if mgr.active:
                                    new_idx = (mgr.active_index + 1) % 2
                                    if hasattr(mgr, 'update_symlink'):
                                        mgr.update_symlink(new_idx)
                                        mgr.last_msg = f"Manual Rotate -> {new_idx}"

                        elif key == 't' or key == 'T':
                            vals = _modal_input(
                                live, console,
                                prompt_lines=["[bold]New Tunnel[/bold]", ""],
                                fields=[("Name", ""), ("Local port", "8888")],
                                terminal_raw_mode=raw,
                            )
                            if vals:
                                try:
                                    tunnel_mgr.add(
                                        name=vals["Name"],
                                        local_port=int(vals["Local port"]),
                                    )
                                    # Move focus to the new tunnel
                                    focused_section = "tunnels"
                                    selected_tunnel_idx = list(tunnel_mgr.tunnels.keys()).index(vals["Name"])
                                except (ValueError, KeyError) as e:
                                    logger.warning(f"add tunnel failed: {e}")
                                    error_msg = str(e)
                                    error_panel = Panel(f"[red]Could not create tunnel: {error_msg}[/red]",
                                                        title="[bold blue]Auto2FA[/bold blue]")
                                    live.update(error_panel)
                                    time.sleep(1.5)

                        elif key == '\r' or key == '\n':   # Enter
                            if focused_section == "tunnels":
                                names = list(tunnel_mgr.tunnels.keys())
                                if names:
                                    n = names[selected_tunnel_idx]
                                    picked = _node_picker(live, console, tunnel_mgr, n, raw)
                                    if picked:
                                        node, user = picked
                                        tunnel_mgr.set_node(n, node, user)
                                        tunnel_mgr.start(n)

                        elif key == 'd' or key == 'D':
                            if focused_section == "tunnels":
                                names = list(tunnel_mgr.tunnels.keys())
                                if names:
                                    n = names[selected_tunnel_idx]
                                    confirm = Panel(
                                        f"Delete tunnel [bold red]{n}[/bold red]? [Y]es / [N]o",
                                        title="[bold blue]Confirm[/bold blue]", border_style="red")
                                    live.update(confirm)
                                    while True:
                                        ck = sys.stdin.read(1)
                                        if ck in ('y', 'Y'):
                                            tunnel_mgr.stop(n)
                                            tunnel_mgr.remove(n)
                                            selected_tunnel_idx = max(0, selected_tunnel_idx - 1)
                                            break
                                        elif ck in ('n', 'N', '\x1b'):
                                            break

                    except Exception:
                        pass
                    
    # Cleanup
    # RawMode exit will restore terminal
    print("Stopping managers...")
    for mgr in managers:
        mgr.running = False
        mgr.active = False

    try:
        tunnel_mgr.shutdown()
    except Exception as e:
        logger.error(f"tunnel_mgr.shutdown failed: {e}")

    time.sleep(0.5)
    print("Clean exit.")

if __name__ == "__main__":
    main()