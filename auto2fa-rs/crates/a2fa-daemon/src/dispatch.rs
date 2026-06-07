//! IPC dispatch — the single authoritative entry point for one request line.
//!
//! # Design
//!
//! `dispatch(state, line)` takes raw bytes (so it can handle the invalid-UTF-8
//! case) and returns a complete JSON response line (including the trailing `\n`).
//!
//! The returned string is ready to be written directly to the client socket.
//!
//! # Line framing (mirrors `_handle_client` in daemon.py)
//!
//! 1. Invalid UTF-8 → `invalid_request`.
//! 2. Valid UTF-8 but not a JSON object (e.g. `"5\n"`, `"[]\n"`) → `invalid_request`.
//! 3. JSON object, unknown `method` → `unknown_method`.
//! 4. Known method → delegate to the appropriate handler.
//!
//! # Note on `subscribe_events`
//!
//! The `subscribe_events` method requires access to the underlying `UnixStream`
//! to register the subscriber.  It is handled directly in `server.rs`'s
//! connection loop **before** `dispatch` is called.  If `dispatch` receives it
//! anyway (e.g. in tests), it returns a normal ack.

use std::sync::{Arc, Mutex};

use a2fa_core::engine::State;
use a2fa_core::error::Error;
use a2fa_core::proto::{encode_error, encode_response, ErrCode, Method, Request};
use serde_json::Value;

use crate::handlers::{hosts, system, tunnels};
use crate::managers::HostManagers;
use crate::tunnel_runtime::TunnelRuntime;
use crate::workers::OtpRegistry;

/// Shared daemon-wide context threaded through the dispatch layer.
///
/// Bundles the Arc-wrapped singletons that handlers may need so we can add
/// new context objects in one place without changing every function signature.
#[derive(Clone)]
pub struct DaemonCtx {
    pub state: Arc<Mutex<State>>,
    pub managers: Arc<HostManagers>,
    pub registry: Arc<OtpRegistry>,
    pub runtime: Arc<TunnelRuntime>,
}

/// Dispatch one request line (raw bytes) and return the response line.
///
/// Always returns a complete, newline-terminated JSON string.
pub fn dispatch(state: &Arc<Mutex<State>>, line: &[u8]) -> String {
    // 1. UTF-8 decode.
    let text = match std::str::from_utf8(line) {
        Ok(s) => s,
        Err(_) => {
            return encode_error("", ErrCode::InvalidRequest, "invalid UTF-8");
        }
    };

    // 2. JSON parse.
    let raw: Value = match serde_json::from_str(text.trim_end()) {
        Ok(v) => v,
        Err(_) => {
            return encode_error("", ErrCode::InvalidRequest, "bad JSON");
        }
    };

    // 3. Must be an object.
    if !raw.is_object() {
        return encode_error("", ErrCode::InvalidRequest, "request must be a JSON object");
    }

    // 4. Deserialize into Request.
    let req: Request = match serde_json::from_value(raw) {
        Ok(r) => r,
        Err(e) => {
            return encode_error("", ErrCode::InvalidRequest, &e.to_string());
        }
    };

    let id = &req.id;
    let params = &req.params;

    // 5. Route method.
    let method = match Method::from_str(&req.method) {
        Some(m) => m,
        None => {
            return encode_error(id, ErrCode::UnknownMethod, &format!("unknown method {}", req.method));
        }
    };

    let result = route_with_ctx(&DaemonCtx {
        state: Arc::clone(state),
        managers: HostManagers::new(),
        registry: OtpRegistry::new(),
        runtime: TunnelRuntime::new(),
    }, method, params);

    match result {
        Ok(value) => encode_response(id, value),
        Err(e) => encode_error(id, e.to_errcode(), &e.to_string()),
    }
}

