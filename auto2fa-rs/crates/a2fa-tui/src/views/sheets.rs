//! Modal input sheets: add-host, new-tunnel, node-picker.
//!
//! Each sheet renders a centered overlay and exposes an in-progress input
//! buffer that `main.rs` fills from key events.

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

// ---------------------------------------------------------------------------
// Generic helpers
// ---------------------------------------------------------------------------

/// Return a centered rect with the given percentage width and fixed height.
pub fn centered_rect(percent_x: u16, height: u16, r: Rect) -> Rect {
    let vert = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length((r.height.saturating_sub(height)) / 2),
            Constraint::Length(height),
            Constraint::Min(0),
        ])
        .split(r);

    let horiz = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vert[1]);

    horiz[1]
}

fn render_input_field(
    f: &mut Frame,
    area: Rect,
    label: &str,
    value: &str,
    focused: bool,
) {
    let border_style = if focused {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let display = format!("{}: {}", label, value);
    let para = Paragraph::new(display)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border_style),
        );
    f.render_widget(para, area);
}

// ---------------------------------------------------------------------------
// Add-host sheet
// ---------------------------------------------------------------------------

/// State for the add-host modal.
#[derive(Debug, Clone, Default)]
pub struct AddHostSheet {
    /// The host alias being entered.
    pub host_buf: String,
    /// Which field is focused (0 = host).
    #[allow(dead_code)]
    pub field: usize,
    /// Optional error to display.
    pub error: String,
}

impl AddHostSheet {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Render the add-host modal.
pub fn render_add_host(f: &mut Frame, sheet: &AddHostSheet) {
    let area = centered_rect(60, 10, f.area());
    f.render_widget(Clear, area);

    let block = Block::default()
        .title("Add Host")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD));
    f.render_widget(block, area);

    let inner = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // host field
            Constraint::Length(1), // error line
            Constraint::Length(1), // hint
        ])
        .split(inner);

    render_input_field(f, chunks[0], "Host alias", &sheet.host_buf, true);

    if !sheet.error.is_empty() {
        let err = Paragraph::new(sheet.error.as_str())
            .style(Style::default().fg(Color::Red));
        f.render_widget(err, chunks[1]);
    }

    let hint = Paragraph::new("Enter: confirm   Esc: cancel")
        .style(Style::default().fg(Color::DarkGray))
        .alignment(Alignment::Center);
    f.render_widget(hint, chunks[2]);
}

// ---------------------------------------------------------------------------
// New-tunnel sheet
// ---------------------------------------------------------------------------

/// State for the new-tunnel modal.
#[derive(Debug, Clone, Default)]
pub struct NewTunnelSheet {
    pub name_buf: String,
    pub port_buf: String,
    /// 0 = name field, 1 = port field.
    pub field: usize,
    pub error: String,
}

impl NewTunnelSheet {
    pub fn new() -> Self {
        Self::default()
    }

    /// Try to parse and return `(name, port)`.  Sets `self.error` on failure.
    pub fn validate(&mut self) -> Option<(String, u16)> {
        let name = self.name_buf.trim().to_string();
        if name.is_empty() {
            self.error = "Name cannot be empty.".to_string();
            self.field = 0;
            return None;
        }
        let port_str = self.port_buf.trim().to_string();
        if port_str.is_empty() {
            self.error = "Port cannot be empty.".to_string();
            self.field = 1;
            return None;
        }
        match port_str.parse::<u16>() {
            Ok(p) if p >= 1024 => Some((name, p)),
            Ok(_) => {
                self.error = "Port must be ≥ 1024.".to_string();
                self.field = 1;
                None
            }
            Err(_) => {
                self.error = "Port must be a number.".to_string();
                self.field = 1;
                None
            }
        }
    }
}

/// Render the new-tunnel modal.
pub fn render_new_tunnel(f: &mut Frame, sheet: &NewTunnelSheet) {
    let area = centered_rect(60, 12, f.area());
    f.render_widget(Clear, area);

    let block = Block::default()
        .title("New Tunnel")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD));
    f.render_widget(block, area);

    let inner = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // name field
            Constraint::Length(3), // port field
            Constraint::Length(1), // error
            Constraint::Length(1), // hint
        ])
        .split(inner);

    render_input_field(f, chunks[0], "Name", &sheet.name_buf, sheet.field == 0);
    render_input_field(f, chunks[1], "Local port", &sheet.port_buf, sheet.field == 1);

    if !sheet.error.is_empty() {
        let err = Paragraph::new(sheet.error.as_str())
            .style(Style::default().fg(Color::Red));
        f.render_widget(err, chunks[2]);
    }

    let hint = Paragraph::new("Tab: next field   Enter: confirm   Esc: cancel")
        .style(Style::default().fg(Color::DarkGray))
        .alignment(Alignment::Center);
    f.render_widget(hint, chunks[3]);
}

