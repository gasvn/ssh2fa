//! Pure application model (no I/O).
//!
//! All UI state lives here; `main.rs` feeds it events and the view modules
//! read it for rendering.  Keeping this I/O-free makes it directly testable.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

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
    ConfirmDelete,
    Help,
}

// ---------------------------------------------------------------------------
// Toast / transient notification
// ---------------------------------------------------------------------------

/// Severity of a transient toast — drives its color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Info,
    Warning,
    Error,
}

impl Severity {
    pub fn color(self) -> Color {
        match self {
            Severity::Info => Color::Green,
            Severity::Warning => Color::Yellow,
            Severity::Error => Color::Red,
        }
    }
}

/// A transient toast shown in the status line, auto-clearing after a while.
#[derive(Debug, Clone)]
pub struct Toast {
    pub text: String,
    pub severity: Severity,
    pub created: Instant,
}

/// How long a toast remains visible before auto-clearing.
pub const TOAST_TTL: Duration = Duration::from_secs(6);

/// Build the URL for a tunnel's local endpoint.
///
/// Always `http://localhost:<port>`, with `url_path` appended when present.
/// A missing leading '/' on `url_path` is added so the result is well-formed.
pub fn tunnel_url(local_port: u16, url_path: Option<&str>) -> String {
    let base = format!("http://localhost:{local_port}");
    match url_path {
        Some(p) if !p.is_empty() => {
            if p.starts_with('/') {
                format!("{base}{p}")
            } else {
                format!("{base}/{p}")
            }
        }
        _ => base,
    }
}

/// Decide the toast (and whether to fire a native notification) for a single
/// tunnel status transition. Pure mirror of Python `_notify_status_transitions`.
///
/// Returns `None` when the transition is uninteresting (no change, or an
/// unhandled state). The bool in the tuple is `fire_native`.
pub fn status_transition_toast(
    prev: Option<&str>,
    new: &str,
    last_msg: &str,
    user_stopped: bool,
) -> Option<(String, Severity, bool)> {
    if prev == Some(new) {
        return None;
    }
    match (prev, new) {
        (Some("alive"), "stale") => Some((
            format!("⚠ {NAME}: compute node ended — Enter to repick"),
            Severity::Warning,
            true,
        )),
        (Some("alive"), "failed") => Some((
            format!("✕ {NAME} disconnected: {last_msg}"),
            Severity::Error,
            true,
        )),
        (Some("alive"), "idle") => {
            if user_stopped {
                Some((format!("⊘ {NAME} stopped"), Severity::Info, false))
            } else {
                Some((
                    format!("⚠ {NAME}: connection dropped — reconnecting"),
                    Severity::Warning,
                    true,
                ))
            }
        }
        (Some("alive"), "starting") => Some((
            format!("↻ {NAME}: reconnecting…"),
            Severity::Warning,
            false,
        )),
        // Transitions into a bad state with no alive-prev to compare.
        (_, "failed") => {
            let m = if last_msg.is_empty() {
                "failed to connect".to_string()
            } else {
                last_msg.to_string()
            };
            Some((format!("✕ {NAME}: {m}"), Severity::Error, true))
        }
        (_, "port_busy") => {
            Some((format!("✕ {NAME}: port in use"), Severity::Error, false))
        }
        (_, "stale") => Some((
            format!("⚠ {NAME}: node not running — Enter to repick"),
            Severity::Warning,
            false,
        )),
        (Some("starting"), "alive")
        | (Some("idle"), "alive")
        | (Some("stale"), "alive")
        | (Some("failed"), "alive")
        | (Some("port_busy"), "alive") => {
            Some((format!("✓ {NAME} connected"), Severity::Info, false))
        }
        _ => None,
    }
}

