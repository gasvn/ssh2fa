//! Tunnels table view.

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Cell, Row, Table, TableState},
    Frame,
};

use crate::app::{status_color, AppModel, Pane};

/// Render the tunnels table into `area`.
///
/// Columns: Status | A | P | Name | Ports | Jump | Node | Message
pub fn render_tunnels(f: &mut Frame, area: Rect, app: &AppModel) {
    let is_focused = app.focus == Pane::Tunnels;
    let border_style = if is_focused {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let title = if is_focused {
        "[ TUNNELS ]"
    } else {
        " TUNNELS "
    };

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(border_style);

    let header_cells = ["", "A", "P", "Name", "Ports", "Jump", "Node", "Message"]
        .iter()
        .map(|h| Cell::from(*h).style(Style::default().add_modifier(Modifier::BOLD)));
    let header = Row::new(header_cells).height(1);

    let filter = &app.filter;
    let visible = app.visible_tunnels(filter);

    let rows: Vec<Row> = visible
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let status_str = t.status.to_string();
            let color = status_color(&status_str);
            let glyph = match status_str.as_str() {
                "alive" => "●",
                "starting" => "◐",
                "failed" | "port_busy" | "stale" => "●",
                _ => "○",
            };
            let auto = if t.auto_start { "*" } else { " " };
            let pinned = if t
                .jump_candidates
                .as_ref()
                .map(|c| !c.is_empty())
                .unwrap_or(false)
            {
                "P"
            } else {
                " "
            };
            let ports = format!("{}→{}", t.local_port, t.remote_port);
            let jump = t.active_jump.as_deref().unwrap_or("—");
            let node = t.last_node.as_deref().unwrap_or("—");
            let msg: String = t.last_msg.chars().take(30).collect();

            let row_style = if is_focused && i == app.tunnels_sel {
                Style::default()
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };

            Row::new(vec![
                Cell::from(glyph).style(Style::default().fg(color)),
                Cell::from(auto),
                Cell::from(pinned),
                Cell::from(t.name.clone()),
                Cell::from(ports),
                Cell::from(jump.to_string()),
                Cell::from(node.to_string()),
                Cell::from(msg),
            ])
            .style(row_style)
        })
        .collect();

    let widths = [
        ratatui::layout::Constraint::Length(3),  // glyph
        ratatui::layout::Constraint::Length(2),  // auto
        ratatui::layout::Constraint::Length(2),  // pinned
        ratatui::layout::Constraint::Length(20), // name
        ratatui::layout::Constraint::Length(12), // ports
        ratatui::layout::Constraint::Length(12), // jump
        ratatui::layout::Constraint::Length(14), // node
        ratatui::layout::Constraint::Min(10),    // message
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .block(block)
        .row_highlight_style(Style::default().bg(Color::DarkGray));

    let mut _state = TableState::default();
    f.render_stateful_widget(table, area, &mut _state);
}