// ---------------------------------------------------------------------------
// Node-picker sheet (squeue-backed)
// ---------------------------------------------------------------------------

/// A single SLURM job row as returned by `discover_nodes`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SqueueJob {
    pub jobid: String,
    pub partition: String,
    pub name: String,
    pub state: String,
    pub time: String,
    pub node: String,
}

/// State for the squeue-backed node-picker modal.
///
/// Two display modes share one struct:
///   * list mode (`custom == false`): a scrollable list of RUNNING squeue jobs.
///   * custom mode (`custom == true`): a free-text field for a manual node name.
#[derive(Debug, Clone, Default)]
pub struct NodePickerSheet {
    /// The tunnel name this picker is for.
    pub tunnel_name: String,
    /// The jump host used for discovery (None → no ready jump host).
    pub jump: Option<String>,
    /// The user to pass to `tunnel_set_node`.
    pub user: String,
    /// RUNNING jobs to pick from.
    pub jobs: Vec<SqueueJob>,
    /// Selected row index into `jobs`.
    pub sel: usize,
    /// Custom-entry text buffer (used when `custom` is true).
    pub node_buf: String,
    /// Whether the modal is in custom free-text entry mode.
    pub custom: bool,
    /// Status / error / hint message line.
    pub error: String,
}

impl NodePickerSheet {
    pub fn new(tunnel_name: &str, jump: Option<String>, user: String) -> Self {
        Self {
            tunnel_name: tunnel_name.to_string(),
            jump,
            user,
            ..Self::default()
        }
    }

    /// Replace the job list with a fresh squeue result, keeping only RUNNING
    /// jobs, and clamp the selection.  Pre-selects `preselect_node` if present.
    pub fn set_jobs(&mut self, jobs: Vec<SqueueJob>, preselect_node: Option<&str>) {
        self.jobs = filter_running(jobs);
        if self.jobs.is_empty() {
            self.sel = 0;
            self.error = "no running jobs — press c for custom, r to retry".to_string();
            return;
        }
        self.sel = preselect_node
            .and_then(|n| self.jobs.iter().position(|j| j.node == n))
            .unwrap_or(0);
        self.error.clear();
    }

    /// Move the list selection down (clamped at the last row).
    pub fn move_down(&mut self) {
        if !self.jobs.is_empty() {
            self.sel = (self.sel + 1).min(self.jobs.len() - 1);
        }
    }

    /// Move the list selection up (clamped at row 0).
    pub fn move_up(&mut self) {
        if self.sel > 0 {
            self.sel -= 1;
        }
    }

    /// The currently-selected job's node string, if any.
    pub fn selected_node(&self) -> Option<String> {
        self.jobs.get(self.sel).map(|j| j.node.clone())
    }

    /// Switch into custom free-text entry mode.
    pub fn enter_custom(&mut self) {
        self.custom = true;
        self.node_buf.clear();
        self.error.clear();
    }

    /// Resolve the node to submit, depending on the current mode.
    ///
    /// Returns `None` (and sets `error`) when there is nothing valid to submit.
    pub fn resolve_node(&mut self) -> Option<String> {
        if self.custom {
            let node = self.node_buf.trim().to_string();
            if node.is_empty() {
                self.error = "Node cannot be empty.".to_string();
                return None;
            }
            Some(node)
        } else {
            match self.selected_node() {
                Some(n) => Some(n),
                None => {
                    self.error =
                        "no running jobs — press c for custom, r to retry".to_string();
                    None
                }
            }
        }
    }
}

/// Keep only jobs whose state is RUNNING (case-insensitive, also accepts the
/// short SLURM code "R").
pub fn filter_running(jobs: Vec<SqueueJob>) -> Vec<SqueueJob> {
    jobs.into_iter()
        .filter(|j| {
            let s = j.state.to_ascii_uppercase();
            s == "RUNNING" || s == "R"
        })
        .collect()
}

