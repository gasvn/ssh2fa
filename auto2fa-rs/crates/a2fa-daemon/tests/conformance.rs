//! Protocol conformance harness — Rust daemon vs Python oracle.
//!
//! # Purpose
//! For each READ-ONLY IPC method, send the SAME request to both the running
//! Python daemon (`~/.ssh2fa/ssh2fa.sock`) and a freshly-started Rust daemon
//! on a temp socket, then assert that the responses are *structurally* equal
//! after normalising legitimately-volatile fields.
//!
//! # Requirements
//! - The Python daemon MUST be running on `~/.ssh2fa/ssh2fa.sock`.
//! - The Rust daemon binary MUST be built: `cargo build -p a2fa-daemon`.
//! - Run with:
//!   `cargo test --test conformance -- --ignored --nocapture`
//!
//! # Normalization rules (fields that legitimately differ between the two daemons)
//! - `ping` : strip `pid` (each process has its own PID).
//! - `list_hosts` : assert same SET OF HOST NAMES + same KEYS per object;
//!   runtime values (status, pool_alive, is_master_ready, pool_index, last_msg)
//!   are stripped before comparison.
//! - `list_tunnels` : assert same SET OF TUNNEL NAMES + same KEYS per object
//!   + same config values (local_port, remote_port, auto_start, tags);
//!   runtime values (total_uptime_sec, last_alive_at, status, last_msg,
//!   active_jump, connect_count, fail_count) are stripped.
//! - `port_suggest` : only assert that `port` is an integer >= 1024; the two
//!   daemons may legitimately return different ports (Rust daemon has no live
//!   tunnels holding ports).
//! - `log_tail` : assert only SHAPE (`{"lines": [...]}`) — both daemons log to
//!   the SAME file but the Rust daemon (brand-new) may have written nothing yet.

// The module doc above uses multi-line list items for readability; clippy's
// markdown-continuation lint is noise here.
#![allow(clippy::doc_lazy_continuation)]

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

// ── Paths ──────────────────────────────────────────────────────────────────

fn python_sock() -> String {
    std::env::var("HOME").unwrap() + "/.ssh2fa/ssh2fa.sock"
}

const RUST_SOCK: &str = "/tmp/a2fa-conf.sock";
const RUST_LOCK: &str = "/tmp/a2fa-conf.lock";

// ── IPC helper ─────────────────────────────────────────────────────────────

fn send(sock_path: &str, method: &str, params: Value) -> Result<Value, String> {
    let stream = UnixStream::connect(sock_path)
        .map_err(|e| format!("connect to {sock_path}: {e}"))?;
    let mut writer = stream.try_clone().map_err(|e| e.to_string())?;

    let req = json!({
        "id": format!("conf-{}", method),
        "method": method,
        "params": params,
    });
    let line = serde_json::to_string(&req).unwrap() + "\n";
    writer
        .write_all(line.as_bytes())
        .map_err(|e| format!("write: {e}"))?;
    drop(writer);

    let mut reader = BufReader::new(stream);
    let mut buf = String::new();
    reader
        .read_line(&mut buf)
        .map_err(|e| format!("read: {e}"))?;

    serde_json::from_str(buf.trim_end()).map_err(|e| format!("bad JSON: {e} — raw: {buf:?}"))
}

// ── Rust daemon lifecycle ───────────────────────────────────────────────────

struct RustDaemon {
    child: Child,
}

impl RustDaemon {
    fn start() -> Self {
        // Remove stale socket / lock from any previous crash.
        let _ = std::fs::remove_file(RUST_SOCK);
        let _ = std::fs::remove_file(RUST_LOCK);

        // Find binary relative to the integration test's working directory.
        // `cargo test --test conformance` sets CARGO_MANIFEST_DIR.
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
        // workspace root is two levels up from crates/a2fa-daemon
        let workspace_root = Path::new(&manifest_dir)
            .ancestors()
            .nth(2)
            .unwrap()
            .to_path_buf();
        let bin = workspace_root
            .join("target")
            .join("debug")
            .join("ssh2fa-daemon");

        let child = Command::new(&bin)
            .env("AUTO2FA_SOCK", RUST_SOCK)
            .env("AUTO2FA_LOCK", RUST_LOCK)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap_or_else(|e| panic!("failed to spawn {}: {e}", bin.display()));

        // Wait until the socket appears (up to 5 s).
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if Path::new(RUST_SOCK).exists() {
                // Socket file is there; try an actual connect.
                if UnixStream::connect(RUST_SOCK).is_ok() {
                    break;
                }
            }
            if Instant::now() > deadline {
                panic!("Rust daemon did not bind {} within 5 s", RUST_SOCK);
            }
            std::thread::sleep(Duration::from_millis(100));
        }

        RustDaemon { child }
    }
}

impl Drop for RustDaemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(RUST_SOCK);
        let _ = std::fs::remove_file(RUST_LOCK);
    }
}

// ── Normalization helpers ───────────────────────────────────────────────────

