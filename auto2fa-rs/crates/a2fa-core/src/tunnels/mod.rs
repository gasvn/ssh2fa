pub mod cleanup;
pub mod discovery;
pub mod forward;
pub mod post_connect;
pub mod probe;
pub mod uptime;

// Convenience re-exports for the most-used items.
pub use cleanup::{cleanup_orphans, is_auto2fa_tunnel_proc, orphan_pattern};
pub use discovery::{discover_nodes, discover_nodes_via_control, expand_first_node, parse_squeue, Job};
pub use forward::{build_forward_argv, probe_and_settle, start_forward, stop_forward, ProbeOutcome};
pub use post_connect::run_post_connect;
pub use probe::{port_available, probe_port_ready};
pub use uptime::{live_uptime, now_unix};
