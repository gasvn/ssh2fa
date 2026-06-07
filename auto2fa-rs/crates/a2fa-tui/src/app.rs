//! Pure application model (no I/O).
//!
//! All UI state lives here; `main.rs` feeds it events and the view modules
//! read it for rendering.  Keeping this I/O-free makes it directly testable.

use a2fa_core::model::{Host, Tunnel, TunnelStatus};
use ratatui::style::Color;

// ---------------------------------------------------------------------------
// Focus pane
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Hosts,
    Tunnels,
    Logs,
}

// ---------------------------------------------------------------------------
// Input mode (for filter / modal sheets)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    /// Normal navigation.
    Normal,
    /// User is typing in the filter bar.
    Filter,
    /// A modal sheet is active.
    Sheet(SheetKind),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SheetKind {
    AddHost,
    NewTunnel,
    NodePicker,
}

// ---------------------------------------------------------------------------
// Application model
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct AppModel {
    pub hosts: Vec<Host>,
    pub tunnels: Vec<Tunnel>,

    /// Currently focused pane.
    pub focus: Pane,

    /// Selected row index into the visible list for each pane.
    pub hosts_sel: usize,
    pub tunnels_sel: usize,

    /// Active substring filter (applies to the focused pane).
    pub filter: String,

    /// Log lines received from the daemon.
    pub log_lines: Vec<String>,

    /// Status bar message (transient, overwritten on next action).
    pub status_msg: String,

    /// Current input mode.
    pub input_mode: InputMode,

    /// Buffer for the filter input field.
    pub filter_buf: String,

    /// Signal to exit the event loop.
    pub should_quit: bool,
}

impl Default for AppModel {
    fn default() -> Self {
        Self {
            hosts: Vec::new(),
            tunnels: Vec::new(),
            focus: Pane::Tunnels,
            hosts_sel: 0,
            tunnels_sel: 0,
            filter: String::new(),
            log_lines: Vec::new(),
            status_msg: String::new(),
            input_mode: InputMode::Normal,
            filter_buf: String::new(),
            should_quit: false,
        }
    }
}

impl AppModel {
    pub fn new() -> Self {
        Self::default()
    }

    // -----------------------------------------------------------------------
    // Test helpers (also available via feature flag)
    // -----------------------------------------------------------------------