/// Strip a set of keys from a JSON object; returns the modified value.
fn strip_keys(mut v: Value, keys: &[&str]) -> Value {
    if let Value::Object(ref mut m) = v {
        for k in keys {
            m.remove(*k);
        }
    }
    v
}

/// Extract the set of string values for `key` from an array of objects.
fn name_set(arr: &[Value], key: &str) -> BTreeSet<String> {
    arr.iter()
        .filter_map(|v| v.get(key)?.as_str().map(String::from))
        .collect()
}

/// Extract the set of keys from a JSON object.
fn key_set(v: &Value) -> BTreeSet<String> {
    v.as_object()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default()
}

// ── Per-method comparison logic ─────────────────────────────────────────────

/// Returns Ok(()) on PASS or Err(message) on FAIL.
type CheckResult = Result<(), String>;

fn check_ping(py_resp: &Value, rs_resp: &Value) -> CheckResult {
    // Strip pid from both; compare the rest.
    let volatile = ["pid"];
    let py_r = strip_keys(py_resp["result"].clone(), &volatile);
    let rs_r = strip_keys(rs_resp["result"].clone(), &volatile);
    if py_r != rs_r {
        return Err(format!(
            "ping result mismatch after stripping pid:\n  Python: {py_r}\n  Rust:   {rs_r}"
        ));
    }
    Ok(())
}

fn check_list_hosts(py_resp: &Value, rs_resp: &Value) -> CheckResult {
    let py_arr = py_resp["result"]
        .as_array()
        .ok_or("Python list_hosts result is not an array")?;
    let rs_arr = rs_resp["result"]
        .as_array()
        .ok_or("Rust list_hosts result is not an array")?;

    // Same set of host names?
    let py_names = name_set(py_arr, "host");
    let rs_names = name_set(rs_arr, "host");
    if py_names != rs_names {
        return Err(format!(
            "list_hosts name mismatch:\n  Python: {py_names:?}\n  Rust:   {rs_names:?}"
        ));
    }

    // Same shape (keys) per host object?
    let runtime_keys = [
        "status",
        "pool_alive",
        "is_master_ready",
        "pool_index",
        "last_msg",
        "is_mounted", // may differ — ignore
        "active",     // may differ — ignore
    ];
    for py_obj in py_arr {
        let host = py_obj["host"].as_str().unwrap_or("?");
        let rs_obj = rs_arr
            .iter()
            .find(|o| o["host"].as_str() == Some(host))
            .ok_or_else(|| format!("host {host} missing from Rust response"))?;

        let py_keys: BTreeSet<String> = key_set(py_obj);
        let rs_keys: BTreeSet<String> = key_set(rs_obj);
        if py_keys != rs_keys {
            return Err(format!(
                "list_hosts key shape mismatch for host {host}:\n  Python keys: {py_keys:?}\n  Rust keys:   {rs_keys:?}"
            ));
        }

        // Compare stable (non-runtime) field values.
        let py_stable = strip_keys(py_obj.clone(), &runtime_keys);
        let rs_stable = strip_keys(rs_obj.clone(), &runtime_keys);
        if py_stable != rs_stable {
            return Err(format!(
                "list_hosts stable-field mismatch for host {host}:\n  Python: {py_stable}\n  Rust:   {rs_stable}"
            ));
        }
    }

    Ok(())
}

fn check_list_tunnels(py_resp: &Value, rs_resp: &Value) -> CheckResult {
    let py_arr = py_resp["result"]
        .as_array()
        .ok_or("Python list_tunnels result is not an array")?;
    let rs_arr = rs_resp["result"]
        .as_array()
        .ok_or("Rust list_tunnels result is not an array")?;

    // Same set of tunnel names?
    let py_names = name_set(py_arr, "name");
    let rs_names = name_set(rs_arr, "name");
    if py_names != rs_names {
        return Err(format!(
            "list_tunnels name mismatch:\n  Python: {py_names:?}\n  Rust:   {rs_names:?}"
        ));
    }

    for py_obj in py_arr {
        let name = py_obj["name"].as_str().unwrap_or("?");
        let rs_obj = rs_arr
            .iter()
            .find(|o| o["name"].as_str() == Some(name))
            .ok_or_else(|| format!("tunnel {name} missing from Rust response"))?;

        // Shape parity: same keys?
        let py_keys: BTreeSet<String> = key_set(py_obj);
        let rs_keys: BTreeSet<String> = key_set(rs_obj);
        if py_keys != rs_keys {
            return Err(format!(
                "list_tunnels key shape mismatch for tunnel {name}:\n  Python keys: {py_keys:?}\n  Rust keys:   {rs_keys:?}"
            ));
        }

        // Config field parity (must be identical).
        let config_keys = ["local_port", "remote_port", "auto_start", "tags",
                           "jump_candidates", "post_connect_cmd", "url_path"];
        for ck in config_keys {
            if py_obj.get(ck) != rs_obj.get(ck) {
                return Err(format!(
                    "list_tunnels config field '{ck}' mismatch for tunnel {name}:\n  Python: {}\n  Rust:   {}",
                    py_obj[ck], rs_obj[ck]
                ));
            }
        }
    }

    Ok(())
}

