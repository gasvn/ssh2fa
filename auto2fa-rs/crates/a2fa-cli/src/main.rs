//! `a2fa-cli` — command-line client for the auto2fa daemon.
//!
//! With no subcommand, the `a2fa-tui` binary is launched.
//! With a subcommand, the request is sent over the Unix socket and the
//! result is printed to stdout.

mod cli;
mod client;

use std::process::{self, Command};

use anyhow::{anyhow, bail, Result};
use clap::Parser;
use serde_json::Value;

use cli::{Cli, Commands};

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e}");
        process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();

    match &cli.command {
        None => launch_tui(),
        Some(Commands::List) => cmd_list(),
        Some(cmd) => {
            // For Raw, parse the params string here so we can give a proper
            // user-facing error rather than silently swallowing bad JSON.
            let (method, params) = if let Commands::Raw { method, params } = cmd {
                let p = match params {
                    None => Value::Object(Default::default()),
                    Some(s) => serde_json::from_str(s)
                        .map_err(|e| anyhow!("invalid JSON for params: {e}"))?,
                };
                (method.clone(), p)
            } else {
                cli::to_request(cmd)
            };

            let result = client::rpc(&method, params)?;
            print_result(&method, result);
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// TUI launcher
// ---------------------------------------------------------------------------

fn launch_tui() -> Result<()> {
    // Look for a2fa-tui next to the current executable first, then fall back
    // to PATH.
    let tui_name = "a2fa-tui";

    let next_to_exe: Option<std::path::PathBuf> = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(tui_name)))
        .filter(|p| p.exists());

    let tui_path: std::ffi::OsString = if let Some(p) = next_to_exe {
        p.into_os_string()
    } else {
        // Fall back to PATH — `Command` will search PATH automatically when
        // given a plain name.
        tui_name.into()
    };

    let status = Command::new(&tui_path).status();
    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => {
            let code = s.code().unwrap_or(1);
            process::exit(code);
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!(
                "a2fa-tui not found — install it or run a subcommand (try `a2fa-cli --help`)"
            );
            process::exit(1);
        }
        Err(e) => bail!("failed to launch a2fa-tui: {e}"),
    }
}

// ---------------------------------------------------------------------------
// `list` combines hosts + tunnels into one output
// ---------------------------------------------------------------------------

fn cmd_list() -> Result<()> {
    let hosts = client::rpc("list_hosts", serde_json::json!({}))?;
    print_hosts(&hosts);
    println!();
    let tunnels = client::rpc("list_tunnels", serde_json::json!({}))?;
    print_tunnels(&tunnels);
    Ok(())
}

// ---------------------------------------------------------------------------
// Formatted output helpers
// ---------------------------------------------------------------------------

fn print_result(method: &str, result: Value) {
    match method {
        "list_hosts" => print_hosts(&result),
        "list_tunnels" => print_tunnels(&result),
        "log_tail" => print_log_tail(&result),
        "wake_recover" => print_wake(&result),
        _ => {
            // For start/stop/toggle/node and raw, print JSON or a short OK.
            if result.is_null() || result == Value::Object(Default::default()) {
                println!("{method}: OK");
            } else {
                println!("{}", serde_json::to_string_pretty(&result).unwrap_or_default());
            }
        }
    }
}

fn status_glyph(state: &str, is_tty: bool) -> &'static str {
    if !is_tty {
        return match state {
            "alive" | "connected" => "[up]",
            "starting" | "connecting" => "[..]",
            "failed" | "stale" | "port_busy" => "[!!]",
            _ => "[--]",
        };
    }
    match state {
        "alive" | "connected" => "\x1b[32m●\x1b[0m",
        "starting" | "connecting" => "\x1b[33m◐\x1b[0m",
        "failed" | "stale" | "port_busy" => "\x1b[31m●\x1b[0m",
        _ => "\x1b[37m○\x1b[0m",
    }
}

fn is_tty() -> bool {
    libc_isatty(1)
}

#[cfg(unix)]
fn libc_isatty(fd: i32) -> bool {
    extern "C" {
        fn isatty(fd: i32) -> i32;
    }
    unsafe { isatty(fd) != 0 }
}

#[cfg(not(unix))]
fn libc_isatty(_fd: i32) -> bool {
    false
}

fn print_hosts(result: &Value) {
    let tty = is_tty();
    let bold_start = if tty { "\x1b[1m" } else { "" };
    let bold_end = if tty { "\x1b[0m" } else { "" };
    println!("{bold_start}HOSTS{bold_end}");
    let hosts = match result.as_array() {
        Some(a) => a,
        None => {
            println!("  (no data)");
            return;
        }
    };
    for h in hosts {
        let host = h.get("host").and_then(Value::as_str).unwrap_or("?");
        let ready = h
            .get("is_master_ready")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let state = if ready { "connected" } else { "stopped" };
        let last_msg = h
            .get("last_msg")
            .and_then(Value::as_str)
            .unwrap_or("")
            .chars()
            .take(50)
            .collect::<String>();
        let glyph = status_glyph(state, tty);
        // Single-master: one connection per host, no pool index/count.
        println!("  {glyph} {host:<40} {state:<12}  {last_msg}");
    }
}

fn print_tunnels(result: &Value) {
    let tty = is_tty();
    let bold_start = if tty { "\x1b[1m" } else { "" };
    let bold_end = if tty { "\x1b[0m" } else { "" };
    println!("{bold_start}TUNNELS{bold_end}");
    let tunnels = match result.as_array() {
        Some(a) => a,
        None => {
            println!("  (no data)");
            return;
        }
    };
    if tunnels.is_empty() {
        println!("  (none)");
        return;
    }
    for t in tunnels {
        let name = t.get("name").and_then(Value::as_str).unwrap_or("?");
        let status = t.get("status").and_then(Value::as_str).unwrap_or("unknown");
        let glyph = status_glyph(status, tty);
        let auto = if t.get("auto_start").and_then(Value::as_bool).unwrap_or(false) {
            "*"
        } else {
            " "
        };
        let pinned = if t
            .get("jump_candidates")
            .and_then(Value::as_array)
            .map(|a| !a.is_empty())
            .unwrap_or(false)
        {
            "P"
        } else {
            " "
        };
        let local_port = t
            .get("local_port")
            .map(|v| v.to_string())
            .unwrap_or_else(|| "?".to_string());
        let remote_port = t
            .get("remote_port")
            .map(|v| v.to_string())
            .unwrap_or_else(|| "?".to_string());
        let jump = t
            .get("active_jump")
            .and_then(Value::as_str)
            .unwrap_or("—");
        let node = t
            .get("last_node")
            .and_then(Value::as_str)
            .unwrap_or("—");
        let last_msg = t
            .get("last_msg")
            .and_then(Value::as_str)
            .unwrap_or("")
            .chars()
            .take(30)
            .collect::<String>();
        println!(
            "  {glyph} {auto}{pinned} {name:<20} :{local_port}->:{remote_port} \
             via {jump:<10} node={node}  {last_msg}"
        );
    }
}

fn print_log_tail(result: &Value) {
    if let Some(lines) = result.get("lines").and_then(Value::as_array) {
        for line in lines {
            if let Some(s) = line.as_str() {
                println!("{s}");
            }
        }
    } else {
        // Fallback: dump JSON
        println!("{}", serde_json::to_string_pretty(result).unwrap_or_default());
    }
}

fn print_wake(result: &Value) {
    let count = result
        .get("tunnels_restarting")
        .and_then(Value::as_array)
        .map(|a| a.len())
        .unwrap_or(0);
    println!("wake_recover: restarting {count} tunnels");
}
