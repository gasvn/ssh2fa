#!/usr/bin/env python3
"""Auto2FA Rich Monitor

Displays a live-updating dashboard in the terminal showing the status of
Auto2FA connections, configuration files, and logs.
"""

from __future__ import annotations

import hashlib
import json
import subprocess
import time
from collections import deque
from datetime import datetime
from pathlib import Path
from typing import Deque, Iterable, Tuple

from rich import box
from rich.console import Console
from rich.layout import Layout
from rich.live import Live
from rich.panel import Panel
from rich.table import Table
from rich.text import Text

SSH_CONFIG_PATH = Path.home() / ".ssh" / "config"
PASSWORDS_PATH = Path.home() / ".ssh" / "passwords.json"
CONTROL_DIR = Path.home() / ".ssh" / "auto2fa_control"
LOG_PATH = Path("/tmp/auto2fa.log")
DEFAULT_REFRESH_SECONDS = 2.0
DEFAULT_LOG_LINES = 12


def build_control_socket_path(host: str) -> Path:
    """Return the deterministic control socket path for the given host."""
    CONTROL_DIR.mkdir(parents=True, exist_ok=True)
    safe_host = "".join(c if c.isalnum() or c in ("-", "_", ".") else "_" for c in host)
    host_hash = hashlib.sha1(host.encode("utf-8")).hexdigest()[:10]
    return CONTROL_DIR / f"{safe_host}_{host_hash}.sock"


def format_bool(ok: bool) -> str:
    return "[bold green]✅ 正常[/]" if ok else "[bold red]❌ 异常[/]"


def read_tail(path: Path, num_lines: int) -> Iterable[str]:
    if not path.exists():
        return []
    try:
        with path.open("r", encoding="utf-8", errors="ignore") as fh:
            deque_lines: Deque[str] = deque(maxlen=num_lines)
            for line in fh:
                deque_lines.append(line.rstrip())
        return list(deque_lines)
    except Exception as exc:  # pragma: no cover - defensive guard
        return [f"无法读取日志: {exc}"]


def format_timestamp(ts: float | None) -> str:
    if ts is None:
        return "未知"
    return datetime.fromtimestamp(ts).strftime("%Y-%m-%d %H:%M:%S")


def check_control_master(host: str, socket_path: Path) -> Tuple[bool, str]:
    if not socket_path.exists():
        return False, "控制 socket 不存在"

    try:
        result = subprocess.run(
            [
                "ssh",
                "-S",
                str(socket_path),
                "-O",
                "check",
                host,
            ],
            capture_output=True,
            text=True,
            timeout=5,
        )
    except FileNotFoundError:
        return False, "系统未找到 ssh 命令"
    except subprocess.TimeoutExpired:
        return False, "ssh -O check 超时"
    except Exception as exc:  # pragma: no cover - defensive guard
        return False, f"检查失败: {exc}"

    if result.returncode == 0:
        detail = result.stdout.strip() or "控制主连接已就绪"
        return True, detail

    detail = result.stderr.strip() or result.stdout.strip() or f"退出码 {result.returncode}"
    return False, detail


def check_ssh_config(host: str) -> Tuple[bool, str]:
    if not SSH_CONFIG_PATH.exists():
        return False, "配置文件不存在"

    try:
        with SSH_CONFIG_PATH.open("r", encoding="utf-8", errors="ignore") as fh:
            for raw_line in fh:
                line = raw_line.strip()
                if line.lower().startswith("host "):
                    tokens = line.split()
                    if len(tokens) > 1 and tokens[1] == host:
                        return True, "找到匹配的 Host 条目"
    except Exception as exc:  # pragma: no cover - defensive guard
        return False, f"读取失败: {exc}"

    return False, "未找到对应 Host 条目"


