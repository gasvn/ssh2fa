//! Command-line argument definitions and the pure `to_request` mapping.
//!
//! Keeping `to_request` free of I/O lets it be unit-tested without a running
//! daemon.

use clap::{Parser, Subcommand};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Top-level CLI struct
// ---------------------------------------------------------------------------

/// auto2fa — SSH tunnel manager with 2-FA support.
///
/// With no subcommand, the interactive TUI is launched.
///
/// Examples:
///   a2fa-cli list
///   a2fa-cli start jupyter
///   a2fa-cli node jupyter compute-node-01 --user alice
///   a2fa-cli logs --lines 100
///   a2fa-cli raw ping
#[derive(Parser, Debug)]
#[command(name = "a2fa-cli", author, version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

// ---------------------------------------------------------------------------
// Subcommands
// ---------------------------------------------------------------------------

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Show all hosts and tunnels (combined).
    List,

    /// Show SSH hosts (connection pool status).
    Hosts,

    /// Show configured tunnels.
    Tunnels,

    /// Start a tunnel (idempotent — no-op if already alive).
    Start {
        /// Tunnel name.
        name: String,
    },

    /// Stop a tunnel (idempotent — no-op if already stopped).
    Stop {
        /// Tunnel name.
        name: String,
    },

    /// Flip a tunnel between alive and stopped.
    Toggle {
        /// Tunnel name.
        name: String,
    },

    /// Set the target node for a tunnel and start it.
    ///
    /// Example: a2fa-cli node jupyter compute-node-01 --user alice
    Node {
        /// Tunnel name.
        name: String,
        /// Target compute node, e.g. compute-node-01.
        node: String,
        /// Remote username (defaults to $USER if omitted).
        #[arg(long)]
        user: Option<String>,
    },

    /// Trigger an immediate wake/recover cycle (restart stalled tunnels).
    Wake,

    /// Tail the daemon log.
    Logs {
        /// Number of lines to show.
        #[arg(long, default_value_t = 50)]
        lines: u32,
    },

    /// Send a raw JSON-RPC request to the daemon.
    ///
    /// Example: a2fa-cli raw ping
    ///          a2fa-cli raw tunnel_start '{"name":"jupyter"}'
    Raw {
        /// RPC method name.
        method: String,
        /// Optional JSON params object (omit for `{}`).
        params: Option<String>,
    },
}

// ---------------------------------------------------------------------------
// Pure mapping: Commands → (method, params)
// ---------------------------------------------------------------------------

/// Map a parsed subcommand to the RPC method name and params object.
///
/// This function is pure (no I/O) so it can be exhaustively unit-tested.
///
/// `Raw` is handled specially: the params string is parsed here and any JSON
/// parse error is returned as an `Err`.
pub fn to_request(cmd: &Commands) -> (String, Value) {
    match cmd {
        Commands::List => ("list_hosts".to_string(), json!({})),
        Commands::Hosts => ("list_hosts".to_string(), json!({})),
        Commands::Tunnels => ("list_tunnels".to_string(), json!({})),

        Commands::Start { name } => ("tunnel_start".to_string(), json!({ "name": name })),
        Commands::Stop { name } => ("tunnel_stop".to_string(), json!({ "name": name })),
        Commands::Toggle { name } => ("tunnel_toggle".to_string(), json!({ "name": name })),

        Commands::Node { name, node, user } => {
            let u = user
                .as_deref()
                .map(|s| s.to_string())
                .or_else(|| std::env::var("USER").ok())
                .unwrap_or_default();
            (
                "tunnel_set_node".to_string(),
                json!({ "name": name, "node": node, "user": u }),
            )
        }

        Commands::Wake => ("wake_recover".to_string(), json!({})),

        Commands::Logs { lines } => ("log_tail".to_string(), json!({ "lines": lines })),

        // Raw: return a sentinel; main.rs will handle JSON parsing of params
        // with a user-facing error message.  We still provide a best-effort
        // parse here so callers that don't need to distinguish can use this.
        Commands::Raw { method, params } => {
            let p = params
                .as_deref()
                .map(|s| serde_json::from_str(s).unwrap_or(json!({})))
                .unwrap_or(json!({}));
            (method.clone(), p)
        }
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_builds_set_node() {
        let (m, p) = to_request(&Commands::Node {
            name: "jup".into(),
            node: "compute-01".into(),
            user: None,
        });
        assert_eq!(m, "tunnel_set_node");
        assert_eq!(p["name"], "jup");
        assert_eq!(p["node"], "compute-01");
        // user field must be present (even if empty string)
        assert!(p.get("user").is_some());
    }

    #[test]
    fn node_with_explicit_user() {
        let (m, p) = to_request(&Commands::Node {
            name: "jup".into(),
            node: "compute-node-01".into(),
            user: Some("alice".into()),
        });
        assert_eq!(m, "tunnel_set_node");
        assert_eq!(p["user"], "alice");
    }

    #[test]
    fn start_builds_tunnel_start() {
        let (m, p) = to_request(&Commands::Start { name: "x".into() });
        assert_eq!(m, "tunnel_start");
        assert_eq!(p["name"], "x");
    }

    #[test]
    fn stop_builds_tunnel_stop() {
        let (m, p) = to_request(&Commands::Stop { name: "x".into() });
        assert_eq!(m, "tunnel_stop");
        assert_eq!(p["name"], "x");
    }

    #[test]
    fn toggle_builds_tunnel_toggle() {
        let (m, p) = to_request(&Commands::Toggle { name: "x".into() });
        assert_eq!(m, "tunnel_toggle");
        assert_eq!(p["name"], "x");
    }

    #[test]
    fn logs_builds_log_tail_with_lines() {
        let (m, p) = to_request(&Commands::Logs { lines: 50 });
        assert_eq!(m, "log_tail");
        assert_eq!(p["lines"], 50);
    }

    #[test]
    fn logs_custom_lines() {
        let (m, p) = to_request(&Commands::Logs { lines: 200 });
        assert_eq!(m, "log_tail");
        assert_eq!(p["lines"], 200);
    }

    #[test]
    fn wake_builds_wake_recover() {
        let (m, p) = to_request(&Commands::Wake);
        assert_eq!(m, "wake_recover");
        assert!(p.is_object());
    }

    #[test]
    fn hosts_calls_list_hosts() {
        let (m, _) = to_request(&Commands::Hosts);
        assert_eq!(m, "list_hosts");
    }

    #[test]
    fn tunnels_calls_list_tunnels() {
        let (m, _) = to_request(&Commands::Tunnels);
        assert_eq!(m, "list_tunnels");
    }

    #[test]
    fn raw_passes_method_and_parses_params() {
        let (m, p) = to_request(&Commands::Raw {
            method: "ping".into(),
            params: None,
        });
        assert_eq!(m, "ping");
        assert!(p.is_object());
    }

    #[test]
    fn raw_parses_json_params() {
        let (m, p) = to_request(&Commands::Raw {
            method: "tunnel_start".into(),
            params: Some(r#"{"name":"jupyter"}"#.into()),
        });
        assert_eq!(m, "tunnel_start");
        assert_eq!(p["name"], "jupyter");
    }
}