/// Full dispatch with daemon context (persistent managers + OTP registry).
///
/// This is used by the production server path. `dispatch` (above) is kept for
/// backward compatibility with existing unit tests that don't supply a context.
pub fn dispatch_with_ctx(ctx: &DaemonCtx, line: &[u8]) -> String {
    // 1. UTF-8 decode.
    let text = match std::str::from_utf8(line) {
        Ok(s) => s,
        Err(_) => {
            return encode_error("", ErrCode::InvalidRequest, "invalid UTF-8");
        }
    };

    // 2. JSON parse.
    let raw: Value = match serde_json::from_str(text.trim_end()) {
        Ok(v) => v,
        Err(_) => {
            return encode_error("", ErrCode::InvalidRequest, "bad JSON");
        }
    };

    // 3. Must be an object.
    if !raw.is_object() {
        return encode_error("", ErrCode::InvalidRequest, "request must be a JSON object");
    }

    // 4. Deserialize into Request.
    let req: Request = match serde_json::from_value(raw) {
        Ok(r) => r,
        Err(e) => {
            return encode_error("", ErrCode::InvalidRequest, &e.to_string());
        }
    };

    let id = &req.id;
    let params = &req.params;

    // 5. Route method.
    let method = match Method::from_str(&req.method) {
        Some(m) => m,
        None => {
            return encode_error(id, ErrCode::UnknownMethod, &format!("unknown method {}", req.method));
        }
    };

    let result = route_with_ctx(ctx, method, params);

    match result {
        Ok(value) => encode_response(id, value),
        Err(e) => encode_error(id, e.to_errcode(), &e.to_string()),
    }
}

/// Route a parsed, validated request to the correct handler.
/// Used by the legacy `dispatch` path (unit tests).
fn route_with_ctx(
    ctx: &DaemonCtx,
    method: Method,
    params: &Value,
) -> Result<Value, Error> {
    let state = &ctx.state;
    match method {
        // --- System / utility ---
        Method::Ping              => hosts::ping(state),
        Method::LogTail           => system::log_tail(state, params),
        Method::WakeRecover       => system::wake_recover(state, params),
        Method::ResetAll          => system::reset_all(state, params),
        Method::SubscribeEvents   => Ok(system::subscribe_events_ack()),

        // --- Hosts ---
        Method::ListHosts         => hosts::list_hosts(state),
        Method::HostToggle        => hosts::host_toggle_managed(state, params,
                                        Some(Arc::clone(&ctx.managers)),
                                        Some(Arc::clone(&ctx.registry))),
        Method::HostMountToggle   => hosts::host_mount_toggle(state, params),
        Method::HostRotate        => hosts::host_rotate(state, params),
        Method::HostAdd           => hosts::host_add(state, params),
        Method::HostTestCredentials => hosts::host_test_credentials(state, params),

        // --- Tunnels (read/compute) ---
        Method::ListTunnels       => tunnels::list_tunnels(state),
        Method::PortSuggest       => tunnels::port_suggest(state, params),
        Method::TunnelEvents      => tunnels::tunnel_events(state, params, Some(Arc::clone(&ctx.runtime))),

        // --- Tunnels (write/persist) ---
        Method::TunnelAdd         => tunnels::tunnel_add(state, params),
        Method::TunnelRemove      => tunnels::tunnel_remove(state, params, Some(Arc::clone(&ctx.runtime))),
        Method::TunnelStart       => tunnels::tunnel_start(state, params, Some(Arc::clone(&ctx.runtime))),
        Method::TunnelStop        => tunnels::tunnel_stop(state, params, Some(Arc::clone(&ctx.runtime))),
        Method::TunnelToggle      => tunnels::tunnel_toggle(state, params, Some(Arc::clone(&ctx.runtime))),
        Method::TunnelSetNode     => tunnels::tunnel_set_node(state, params, Some(Arc::clone(&ctx.runtime))),
        Method::TunnelSetAutostart=> tunnels::tunnel_set_autostart(state, params),
        Method::TunnelSetJumpCandidates => tunnels::tunnel_set_jump_candidates(state, params),
        Method::TunnelSetPostConnect    => tunnels::tunnel_set_post_connect(state, params),
        Method::TunnelSetTags     => tunnels::tunnel_set_tags(state, params),
        Method::TunnelSetUrlPath  => tunnels::tunnel_set_url_path(state, params),
        Method::TunnelRename      => tunnels::tunnel_rename(state, params),
        Method::TunnelsBatch      => tunnels::tunnels_batch(state, params),

        // --- Discovery ---
        Method::DiscoverNodes     => tunnels::discover_nodes(state, params),
    }
}

