//! Integration tests for the dispatch function.
//!
//! These tests drive the pure `dispatch(state, line)` function with an
//! in-memory `State` — no socket or SSH connection required.

use a2fa_core::engine::State;
use a2fa_core::model::{Tunnel, TunnelStatus};
use a2fa_daemon::dispatch::dispatch;
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn empty_state() -> Arc<Mutex<State>> {
    Arc::new(Mutex::new(State::with_tunnels(vec![])))
}

fn state_with_tunnel(name: &str, port: u16) -> Arc<Mutex<State>> {
    let t = Tunnel {
        name: name.into(),
        local_port: port,
        remote_port: port,
        jump_candidates: None,
        last_node: None,
        last_user: None,
        auto_start: false,
        post_connect_cmd: None,
        tags: vec![],
        url_path: None,
        wants_alive: false,
        status: TunnelStatus::Idle,
        active_jump: None,
        last_msg: "Ready".into(),
        last_alive_at: 0.0,
        total_uptime_sec: 0.0,
        connect_count: 0,
        fail_count: 0,
    };
    Arc::new(Mutex::new(State::with_tunnels(vec![t])))
}

fn parse(line: &str) -> serde_json::Value {
    serde_json::from_str(line.trim_end()).expect("response must be valid JSON")
}

// ---------------------------------------------------------------------------
// Framing / error-path tests (TDD requirement)
// ---------------------------------------------------------------------------

#[test]
fn invalid_utf8_line_is_invalid_request() {
    let state = empty_state();
    // b"\xff\xfe" is invalid UTF-8 — should yield invalid_request
    let bad: Vec<u8> = {
        let mut v = b"\xff\xfe".to_vec();
        v.extend_from_slice(b"{\"id\":\"1\",\"method\":\"ping\",\"params\":{}}\n");
        v
    };
    let resp = dispatch(&state, &bad);
    let v = parse(&resp);
    assert_eq!(
        v["error"]["code"], "invalid_request",
        "expected invalid_request, got: {v}"
    );
}

#[test]
fn non_object_request_is_invalid_request() {
    let state = empty_state();
    let resp = dispatch(&state, b"5\n");
    let v = parse(&resp);
    assert_eq!(
        v["error"]["code"], "invalid_request",
        "expected invalid_request for bare number, got: {v}"
    );
}

#[test]
fn array_request_is_invalid_request() {
    let state = empty_state();
    let resp = dispatch(&state, b"[1,2,3]\n");
    let v = parse(&resp);
    assert_eq!(v["error"]["code"], "invalid_request");
}

#[test]
fn unknown_method_returns_unknown_method() {
    let state = empty_state();
    let resp = dispatch(&state, b"{\"id\":\"1\",\"method\":\"nope\",\"params\":{}}\n");
    let v = parse(&resp);
    assert_eq!(
        v["error"]["code"], "unknown_method",
        "expected unknown_method, got: {v}"
    );
}

// ---------------------------------------------------------------------------
// Happy-path tests (TDD requirement)
// ---------------------------------------------------------------------------

#[test]
fn ping_returns_ok() {
    let state = empty_state();
    let resp = dispatch(&state, b"{\"id\":\"1\",\"method\":\"ping\",\"params\":{}}\n");
    let v = parse(&resp);
    assert_eq!(v["id"], "1", "id should be echoed");
    assert_eq!(v["result"]["ok"], true);
    assert!(v["result"]["pid"].as_u64().unwrap() > 0);
}

#[test]
fn list_tunnels_returns_array() {
    // Seed state with 1 tunnel
    let state = state_with_tunnel("nb", 9876);
    let resp = dispatch(&state, b"{\"id\":\"2\",\"method\":\"list_tunnels\",\"params\":{}}\n");
    let v = parse(&resp);
    let arr = v["result"].as_array().expect("result should be an array");
    assert_eq!(arr.len(), 1, "expected 1 tunnel, got {}", arr.len());
    assert_eq!(arr[0]["name"], "nb");
}

// ---------------------------------------------------------------------------
// Additional coverage tests
// ---------------------------------------------------------------------------

#[test]
fn response_is_newline_terminated() {
    let state = empty_state();
    let resp = dispatch(&state, b"{\"id\":\"1\",\"method\":\"ping\",\"params\":{}}\n");
    assert!(resp.ends_with('\n'), "every response must end with \\n");
}

#[test]
fn list_hosts_returns_empty_array_for_empty_state() {
    let state = empty_state();
    let resp = dispatch(&state, b"{\"id\":\"3\",\"method\":\"list_hosts\",\"params\":{}}\n");
    let v = parse(&resp);
    assert!(v["result"].as_array().unwrap().is_empty());
}

#[test]
fn port_suggest_returns_free_port() {
    let state = empty_state();
    let resp = dispatch(&state, b"{\"id\":\"4\",\"method\":\"port_suggest\",\"params\":{}}\n");
    let v = parse(&resp);
    assert!(v["result"]["port"].as_u64().unwrap() >= 1024);
}

#[test]
fn log_tail_returns_lines_array() {
    let state = empty_state();
    let resp = dispatch(&state, b"{\"id\":\"5\",\"method\":\"log_tail\",\"params\":{\"lines\":5}}\n");
    let v = parse(&resp);
    // lines must be an array (could be empty if log doesn't exist)
    assert!(v["result"]["lines"].is_array(), "lines must be an array");
}

#[test]
fn reset_all_returns_counts() {
    let state = empty_state();
    let resp = dispatch(&state, b"{\"id\":\"6\",\"method\":\"reset_all\",\"params\":{}}\n");
    let v = parse(&resp);
    assert!(v["result"]["tunnels_stopped"].as_u64().is_some());
    assert!(v["result"]["masters_rebuilt"].as_u64().is_some());
}

#[test]
fn wake_recover_returns_tunnels_restarting() {
    let state = empty_state();
    let resp = dispatch(&state, b"{\"id\":\"7\",\"method\":\"wake_recover\",\"params\":{}}\n");
    let v = parse(&resp);
    assert!(v["result"]["tunnels_restarting"].is_array());
}

#[test]
fn subscribe_events_returns_ack() {
    let state = empty_state();
    let resp = dispatch(&state, b"{\"id\":\"8\",\"method\":\"subscribe_events\",\"params\":{}}\n");
    let v = parse(&resp);
    assert_eq!(v["result"]["subscribed"], true);
}