    /// Construct an `AppModel` pre-populated with tunnels (for unit tests).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn with_tunnels(tunnels: Vec<Tunnel>) -> Self {
        Self {
            tunnels,
            ..Self::default()
        }
    }

    // -----------------------------------------------------------------------
    // Visibility / filter helpers
    // -----------------------------------------------------------------------

    /// Return all tunnels whose name, last_node, active_jump, or tags contain
    /// `filter` (case-insensitive substring).  Empty `filter` returns all tunnels.
    pub fn visible_tunnels<'a>(&'a self, filter: &str) -> Vec<&'a Tunnel> {
        let q = filter.to_lowercase();
        self.tunnels
            .iter()
            .filter(|t| {
                if q.is_empty() {
                    return true;
                }
                if t.name.to_lowercase().contains(&q) {
                    return true;
                }
                if let Some(n) = &t.last_node {
                    if n.to_lowercase().contains(&q) {
                        return true;
                    }
                }
                if let Some(j) = &t.active_jump {
                    if j.to_lowercase().contains(&q) {
                        return true;
                    }
                }
                if t.tags.iter().any(|tag| tag.to_lowercase().contains(&q)) {
                    return true;
                }
                false
            })
            .collect()
    }

    /// Return all hosts whose name or last_msg contains `filter`.
    pub fn visible_hosts<'a>(&'a self, filter: &str) -> Vec<&'a Host> {
        let q = filter.to_lowercase();
        self.hosts
            .iter()
            .filter(|h| {
                if q.is_empty() {
                    return true;
                }
                h.host.to_lowercase().contains(&q) || h.last_msg.to_lowercase().contains(&q)
            })
            .collect()
    }

    // -----------------------------------------------------------------------
    // Selection helpers
    // -----------------------------------------------------------------------

    pub fn selected_tunnel(&self) -> Option<&Tunnel> {
        let visible = self.visible_tunnels(&self.filter);
        visible.get(self.tunnels_sel).copied()
    }

    pub fn selected_host(&self) -> Option<&Host> {
        let visible = self.visible_hosts(&self.filter);
        visible.get(self.hosts_sel).copied()
    }

    // -----------------------------------------------------------------------
    // Reducer: key-driven state transitions (pure)
    // -----------------------------------------------------------------------

    /// Move selection down within the focused pane.
    pub fn move_down(&mut self) {
        match self.focus {
            Pane::Hosts => {
                let len = self.visible_hosts(&self.filter).len();
                if len > 0 {
                    self.hosts_sel = (self.hosts_sel + 1).min(len - 1);
                }
            }
            Pane::Tunnels => {
                let len = self.visible_tunnels(&self.filter).len();
                if len > 0 {
                    self.tunnels_sel = (self.tunnels_sel + 1).min(len - 1);
                }
            }
            Pane::Logs => {}
        }
    }

    /// Move selection up within the focused pane.
    pub fn move_up(&mut self) {
        match self.focus {
            Pane::Hosts => {
                if self.hosts_sel > 0 {
                    self.hosts_sel -= 1;
                }
            }
            Pane::Tunnels => {
                if self.tunnels_sel > 0 {
                    self.tunnels_sel -= 1;
                }
            }
            Pane::Logs => {}
        }
    }

    /// Cycle focus: Hosts ↔ Tunnels (logs is entered/exited explicitly).
    pub fn cycle_focus(&mut self) {
        self.focus = match self.focus {
            Pane::Hosts => Pane::Tunnels,
            Pane::Tunnels => Pane::Hosts,
            Pane::Logs => Pane::Tunnels,
        };
    }

    /// Commit the filter buffer as the active filter and return to Normal mode.
    pub fn commit_filter(&mut self) {
        self.filter = std::mem::take(&mut self.filter_buf);
        self.input_mode = InputMode::Normal;
        // Clamp selections after filtering.
        let ht_len = self.visible_hosts(&self.filter).len();
        if ht_len > 0 {
            self.hosts_sel = self.hosts_sel.min(ht_len - 1);
        } else {
            self.hosts_sel = 0;
        }
        let tn_len = self.visible_tunnels(&self.filter).len();
        if tn_len > 0 {
            self.tunnels_sel = self.tunnels_sel.min(tn_len - 1);
        } else {
            self.tunnels_sel = 0;
        }
    }

    /// Clear the active filter and return to Normal mode.
    pub fn clear_filter(&mut self) {
        self.filter.clear();
        self.filter_buf.clear();
        self.input_mode = InputMode::Normal;
    }

    /// Replace the tunnel list with a fresh snapshot from the daemon.
    pub fn set_tunnels(&mut self, tunnels: Vec<Tunnel>) {
        self.tunnels = tunnels;
        let new_vis = self.visible_tunnels(&self.filter).len();
        if new_vis > 0 && self.tunnels_sel >= new_vis {
            self.tunnels_sel = new_vis - 1;
        } else if new_vis == 0 {
            self.tunnels_sel = 0;
        }
    }

    /// Replace the host list with a fresh snapshot from the daemon.
    pub fn set_hosts(&mut self, hosts: Vec<Host>) {
        self.hosts = hosts;
        let new_vis = self.visible_hosts(&self.filter).len();
        if new_vis > 0 && self.hosts_sel >= new_vis {
            self.hosts_sel = new_vis - 1;
        } else if new_vis == 0 {
            self.hosts_sel = 0;
        }
    }

    /// Append log lines (used by the log_tail response and event stream).
    pub fn append_logs(&mut self, lines: Vec<String>) {
        self.log_lines.extend(lines);
        // Keep at most 2000 lines to avoid unbounded memory growth.
        if self.log_lines.len() > 2000 {
            let overflow = self.log_lines.len() - 2000;
            self.log_lines.drain(0..overflow);
        }
    }
}