fn check_port_suggest(py_resp: &Value, rs_resp: &Value) -> CheckResult {
    // Both must return `{"port": <int >= 1024>}`.
    let py_port = py_resp["result"]["port"]
        .as_u64()
        .ok_or_else(|| format!("Python port_suggest missing port: {}", py_resp["result"]))?;
    let rs_port = rs_resp["result"]["port"]
        .as_u64()
        .ok_or_else(|| format!("Rust port_suggest missing port: {}", rs_resp["result"]))?;

    if py_port < 1024 {
        return Err(format!("Python port_suggest returned port < 1024: {py_port}"));
    }
    if rs_port < 1024 {
        return Err(format!("Rust port_suggest returned port < 1024: {rs_port}"));
    }
    // The two ports may differ legitimately (Rust has no live tunnels to skip).
    // We only check shape here, not equality.
    Ok(())
}

fn check_log_tail(py_resp: &Value, rs_resp: &Value) -> CheckResult {
    // Just assert shape: result must have `lines` key that is an array.
    let py_lines = py_resp["result"]
        .get("lines")
        .ok_or_else(|| format!("Python log_tail missing 'lines': {}", py_resp["result"]))?;
    let rs_lines = rs_resp["result"]
        .get("lines")
        .ok_or_else(|| format!("Rust log_tail missing 'lines': {}", rs_resp["result"]))?;

    if !py_lines.is_array() {
        return Err(format!("Python log_tail 'lines' is not an array: {py_lines}"));
    }
    if !rs_lines.is_array() {
        return Err(format!("Rust log_tail 'lines' is not an array: {rs_lines}"));
    }
    Ok(())
}

// ── Main harness ────────────────────────────────────────────────────────────

struct TestCase {
    method: &'static str,
    params: Value,
}

fn run_harness() {
    let py_sock = python_sock();

    println!("\n=== Protocol Conformance Harness ===");
    println!("Python oracle : {py_sock}");
    println!("Rust daemon   : {RUST_SOCK}");

    // Start Rust daemon (auto-killed in Drop).
    println!("\nStarting Rust daemon …");
    let _rust = RustDaemon::start();
    println!("Rust daemon ready.");

    let cases: Vec<TestCase> = vec![
        TestCase { method: "ping",         params: json!({}) },
        TestCase { method: "list_hosts",   params: json!({}) },
        TestCase { method: "list_tunnels", params: json!({}) },
        TestCase { method: "port_suggest", params: json!({"base": 9000}) },
        TestCase { method: "log_tail",     params: json!({"lines": 5}) },
    ];

    let mut pass_count = 0usize;
    let mut fail_count = 0usize;

    for tc in &cases {
        let py_resp = match send(&py_sock, tc.method, tc.params.clone()) {
            Ok(v) => v,
            Err(e) => {
                println!("FAIL  {} — Python IPC error: {e}", tc.method);
                fail_count += 1;
                continue;
            }
        };

        let rs_resp = match send(RUST_SOCK, tc.method, tc.params.clone()) {
            Ok(v) => v,
            Err(e) => {
                println!("FAIL  {} — Rust IPC error: {e}", tc.method);
                fail_count += 1;
                continue;
            }
        };

        // Verify there's no error-level response from either side.
        if py_resp.get("error").is_some() {
            println!("FAIL  {} — Python returned error: {}", tc.method, py_resp["error"]);
            fail_count += 1;
            continue;
        }
        if rs_resp.get("error").is_some() {
            println!("FAIL  {} — Rust returned error: {}", tc.method, rs_resp["error"]);
            fail_count += 1;
            continue;
        }

        let result = match tc.method {
            "ping"         => check_ping(&py_resp, &rs_resp),
            "list_hosts"   => check_list_hosts(&py_resp, &rs_resp),
            "list_tunnels" => check_list_tunnels(&py_resp, &rs_resp),
            "port_suggest" => check_port_suggest(&py_resp, &rs_resp),
            "log_tail"     => check_log_tail(&py_resp, &rs_resp),
            other          => Err(format!("unknown test case: {other}")),
        };

        match result {
            Ok(()) => {
                println!("PASS  {}", tc.method);
                pass_count += 1;
            }
            Err(msg) => {
                println!("FAIL  {} — {msg}", tc.method);
                fail_count += 1;
            }
        }
    }

    println!("\n--- Results: {pass_count} PASS, {fail_count} FAIL ---\n");

    if fail_count > 0 {
        panic!("{fail_count} method(s) failed conformance");
    }
}

// ── Test entry point ────────────────────────────────────────────────────────

#[test]
#[ignore = "requires Python daemon on ~/.ssh2fa/ssh2fa.sock and built Rust daemon binary"]
fn protocol_conformance() {
    run_harness();
}