/// Render the node-picker modal.
pub fn render_node_picker(f: &mut Frame, sheet: &NodePickerSheet) {
    let via = sheet
        .jump
        .as_deref()
        .map(|j| format!(" via {j}"))
        .unwrap_or_default();
    let title = format!("Pick node for '{}'{}", sheet.tunnel_name, via);
    let area = centered_rect(70, 18, f.area());
    f.render_widget(Clear, area);

    let block = Block::default()
        .title(title.as_str())
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD));
    f.render_widget(block, area);

    let inner = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),    // list or custom field
            Constraint::Length(1), // error / status
            Constraint::Length(1), // hint
        ])
        .split(inner);

    if sheet.custom {
        render_input_field(f, chunks[0], "Custom node", &sheet.node_buf, true);
    } else {
        let mut lines: Vec<Line> = Vec::new();
        if sheet.jobs.is_empty() {
            lines.push(Line::from(Span::styled(
                "(no running jobs)",
                Style::default().fg(Color::DarkGray),
            )));
        } else {
            for (i, j) in sheet.jobs.iter().enumerate() {
                let text = format!(
                    "{:<10} {:<12} {:<16} {:<10} {}",
                    truncate(&j.jobid, 10),
                    truncate(&j.partition, 12),
                    truncate(&j.name, 16),
                    truncate(&j.time, 10),
                    j.node,
                );
                let style = if i == sheet.sel {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Yellow)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                lines.push(Line::from(Span::styled(text, style)));
            }
        }
        let list = Paragraph::new(lines);
        f.render_widget(list, chunks[0]);
    }

    if !sheet.error.is_empty() {
        let err = Paragraph::new(sheet.error.as_str()).style(Style::default().fg(Color::Red));
        f.render_widget(err, chunks[1]);
    }

    let hint = if sheet.custom {
        "Enter: confirm   Esc: cancel"
    } else {
        "↑↓/jk: move   Enter: use   c: custom   r: refresh   Esc: cancel"
    };
    let hint_w = Paragraph::new(hint)
        .style(Style::default().fg(Color::DarkGray))
        .alignment(Alignment::Center);
    f.render_widget(hint_w, chunks[2]);
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
    }
}

// ---------------------------------------------------------------------------
// Confirm-delete modal
// ---------------------------------------------------------------------------

/// State for the delete-tunnel confirm modal.
#[derive(Debug, Clone, Default)]
pub struct ConfirmDeleteSheet {
    /// The tunnel name to delete.
    pub name: String,
}

impl ConfirmDeleteSheet {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
        }
    }
}

/// Render the confirm-delete modal.
pub fn render_confirm_delete(f: &mut Frame, sheet: &ConfirmDeleteSheet) {
    let area = centered_rect(60, 7, f.area());
    f.render_widget(Clear, area);

    let block = Block::default()
        .title("Confirm")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD));
    f.render_widget(block, area);

    let inner = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // spacer
            Constraint::Length(1), // question
            Constraint::Length(1), // spacer
            Constraint::Length(1), // hint
        ])
        .split(inner);

    let q = Paragraph::new(format!("Delete tunnel '{}'?", sheet.name))
        .alignment(Alignment::Center);
    f.render_widget(q, chunks[1]);

    let hint = Paragraph::new("y: yes    n / Esc / q: cancel")
        .style(Style::default().fg(Color::DarkGray))
        .alignment(Alignment::Center);
    f.render_widget(hint, chunks[3]);
}

// ---------------------------------------------------------------------------
// Help modal
// ---------------------------------------------------------------------------

/// Keybinding reference lines, grouped. Each entry is `(key, description)`;
/// a `("", "Section")` entry with an empty key is rendered as a section header.
pub fn help_lines() -> Vec<(&'static str, &'static str)> {
    vec![
        ("", "Global"),
        ("q", "Quit"),
        ("t / Ctrl+n", "New tunnel"),
        ("?", "Show this help"),
        ("Tab", "Switch between Hosts and Tunnels"),
        ("/", "Filter the focused pane"),
        ("l", "Toggle the logs view"),
        ("j/k  ↑/↓", "Move cursor"),
        ("", "Tunnels"),
        ("Space", "Start / stop the selected tunnel"),
        ("Enter", "Pick a compute node"),
        ("y", "Copy URL to clipboard"),
        ("d", "Delete the selected tunnel"),
        ("s / x", "Start / stop (explicit aliases)"),
        ("", "Hosts"),
        ("Space", "Start / stop the selected host"),
        ("m", "Mount / unmount remote filesystem"),
        ("r", "Rotate connection pool"),
    ]
}

/// Render the help modal listing all keybindings.
pub fn render_help(f: &mut Frame) {
    let rows = help_lines();
    // border (2) + title (1) + blank (1) + hint (1) + a little slack.
    let height = (rows.len() as u16) + 6;
    let area = centered_rect(64, height.min(f.area().height), f.area());
    f.render_widget(Clear, area);

    let block = Block::default()
        .title("Keyboard Reference")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD));
    f.render_widget(block, area);

    let inner = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };

    let mut lines: Vec<Line> = Vec::new();
    for (key, desc) in rows {
        if key.is_empty() {
            lines.push(Line::from(Span::styled(
                desc.to_string(),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            )));
        } else {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {key:<12}"),
                    Style::default().fg(Color::Yellow),
                ),
                Span::raw(desc.to_string()),
            ]));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "?, Esc or q to close",
        Style::default().fg(Color::DarkGray),
    )));

    let para = Paragraph::new(lines);
    f.render_widget(para, inner);
}