// ---------------------------------------------------------------------------
// Color helper (pure — no I/O)
// ---------------------------------------------------------------------------

/// Map a daemon status string to a ratatui `Color`.
///
/// Status strings from `TunnelStatus`:
///   "alive"               → Green
///   "starting"            → Yellow
///   "failed"/"port_busy"/"stale" → Red
///   "idle" / anything else → Gray
pub fn status_color(status: &str) -> Color {
    match status {
        "alive" => Color::Green,
        "starting" => Color::Yellow,
        "failed" | "port_busy" | "stale" => Color::Red,
        _ => Color::Gray, // "idle", unknown
    }
}

/// Same mapping applied to a `TunnelStatus` enum value.
#[allow(dead_code)]
pub fn tunnel_status_color(status: TunnelStatus) -> Color {
    status_color(&status.to_string())
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: build a minimal Tunnel for testing.
    fn tunnel(name: &str, node: &str) -> Tunnel {
        Tunnel {
            name: name.to_string(),
            local_port: 8888,
            remote_port: 8888,
            jump_candidates: None,
            last_node: Some(node.to_string()),
            last_user: None,
            auto_start: false,
            post_connect_cmd: None,
            tags: vec![],
            url_path: None,
            wants_alive: false,
            status: TunnelStatus::Idle,
            active_jump: None,
            last_msg: String::new(),
            last_alive_at: 0.0,
            total_uptime_sec: 0.0,
            connect_count: 0,
            fail_count: 0,
        }
    }

    #[test]
    fn tunnel_status_color_mapping() {
        assert_eq!(status_color("alive"), Color::Green);
        assert_eq!(status_color("starting"), Color::Yellow);
        assert_eq!(status_color("failed"), Color::Red);
        assert_eq!(status_color("idle"), Color::Gray);
    }

    #[test]
    fn filter_matches_name_and_node() {
        let app = AppModel::with_tunnels(vec![
            tunnel("jupyter", "holygpu01"),
            tunnel("web", "node2"),
        ]);
        let v = app.visible_tunnels("jup");
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].name, "jupyter");
    }

    #[test]
    fn empty_filter_shows_all() {
        let app = AppModel::with_tunnels(vec![tunnel("a", "n"), tunnel("b", "n")]);
        assert_eq!(app.visible_tunnels("").len(), 2);
    }

    #[test]
    fn filter_matches_node() {
        let app = AppModel::with_tunnels(vec![
            tunnel("jupyter", "holygpu01"),
            tunnel("web", "node2"),
        ]);
        let v = app.visible_tunnels("holygpu");
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].name, "jupyter");
    }

    #[test]
    fn port_busy_maps_to_red() {
        assert_eq!(status_color("port_busy"), Color::Red);
    }

    #[test]
    fn stale_maps_to_red() {
        assert_eq!(status_color("stale"), Color::Red);
    }

    #[test]
    fn move_down_clamps_at_end() {
        let mut app = AppModel::with_tunnels(vec![tunnel("a", "n"), tunnel("b", "n")]);
        app.focus = Pane::Tunnels;
        app.move_down();
        app.move_down();
        app.move_down(); // should stay at 1
        assert_eq!(app.tunnels_sel, 1);
    }

    #[test]
    fn move_up_clamps_at_zero() {
        let mut app = AppModel::with_tunnels(vec![tunnel("a", "n")]);
        app.focus = Pane::Tunnels;
        app.move_up();
        assert_eq!(app.tunnels_sel, 0);
    }

    #[test]
    fn commit_filter_clamps_selection() {
        let mut app = AppModel::with_tunnels(vec![
            tunnel("alpha", "n"),
            tunnel("beta", "n"),
            tunnel("gamma", "n"),
        ]);
        app.focus = Pane::Tunnels;
        app.tunnels_sel = 2;
        app.filter_buf = "alp".to_string();
        app.commit_filter();
        // After filtering to 1 item, selection must be clamped to 0.
        assert_eq!(app.tunnels_sel, 0);
    }
}
