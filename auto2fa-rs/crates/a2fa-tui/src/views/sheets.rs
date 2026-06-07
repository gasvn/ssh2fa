//! Modal input sheets: add-host, new-tunnel, node-picker.
//!
//! Each sheet renders a centered overlay and exposes an in-progress input
//! buffer that `main.rs` fills from key events.

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
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
// Node-picker sheet
// ---------------------------------------------------------------------------

/// State for the node-picker modal.
#[derive(Debug, Clone, Default)]
pub struct NodePickerSheet {
    pub node_buf: String,
    pub user_buf: String,
    /// 0 = node, 1 = user.
    pub field: usize,
    pub error: String,
    /// The tunnel name this picker is for.
    pub tunnel_name: String,
}

impl NodePickerSheet {
    #[allow(dead_code)]
    pub fn new(tunnel_name: &str) -> Self {
        Self {
            tunnel_name: tunnel_name.to_string(),
            ..Self::default()
        }
    }

    /// Return `(node, user)` if both are non-empty.
    pub fn validate(&mut self) -> Option<(String, String)> {
        let node = self.node_buf.trim().to_string();
        if node.is_empty() {
            self.error = "Node cannot be empty.".to_string();
            self.field = 0;
            return None;
        }
        let user = self.user_buf.trim().to_string();
        if user.is_empty() {
            self.error = "User cannot be empty.".to_string();
            self.field = 1;
            return None;
        }
        Some((node, user))
    }
}

/// Render the node-picker modal.
pub fn render_node_picker(f: &mut Frame, sheet: &NodePickerSheet) {
    let title = format!("Set node for '{}'", sheet.tunnel_name);
    let area = centered_rect(70, 12, f.area());
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
            Constraint::Length(3), // node field
            Constraint::Length(3), // user field
            Constraint::Length(1), // error
            Constraint::Length(1), // hint
        ])
        .split(inner);

    render_input_field(f, chunks[0], "Node", &sheet.node_buf, sheet.field == 0);
    render_input_field(f, chunks[1], "User", &sheet.user_buf, sheet.field == 1);

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