// ---------------------------------------------------------------------------
// Tests (pure logic)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn job(jobid: &str, node: &str, state: &str) -> SqueueJob {
        SqueueJob {
            jobid: jobid.into(),
            partition: "gpu".into(),
            name: "run".into(),
            state: state.into(),
            time: "1:00:00".into(),
            node: node.into(),
        }
    }

    #[test]
    fn filter_running_keeps_only_running() {
        let jobs = vec![
            job("1", "n1", "RUNNING"),
            job("2", "n2", "PENDING"),
            job("3", "n3", "R"),
            job("4", "n4", "completing"),
        ];
        let kept = filter_running(jobs);
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0].node, "n1");
        assert_eq!(kept[1].node, "n3");
    }

    #[test]
    fn set_jobs_filters_and_preselects() {
        let mut sh = NodePickerSheet::new("nb", Some("k6".into()), "jdoe".into());
        let jobs = vec![
            job("1", "n1", "RUNNING"),
            job("2", "n2", "PENDING"),
            job("3", "n3", "RUNNING"),
        ];
        sh.set_jobs(jobs, Some("n3"));
        assert_eq!(sh.jobs.len(), 2);
        assert_eq!(sh.sel, 1);
        assert_eq!(sh.selected_node().as_deref(), Some("n3"));
        assert!(sh.error.is_empty());
    }

    #[test]
    fn set_jobs_empty_sets_message() {
        let mut sh = NodePickerSheet::new("nb", Some("k6".into()), "jdoe".into());
        sh.set_jobs(vec![job("1", "n1", "PENDING")], None);
        assert!(sh.jobs.is_empty());
        assert!(sh.error.contains("no running jobs"));
    }

    #[test]
    fn move_selection_clamps_at_bounds() {
        let mut sh = NodePickerSheet::new("nb", None, "u".into());
        sh.set_jobs(vec![job("1", "n1", "R"), job("2", "n2", "R")], None);
        sh.move_up(); // already at 0
        assert_eq!(sh.sel, 0);
        sh.move_down();
        sh.move_down();
        sh.move_down(); // clamp at last
        assert_eq!(sh.sel, 1);
        assert_eq!(sh.selected_node().as_deref(), Some("n2"));
    }

    #[test]
    fn resolve_node_list_mode_returns_selected() {
        let mut sh = NodePickerSheet::new("nb", None, "u".into());
        sh.set_jobs(vec![job("1", "holygpu01", "R")], None);
        assert_eq!(sh.resolve_node().as_deref(), Some("holygpu01"));
    }

    #[test]
    fn resolve_node_list_mode_empty_errors() {
        let mut sh = NodePickerSheet::new("nb", None, "u".into());
        sh.set_jobs(vec![], None);
        assert!(sh.resolve_node().is_none());
        assert!(sh.error.contains("no running jobs"));
    }

    #[test]
    fn resolve_node_custom_mode_trims_and_validates() {
        let mut sh = NodePickerSheet::new("nb", None, "u".into());
        sh.enter_custom();
        assert!(sh.custom);
        sh.node_buf = "  holygpu07  ".into();
        assert_eq!(sh.resolve_node().as_deref(), Some("holygpu07"));
    }

    #[test]
    fn resolve_node_custom_empty_errors() {
        let mut sh = NodePickerSheet::new("nb", None, "u".into());
        sh.enter_custom();
        sh.node_buf = "   ".into();
        assert!(sh.resolve_node().is_none());
        assert!(sh.error.contains("empty"));
    }

    #[test]
    fn confirm_delete_carries_name() {
        let sh = ConfirmDeleteSheet::new("jupyter");
        assert_eq!(sh.name, "jupyter");
    }

    #[test]
    fn truncate_shortens_long_strings() {
        assert_eq!(truncate("abc", 10), "abc");
        assert_eq!(truncate("abcdefghij", 5), "abcd\u{2026}");
    }

    #[test]
    fn help_lines_are_nonempty_and_cover_key_bindings() {
        let lines = help_lines();
        assert!(!lines.is_empty());
        let keys: Vec<&str> = lines.iter().map(|(k, _)| *k).collect();
        for expected in ["q", "Space", "Enter", "y", "d", "m", "r"] {
            assert!(keys.contains(&expected), "missing help key: {expected}");
        }
        // Section headers present.
        let sections: Vec<&str> =
            lines.iter().filter(|(k, _)| k.is_empty()).map(|(_, d)| *d).collect();
        assert!(sections.contains(&"Global"));
        assert!(sections.contains(&"Tunnels"));
        assert!(sections.contains(&"Hosts"));
    }
}