def check_passwords_entry(host: str) -> Tuple[bool, str]:
    if not PASSWORDS_PATH.exists():
        return False, "passwords.json 不存在"

    try:
        data = json.loads(PASSWORDS_PATH.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        return False, f"JSON 解析失败: {exc}"
    except Exception as exc:  # pragma: no cover - defensive guard
        return False, f"读取失败: {exc}"

    if host in data:
        entry = data[host]
        info_bits = []
        if "password" in entry and entry["password"]:
            info_bits.append("包含 password")
        else:
            info_bits.append("缺少 password")
        if "otpauthUrl" in entry and entry["otpauthUrl"]:
            info_bits.append("包含 otpauthUrl")
        else:
            info_bits.append("缺少 otpauthUrl")
        return True, "，".join(info_bits)

    return False, "未找到该 Host 的配置"


def build_status_table(host: str, socket_path: Path) -> Table:
    table = Table(title=f"Auto2FA 状态概览 - {host}", box=box.SIMPLE_HEAD, expand=True)
    table.add_column("检查项", justify="left", style="cyan", no_wrap=True)
    table.add_column("状态", justify="center", style="bold")
    table.add_column("说明", justify="left", style="white")

    exists = socket_path.exists()
    table.add_row(
        "控制 Socket 文件",
        format_bool(exists),
        str(socket_path) if exists else "尚未创建（请先运行 auto2fa.py）",
    )

    control_ok, control_detail = check_control_master(host, socket_path)
    table.add_row("ControlMaster 状态", format_bool(control_ok), control_detail)

    ssh_ok, ssh_detail = check_ssh_config(host)
    table.add_row("SSH 配置", format_bool(ssh_ok), ssh_detail)

    pwd_ok, pwd_detail = check_passwords_entry(host)
    table.add_row("passwords.json", format_bool(pwd_ok), pwd_detail)

    if LOG_PATH.exists():
        log_ts = LOG_PATH.stat().st_mtime
        table.add_row("日志更新时间", "", format_timestamp(log_ts))
    else:
        table.add_row("日志文件", format_bool(False), "日志文件不存在")

    return table


def build_paths_panel(host: str, socket_path: Path) -> Panel:
    lines = [
        f"[bold]Host:[/] {host}",
        f"[bold]SSH config:[/] {SSH_CONFIG_PATH}",
        f"[bold]passwords.json:[/] {PASSWORDS_PATH}",
        f"[bold]Control socket:[/] {socket_path}",
        f"[bold]Log file:[/] {LOG_PATH}",
        "",
        f"[bold]刷新时间:[/] {datetime.now().strftime('%Y-%m-%d %H:%M:%S')}",
    ]
    return Panel(Text("\n".join(lines)), title="路径信息", box=box.SIMPLE)


def build_log_panel(lines: Iterable[str]) -> Panel:
    if not lines:
        content = Text("暂无日志或日志文件不存在", style="yellow")
    else:
        content = Text("\n".join(lines), style="white")
    return Panel(content, title="/tmp/auto2fa.log (最新)" , box=box.MINIMAL)


def render_dashboard(host: str, socket_path: Path, log_lines: int) -> Layout:
    layout = Layout()
    layout.split_column(
        Layout(name="upper", ratio=2),
        Layout(name="lower", ratio=1),
    )
    layout["upper"].split_row(
        Layout(name="status", ratio=2),
        Layout(name="paths", ratio=1),
    )

    layout["status"].update(build_status_table(host, socket_path))
    layout["paths"].update(build_paths_panel(host, socket_path))

    recent_logs = read_tail(LOG_PATH, log_lines)
    layout["lower"].update(build_log_panel(recent_logs))

    return layout


def run_monitor(host: str, refresh: float = DEFAULT_REFRESH_SECONDS, log_lines: int = DEFAULT_LOG_LINES) -> None:
    socket_path = build_control_socket_path(host)
    console = Console()

    try:
        with Live(
            render_dashboard(host, socket_path, log_lines),
            console=console,
            refresh_per_second=max(1, int(1 / max(refresh, 0.1))),
            screen=True,
        ) as live:
            while True:
                live.update(render_dashboard(host, socket_path, log_lines), refresh=True)
                time.sleep(max(refresh, 0.1))
    except KeyboardInterrupt:
        console.print("\n[bold yellow]监控已停止[/]")
