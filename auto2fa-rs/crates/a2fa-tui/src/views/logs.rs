//! Log viewer.

use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Text},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::app::AppModel;

/// Render the log viewer, showing the most recent lines that fit in `area`.
pub fn render_logs(f: &mut Frame, area: Rect, app: &AppModel) {
    let block = Block::default()
        .title("[ LOGS ]")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow));

    // Inner height (excluding borders).
    let inner_height = area.height.saturating_sub(2) as usize;

    // Show only the last `inner_height` lines so the view is always at tail.
    let total = app.log_lines.len();
    let start = total.saturating_sub(inner_height);

    let lines: Vec<Line> = app.log_lines[start..]
        .iter()
        .map(|l| Line::from(l.as_str()))
        .collect();

    let text = Text::from(lines);
    let para = Paragraph::new(text).block(block);
    f.render_widget(para, area);
}
