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

def main():
    config = load_hosts()
    managers = []
    
    # Initialize Managers
    for host, creds in config.items():
        if "otpauthUrl" in creds:
            secret = extract_secret_from_url(creds["otpauthUrl"])
            mgr = SSHHostManager(host, creds["password"], secret)
            mgr.daemon = True
            mgr.active = False 
            mgr.start()
            managers.append(mgr)

    if not managers:
        print("No hosts found in passwords.json")
        sys.exit(1)

    console = Console()
    selected_idx = 0
    
    # We need to clear screen first or rich might get confused with raw mode artifacts
    console.clear()

    with RawMode():
        with Live(console=console, refresh_per_second=10, screen=True, auto_refresh=True) as live:
            while True:
                # 1. Render Table
                table = Table(box=box.ROUNDED, expand=True)
                table.add_column("Select", width=3, justify="center")
                table.add_column("Host", ratio=1)
                table.add_column("Status", width=12)
                table.add_column("FS", width=4, justify="center")
                table.add_column("Last Message", ratio=2)
                
                for idx, mgr in enumerate(managers):
                    cursor = ">" if idx == selected_idx else " "
                    row_style = "bold white" if idx == selected_idx else "white"
                    status_style = "dim"
                    
                    if "Connected" in mgr.status:
                        status_style = "green"
                    elif "Connecting" in mgr.status:
                        status_style = "blue"
                    elif "Failed" in mgr.status or "Error" in mgr.status:
                        status_style = "red"
                    
                    # Apply style to status text if it doesn't already have markup
                    status_text = mgr.status
                    if "[" not in status_text:
                        status_text = f"[{status_style}]{status_text}[/{status_style}]"

                    # FS Icon
                    fs_icon = ""
                    if "Mounted" in mgr.last_msg or "Mounting" in mgr.last_msg:
                        fs_icon = "📂" 

                    table.add_row(
                        cursor,
                        mgr.host,
                        status_text,
                        fs_icon,
                        mgr.last_msg,
                        style=row_style
                    )
                
                panel = Panel(
                    table,
                    title="[bold blue]Auto2FA Dashboard[/bold blue]",
                    subtitle="[Up/Down] Navigate  [Space] Toggle  [Q] Quit  |  Mounts: ~/Mounts/<host>"
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
                        
                        if key == 'q' or key == 'Q' or key == '\x03': # q or Ctrl+C
                            break
                        elif key == '\x1b[A': # Up
                            selected_idx = max(0, selected_idx - 1)
                        elif key == '\x1b[B': # Down
                            selected_idx = min(len(managers) - 1, selected_idx + 1)
                        elif key == ' ': # Space
                            managers[selected_idx].toggle()
                            
                    except Exception:
                        pass
                    
    # Cleanup
    # RawMode exit will restore terminal
    print("Stopping managers...")
    for mgr in managers:
        mgr.running = False
        mgr.active = False
    
    time.sleep(0.5)
    print("Clean exit.")

if __name__ == "__main__":
    main()