/// Placeholder token replaced with the tunnel name by the caller. Keeping the
/// decision fn name-agnostic makes it trivially unit-testable.
const NAME: &str = "{name}";

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

    /// Active transient toast (auto-clears after `TOAST_TTL`).
    pub toast: Option<Toast>,

    /// Last-seen status per tunnel, for transition detection.
    pub last_seen_status: HashMap<String, String>,

    /// Tunnels the user just intentionally stopped (so an alive→idle is quiet).
    pub user_stopped: HashSet<String>,
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
            toast: None,
            last_seen_status: HashMap::new(),
            user_stopped: HashSet::new(),
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
    #[allow(dead_code)]
    pub fn clear_filter(&mut self) {
        self.filter.clear();
        self.filter_buf.clear();
        self.input_mode = InputMode::Normal;
    }

    /// Cancel filter ENTRY: discard the in-progress buffer and return to
    /// Normal mode, keeping the previously-active filter. Esc must abort —
    /// committing a junk partial filter could hide every row.
    pub fn cancel_filter(&mut self) {
        self.filter_buf.clear();
        self.input_mode = InputMode::Normal;
    }

    /// Replace the tunnel list with a fresh snapshot from the daemon.
    ///
    /// Detects per-tunnel status transitions vs. the last snapshot, emits the
    /// in-app toast for the most important change, and returns the list of
    /// native notifications to fire (title, message). The caller (main.rs) is
    /// responsible for actually firing them — this keeps `AppModel` I/O-free.
    pub fn set_tunnels(&mut self, tunnels: Vec<Tunnel>) -> Vec<(String, String)> {
        let mut native: Vec<(String, String)> = Vec::new();
        let mut present: HashSet<String> = HashSet::new();

        for t in &tunnels {
            let name = t.name.clone();
            present.insert(name.clone());
            let new_status = t.status.to_string();
            let prev = self.last_seen_status.get(&name).cloned();
            self.last_seen_status.insert(name.clone(), new_status.clone());

            let user_stopped = self.user_stopped.contains(&name);
            if let Some((text, severity, fire_native)) = status_transition_toast(
                prev.as_deref(),
                &new_status,
                &t.last_msg,
                user_stopped,
            ) {
                let text = text.replace("{name}", &name);
                // Intentional-stop is consumed once it produces its quiet toast.
                if prev.as_deref() == Some("alive") && new_status == "idle" {
                    self.user_stopped.remove(&name);
                }
                if fire_native {
                    let title = match severity {
                        Severity::Error => "Auto2FA: tunnel failed",
                        Severity::Warning => "Auto2FA: tunnel issue",
                        Severity::Info => "Auto2FA",
                    };
                    native.push((title.to_string(), text.clone()));
                }
                self.push_toast(text, severity);
            }
        }

        // Drop last-seen entries for tunnels that no longer exist.
        self.last_seen_status.retain(|k, _| present.contains(k));
        self.user_stopped.retain(|k| present.contains(k));

        self.tunnels = tunnels;
        let new_vis = self.visible_tunnels(&self.filter).len();
        if new_vis > 0 && self.tunnels_sel >= new_vis {
            self.tunnels_sel = new_vis - 1;
        } else if new_vis == 0 {
            self.tunnels_sel = 0;
        }
        native
    }

    /// Show a transient toast (also mirrored into the status line).
    pub fn push_toast(&mut self, text: impl Into<String>, severity: Severity) {
        let text = text.into();
        self.status_msg = text.clone();
        self.toast = Some(Toast {
            text,
            severity,
            created: Instant::now(),
        });
    }

    /// Clear the toast if it has outlived `TOAST_TTL`. Returns true if cleared.
    pub fn expire_toast(&mut self) -> bool {
        if let Some(t) = &self.toast {
            if t.created.elapsed() >= TOAST_TTL {
                self.toast = None;
                return true;
            }
        }
        false
    }

    /// Mark a tunnel as intentionally stopped by the user (so the resulting
    /// alive→idle transition produces a quiet toast, not a warning).
    pub fn mark_user_stopped(&mut self, name: &str) {
        self.user_stopped.insert(name.to_string());
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

    /// Replace the log buffer with a fresh `log_tail` snapshot (the daemon
    /// has no log event stream — the view re-fetches on entry instead).
    pub fn set_logs(&mut self, lines: Vec<String>) {
        self.log_lines = lines;
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
            direct_host: None,
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

    // -------------------------------------------------------------------
    // tunnel_url
    // -------------------------------------------------------------------

    #[test]
    fn tunnel_url_no_path() {
        assert_eq!(tunnel_url(8888, None), "http://localhost:8888");
        assert_eq!(tunnel_url(8888, Some("")), "http://localhost:8888");
    }

    #[test]
    fn tunnel_url_with_leading_slash() {
        assert_eq!(tunnel_url(8888, Some("/lab")), "http://localhost:8888/lab");
    }

    #[test]
    fn tunnel_url_without_leading_slash() {
        assert_eq!(tunnel_url(8888, Some("lab")), "http://localhost:8888/lab");
    }

    // -------------------------------------------------------------------
    // status_transition_toast
    // -------------------------------------------------------------------

    #[test]
    fn transition_same_status_is_none() {
        assert!(status_transition_toast(Some("alive"), "alive", "", false).is_none());
    }

    #[test]
    fn transition_alive_to_failed_fires_native() {
        let (text, sev, native) =
            status_transition_toast(Some("alive"), "failed", "boom", false).unwrap();
        assert!(text.contains("disconnected"));
        assert!(text.contains("boom"));
        assert_eq!(sev, Severity::Error);
        assert!(native);
    }

    #[test]
    fn transition_alive_to_stale_fires_native() {
        let (_, sev, native) =
            status_transition_toast(Some("alive"), "stale", "", false).unwrap();
        assert_eq!(sev, Severity::Warning);
        assert!(native);
    }

    #[test]
    fn transition_user_stopped_alive_to_idle_is_quiet() {
        let (text, sev, native) =
            status_transition_toast(Some("alive"), "idle", "", true).unwrap();
        assert!(text.contains("stopped"));
        assert_eq!(sev, Severity::Info);
        assert!(!native);
    }

    #[test]
    fn transition_unexpected_alive_to_idle_warns_and_notifies() {
        let (text, sev, native) =
            status_transition_toast(Some("alive"), "idle", "", false).unwrap();
        assert!(text.contains("dropped"));
        assert_eq!(sev, Severity::Warning);
        assert!(native);
    }

    #[test]
    fn transition_starting_to_alive_is_quiet_no_native() {
        let (text, sev, native) =
            status_transition_toast(Some("starting"), "alive", "", false).unwrap();
        assert!(text.contains("connected"));
        assert_eq!(sev, Severity::Info);
        assert!(!native);
    }

    #[test]
    fn transition_alive_to_starting_quiet_warning() {
        let (text, sev, native) =
            status_transition_toast(Some("alive"), "starting", "", false).unwrap();
        assert!(text.contains("reconnecting"));
        assert_eq!(sev, Severity::Warning);
        assert!(!native);
    }

    #[test]
    fn transition_initial_failed_fires_native() {
        let (text, sev, native) =
            status_transition_toast(None, "failed", "", false).unwrap();
        assert!(text.contains("failed to connect"));
        assert_eq!(sev, Severity::Error);
        assert!(native);
    }

    #[test]
    fn transition_port_busy_no_native() {
        let (text, sev, native) =
            status_transition_toast(Some("idle"), "port_busy", "", false).unwrap();
        assert!(text.contains("port in use"));
        assert_eq!(sev, Severity::Error);
        assert!(!native);
    }

    #[test]
    fn set_tunnels_emits_native_and_clears_removed() {
        let mut app = AppModel::default();
        // Seed an alive tunnel.
        let mut t = tunnel("nb", "n1");
        t.status = TunnelStatus::Alive;
        let natives = app.set_tunnels(vec![t.clone()]);
        // First sight: no prev → alive is not a flagged transition.
        assert!(natives.is_empty());
        // Now it fails — should produce a native notification.
        t.status = TunnelStatus::Failed;
        t.last_msg = "lost".into();
        let natives = app.set_tunnels(vec![t.clone()]);
        assert_eq!(natives.len(), 1);
        assert!(natives[0].1.contains("nb"));
        // Removing the tunnel drops its last_seen entry.
        app.set_tunnels(vec![]);
        assert!(app.last_seen_status.is_empty());
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

    /// Esc during filter entry must DISCARD the buffer and keep the
    /// previously-active filter (it used to commit, hiding every row on a
    /// junk partial filter).
    #[test]
    fn cancel_filter_discards_buffer_keeps_active_filter() {
        let mut app = AppModel::with_tunnels(vec![tunnel("alpha", "n")]);
        app.filter = "alp".to_string(); // previously committed filter
        app.input_mode = InputMode::Filter;
        app.filter_buf = "zzz-junk".to_string(); // in-progress junk
        app.cancel_filter();
        assert_eq!(app.filter, "alp", "active filter must survive a cancel");
        assert!(app.filter_buf.is_empty(), "buffer must be discarded");
        assert!(matches!(app.input_mode, InputMode::Normal));
    }

    /// set_logs REPLACES the buffer (re-fetch on entering the logs view —
    /// append would duplicate the overlap) and trims to the 2000-line cap.
    #[test]
    fn set_logs_replaces_and_trims() {
        let mut app = AppModel::with_tunnels(vec![]);
        app.set_logs(vec!["old1".into(), "old2".into()]);
        app.set_logs(vec!["new1".into()]);
        assert_eq!(app.log_lines, vec!["new1".to_string()], "must replace, not append");

        let many: Vec<String> = (0..2500).map(|i| format!("line{i}")).collect();
        app.set_logs(many);
        assert_eq!(app.log_lines.len(), 2000, "must trim to the cap");
        assert_eq!(app.log_lines[0], "line500", "must keep the NEWEST lines");
    }
}
