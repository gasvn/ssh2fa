//! Hosts table view.

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Cell, Row, Table, TableState},
    Frame,
};

use crate::app::{AppModel, Pane};

/// Render the hosts table into `area`.
///
/// The table shows: Status glyph | Host | Pool | Mounted | Last message
pub fn render_hosts(f: &mut Frame, area: Rect, app: &AppModel) {
    let is_focused = app.focus == Pane::Hosts;
    let border_style = if is_focused {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let title = if is_focused { "[ HOSTS ]" } else { " HOSTS " };

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(border_style);

    let header_cells = ["", "Host", "Pool", "Mnt", "Message"]
        .iter()
        .map(|h| Cell::from(*h).style(Style::default().add_modifier(Modifier::BOLD)));
    let header = Row::new(header_cells).height(1).bottom_margin(0);

    let filter = &app.filter;
    let visible = app.visible_hosts(filter);

    let rows: Vec<Row> = visible
        .iter()
        .enumerate()
        .map(|(i, h)| {
            let (glyph, color) = host_status_glyph(h.is_master_ready, h.active);
            let pool = format!("{}/{}", h.pool_index, h.pool_alive);
            let mnt = if h.is_mounted { "Y" } else { "N" };
            let msg: String = h.last_msg.chars().take(40).collect();

            let style = if is_focused && i == app.hosts_sel {
                Style::default()
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };

            Row::new(vec![
                Cell::from(glyph).style(Style::default().fg(color)),
                Cell::from(h.host.clone()),
                Cell::from(pool),
                Cell::from(mnt),
                Cell::from(msg),
            ])
            .style(style)
        })
        .collect();

    let widths = [
        ratatui::layout::Constraint::Length(3),
        ratatui::layout::Constraint::Length(30),
        ratatui::layout::Constraint::Length(5),
        ratatui::layout::Constraint::Length(3),
        ratatui::layout::Constraint::Min(10),
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .block(block)
        .row_highlight_style(Style::default().bg(Color::DarkGray));

    // Use a stateless render — selection is tracked in AppModel and conveyed
    // via row styling above, so we don't need TableState here.
    let mut _state = TableState::default();
    f.render_stateful_widget(table, area, &mut _state);
}

fn host_status_glyph(is_master_ready: bool, active: bool) -> (&'static str, Color) {
    if !active {
        return ("○", Color::DarkGray);
    }
    if is_master_ready {
        ("●", Color::Green)
    } else {
        ("◐", Color::Yellow)
    }
}