// ---------------------------------------------------------------------------
// Tests (unit — no socket required)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use a2fa_core::engine::State;
    use a2fa_core::model::{Tunnel, TunnelStatus};
    use std::sync::{Arc, Mutex};

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

    fn parse_result(line: &str) -> Value {
        serde_json::from_str(line.trim_end()).expect("response should be valid JSON")
    }

    // -----------------------------------------------------------------------
    // Framing / error-path tests
    // -----------------------------------------------------------------------

    #[test]
    fn invalid_utf8_line_is_invalid_request() {
        let state = empty_state();
        // b"\xff\xfe" is invalid UTF-8
        let line = b"\xff\xfe{\"id\":\"1\",\"method\":\"ping\",\"params\":{}}\n";
        let resp = dispatch(&state, line);
        let v = parse_result(&resp);
        assert_eq!(v["error"]["code"], "invalid_request");
    }

    #[test]
    fn non_object_request_is_invalid_request() {
        let state = empty_state();
        let line = b"5\n";
        let resp = dispatch(&state, line);
        let v = parse_result(&resp);
        assert_eq!(v["error"]["code"], "invalid_request");
    }

    #[test]
    fn non_object_array_is_invalid_request() {
        let state = empty_state();
        let line = b"[1,2,3]\n";
        let resp = dispatch(&state, line);
        let v = parse_result(&resp);
        assert_eq!(v["error"]["code"], "invalid_request");
    }

    #[test]
    fn unknown_method_returns_unknown_method() {
        let state = empty_state();
        let line = b"{\"id\":\"1\",\"method\":\"nope\",\"params\":{}}\n";
        let resp = dispatch(&state, line);
        let v = parse_result(&resp);
        assert_eq!(v["error"]["code"], "unknown_method");
    }

    // -----------------------------------------------------------------------
    // Happy-path tests
    // -----------------------------------------------------------------------

    #[test]
    fn ping_returns_ok() {
        let state = empty_state();
        let line = b"{\"id\":\"1\",\"method\":\"ping\",\"params\":{}}\n";
        let resp = dispatch(&state, line);
        let v = parse_result(&resp);
        assert_eq!(v["id"], "1");
        assert_eq!(v["result"]["ok"], true);
        assert!(v["result"]["pid"].as_u64().unwrap() > 0);
    }

    #[test]
    fn list_tunnels_returns_array() {
        let state = state_with_tunnel("nb", 9876);
        let line = b"{\"id\":\"2\",\"method\":\"list_tunnels\",\"params\":{}}\n";
        let resp = dispatch(&state, line);
        let v = parse_result(&resp);
        let arr = v["result"].as_array().expect("result should be array");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["name"], "nb");
    }

    #[test]
    fn list_hosts_returns_empty_array_for_empty_state() {
        let state = empty_state();
        let line = b"{\"id\":\"3\",\"method\":\"list_hosts\",\"params\":{}}\n";
        let resp = dispatch(&state, line);
        let v = parse_result(&resp);
        assert!(v["result"].as_array().unwrap().is_empty());
    }

    #[test]
    fn response_is_newline_terminated() {
        let state = empty_state();
        let line = b"{\"id\":\"1\",\"method\":\"ping\",\"params\":{}}\n";
        let resp = dispatch(&state, line);
        assert!(resp.ends_with('\n'), "response must end with newline");
    }

    #[test]
    fn id_is_echoed_back() {
        let state = empty_state();
        let line = b"{\"id\":\"req-42\",\"method\":\"ping\",\"params\":{}}\n";
        let resp = dispatch(&state, line);
        let v = parse_result(&resp);
        assert_eq!(v["id"], "req-42");
    }

    #[test]
    fn port_suggest_returns_port() {
        let state = empty_state();
        let line = b"{\"id\":\"1\",\"method\":\"port_suggest\",\"params\":{}}\n";
        let resp = dispatch(&state, line);
        let v = parse_result(&resp);
        assert!(v["result"]["port"].as_u64().unwrap() >= 1024);
    }

    #[test]
    fn tunnel_events_dispatch_returns_events_shape() {
        let state = state_with_tunnel("nb", 9902);
        let line = b"{\"id\":\"ev1\",\"method\":\"tunnel_events\",\"params\":{\"name\":\"nb\"}}\n";
        let resp = dispatch(&state, line);
        let v = parse_result(&resp);
        // Must have result.events as an array (empty since the legacy dispatch path
        // creates a fresh TunnelRuntime with no recorded events).
        assert!(v["result"]["events"].as_array().is_some(), "tunnel_events must return {{events:[...]}}");
    }

    #[test]
    fn tunnel_events_dispatch_unknown_tunnel_returns_not_found() {
        let state = empty_state();
        let line = b"{\"id\":\"ev2\",\"method\":\"tunnel_events\",\"params\":{\"name\":\"ghost\"}}\n";
        let resp = dispatch(&state, line);
        let v = parse_result(&resp);
        assert_eq!(v["error"]["code"], "not_found");
    }
}
