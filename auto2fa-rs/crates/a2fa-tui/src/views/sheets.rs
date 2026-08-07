//! Modal input sheets: add-host, new-tunnel, node-picker.
//!
//! Each sheet renders a centered overlay and exposes an in-progress input
//! buffer that `main.rs` fills from key events.

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
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
///
/// Collects everything `host_add` requires: the daemon mandates a password —
/// the old one-field form could never succeed (every submit came back "bad
/// params: invalid otpauth URL"). The otpauth field is OPTIONAL: left blank it
/// registers a password-only host, and if filled it must parse.
#[derive(Debug, Clone, Default)]
pub struct AddHostSheet {
    /// The host alias being entered.
    pub host_buf: String,
    /// SSH password (rendered masked).
    pub password_buf: String,
    /// otpauth:// URL (or bare base32 secret — the daemon accepts both).
    pub otpauth_buf: String,
    /// Which field is focused (0 = host, 1 = password, 2 = otpauth).
    pub field: usize,
    /// Optional error to display.
    pub error: String,
}

impl AddHostSheet {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mutable buffer of the focused field.
    pub fn focused_buf(&mut self) -> &mut String {
        match self.field {
            1 => &mut self.password_buf,
            2 => &mut self.otpauth_buf,
            _ => &mut self.host_buf,
        }
    }

    pub const FIELD_COUNT: usize = 3;
}

/// Render the add-host modal.
pub fn render_add_host(f: &mut Frame, sheet: &AddHostSheet) {
    let area = centered_rect(64, 16, f.area());
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
            Constraint::Length(3), // password field
            Constraint::Length(3), // otpauth field
            Constraint::Length(1), // error line
            Constraint::Length(1), // hint
        ])
        .split(inner);

    render_input_field(f, chunks[0], "Host alias", &sheet.host_buf, sheet.field == 0);
    // Mask the password — never paint it on screen.
    let masked = "•".repeat(sheet.password_buf.chars().count());
    render_input_field(f, chunks[1], "SSH password", &masked, sheet.field == 1);
    render_input_field(
        f,
        chunks[2],
        "otpauth URL / TOTP secret (blank = no 2FA)",
        &sheet.otpauth_buf,
        sheet.field == 2,
    );

    if !sheet.error.is_empty() {
        let err = Paragraph::new(sheet.error.as_str())
            .style(Style::default().fg(Color::Red));
        f.render_widget(err, chunks[3]);
    }

    let hint = Paragraph::new("Tab/↑↓: switch field   Enter: confirm   Esc: cancel")
        .style(Style::default().fg(Color::DarkGray))
        .alignment(Alignment::Center);
    f.render_widget(hint, chunks[4]);
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
    /// The tunnel or host name to delete.
    pub name: String,
    /// What `name` refers to. Removing a HOST also deletes its stored
    /// credentials, so the two cases must not share one vague prompt.
    pub target: DeleteTarget,
}

/// What a confirm-delete sheet is about to remove.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DeleteTarget {
    #[default]
    Tunnel,
    Host,
}

impl ConfirmDeleteSheet {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            target: DeleteTarget::Tunnel,
        }
    }

    pub fn for_host(name: &str) -> Self {
        Self {
            name: name.to_string(),
            target: DeleteTarget::Host,
        }
    }

    /// The question to put to the user. A host removal says what else goes
    /// with it — the credentials are deleted too, and that is not recoverable
    /// from inside this app.
    pub fn question(&self) -> String {
        match self.target {
            DeleteTarget::Tunnel => format!("Delete tunnel '{}'?", self.name),
            DeleteTarget::Host => format!(
                "Remove host '{}' AND its saved password + 2FA secret?",
                self.name
            ),
        }
    }
}

/// Render the confirm-delete modal.
pub fn render_confirm_delete(f: &mut Frame, sheet: &ConfirmDeleteSheet) {
    // Wide enough, and tall enough to WRAP, because the host question is long
    // by design: it has to say that the saved password and 2FA secret go too.
    // At the old 60x7 that sentence was cut off mid-phrase ("… password + 2FA")
    // — losing the warning entirely while still looking like a complete prompt.
    let area = centered_rect(72, 9, f.area());
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
            Constraint::Length(3), // question (wraps)
            Constraint::Length(1), // spacer
            Constraint::Length(1), // hint
        ])
        .split(inner);

    let q = Paragraph::new(sheet.question())
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true });
    f.render_widget(q, chunks[1]);

    let hint = Paragraph::new("y: yes    n / Esc / q: cancel")
        .style(Style::default().fg(Color::DarkGray))
        .alignment(Alignment::Center);
    f.render_widget(hint, chunks[3]);
}

// ---------------------------------------------------------------------------
// Host detail sheet — what the macOS "Password & setup" view shows
// ---------------------------------------------------------------------------

/// What the daemon has stored for one host, plus its live 2FA code.
///
/// Deliberately mirrors `host_credentials`: never the password or the secret
/// itself, only whether they exist and what the secret describes. The one
/// value that IS shown is the current TOTP code, which is what you would read
/// off an authenticator anyway and which expires in seconds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostDetailSheet {
    pub host: String,
    /// None until the first `host_credentials` reply lands.
    pub loaded: bool,
    pub has_password: bool,
    pub password_length: usize,
    pub has_otp_secret: bool,
    /// Human summary of the stored secret ("Duo · alice", "SHA1/6/30s").
    pub otp_summary: String,
    /// Set when a secret IS stored but no longer parses — a broken credential,
    /// which is a different problem from having none.
    pub otp_error: String,
    pub auto_connect: bool,
    /// Live code + seconds left, from `host_totp`.
    pub code: String,
    pub code_seconds_left: u32,
    /// Result of the last "test login", shown until something else replaces it.
    pub test_result: String,
    pub test_ok: bool,
    /// Set while a blocking RPC is in flight so the UI can say so.
    pub busy: String,
    pub error: String,
}

impl HostDetailSheet {
    pub fn new(host: &str) -> Self {
        Self {
            host: host.to_string(),
            ..Default::default()
        }
    }

    /// What is stored for the password, as its own row.
    ///
    /// Password and 2FA get SEPARATE rows rather than one combined summary:
    /// combined, a realistic account name ("alice@login.example.edu") pushed
    /// the line past the modal's width and the terminal silently truncated the
    /// account — losing exactly the part that identifies which 2FA account is
    /// stored. Caught by the render test, which is the only thing that sees a
    /// width overflow.
    pub fn password_line(&self) -> String {
        if !self.loaded {
            return "loading…".into();
        }
        if self.has_password {
            format!("password ({} chars)", self.password_length)
        } else {
            "NO password saved".into()
        }
    }

    /// What is stored for 2FA, as its own row.
    pub fn otp_line(&self) -> String {
        if !self.loaded {
            return "loading…".into();
        }
        if !self.otp_error.is_empty() {
            return format!("UNREADABLE — {}", self.otp_error);
        }
        if !self.has_otp_secret {
            // Not a fault — a password-only host is supported.
            return "none (password-only host)".into();
        }
        if self.otp_summary.is_empty() {
            "stored".into()
        } else {
            self.otp_summary.clone()
        }
    }
}

/// Render the host-detail modal.
pub fn render_host_detail(f: &mut Frame, sheet: &HostDetailSheet) {
    let area = centered_rect(80, 16, f.area());
    f.render_widget(Clear, area);

    let block = Block::default()
        .title(format!("Host — {}", sheet.host))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD));
    f.render_widget(block, area);

    let inner = Rect {
        x: area.x + 2,
        y: area.y + 1,
        width: area.width.saturating_sub(4),
        height: area.height.saturating_sub(2),
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // password
            Constraint::Length(1), // 2FA secret
            Constraint::Length(1), // auto-connect
            Constraint::Length(1), // spacer
            Constraint::Length(1), // 2FA code
            Constraint::Length(1), // spacer
            Constraint::Length(2), // test-login result
            Constraint::Length(1), // busy / error
            Constraint::Min(1),    // spacer
            Constraint::Length(1), // hint
        ])
        .split(inner);

    let label = |s: &'static str| Span::styled(s, Style::default().fg(Color::DarkGray));
    f.render_widget(
        Paragraph::new(Line::from(vec![
            label("password: "),
            Span::raw(sheet.password_line()),
        ])),
        chunks[0],
    );
    f.render_widget(
        Paragraph::new(Line::from(vec![
            label("2FA:      "),
            Span::raw(sheet.otp_line()),
        ])),
        chunks[1],
    );
    f.render_widget(
        Paragraph::new(Line::from(vec![
            label("startup:  "),
            Span::raw(if sheet.auto_connect {
                "connects automatically"
            } else {
                "manual"
            }),
        ])),
        chunks[2],
    );

    // The live code, grouped like an authenticator (832 194) with the window
    // countdown next to it.
    let code_line = if !sheet.code.is_empty() {
        let grouped = if sheet.code.len() == 6 {
            format!("{} {}", &sheet.code[..3], &sheet.code[3..])
        } else {
            sheet.code.clone()
        };
        Line::from(vec![
            Span::styled("2FA code: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                grouped,
                Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("   {}s left", sheet.code_seconds_left),
                Style::default().fg(Color::DarkGray),
            ),
        ])
    } else if sheet.has_otp_secret {
        Line::from(Span::styled(
            "2FA code: press c to show",
            Style::default().fg(Color::DarkGray),
        ))
    } else {
        Line::from(Span::styled(
            "2FA code: none — this host signs in with a password only",
            Style::default().fg(Color::DarkGray),
        ))
    };
    f.render_widget(Paragraph::new(code_line), chunks[4]);

    if !sheet.test_result.is_empty() {
        let color = if sheet.test_ok { Color::Green } else { Color::Red };
        f.render_widget(
            Paragraph::new(sheet.test_result.as_str()).style(Style::default().fg(color)),
            chunks[6],
        );
    }
    if !sheet.busy.is_empty() {
        f.render_widget(
            Paragraph::new(sheet.busy.as_str()).style(Style::default().fg(Color::Yellow)),
            chunks[7],
        );
    } else if !sheet.error.is_empty() {
        f.render_widget(
            Paragraph::new(sheet.error.as_str()).style(Style::default().fg(Color::Red)),
            chunks[7],
        );
    }

    let hint = Paragraph::new("c: show 2FA code   t: test login   D: remove host   Esc: close")
        .style(Style::default().fg(Color::DarkGray))
        .alignment(Alignment::Center);
    f.render_widget(hint, chunks[9]);
}

// ---------------------------------------------------------------------------
// Tunnel edit sheet
// ---------------------------------------------------------------------------

/// Edit an existing tunnel's ports and auto-start flag.
///
/// Separate from [`NewTunnelSheet`] because the fields differ: creating takes a
/// name and one port, editing takes both ports and the startup flag but must
/// NOT let the name change silently (renaming is a distinct daemon call that
/// moves state, so it gets its own field and is applied only when it differs).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TunnelEditSheet {
    /// The name as it exists on the daemon — the RPC target.
    pub original_name: String,
    pub name_buf: String,
    pub local_buf: String,
    pub remote_buf: String,
    pub auto_start: bool,
    /// 0 = name, 1 = local port, 2 = remote port, 3 = auto-start toggle.
    pub field: usize,
    pub error: String,
}

impl TunnelEditSheet {
    pub const FIELD_COUNT: usize = 4;

    pub fn new(name: &str, local_port: u16, remote_port: u16, auto_start: bool) -> Self {
        Self {
            original_name: name.to_string(),
            name_buf: name.to_string(),
            local_buf: local_port.to_string(),
            remote_buf: remote_port.to_string(),
            auto_start,
            field: 0,
            error: String::new(),
        }
    }

    /// Mutable buffer of the focused text field (the toggle has none).
    pub fn focused_buf(&mut self) -> Option<&mut String> {
        match self.field {
            0 => Some(&mut self.name_buf),
            1 => Some(&mut self.local_buf),
            2 => Some(&mut self.remote_buf),
            _ => None,
        }
    }

    /// Parse the edited fields, or set `self.error` and return `None`.
    ///
    /// Ports are validated the same way `NewTunnelSheet` validates its one
    /// port: a forward below 1024 needs privileges the daemon does not have.
    pub fn validate(&mut self) -> Option<(String, u16, u16, bool)> {
        let name = self.name_buf.trim().to_string();
        if name.is_empty() {
            self.error = "Name cannot be empty.".into();
            self.field = 0;
            return None;
        }
        let local = match self.local_buf.trim().parse::<u16>() {
            Ok(p) if p >= 1024 => p,
            Ok(_) => {
                self.error = "Local port must be ≥ 1024.".into();
                self.field = 1;
                return None;
            }
            Err(_) => {
                self.error = "Local port must be a number.".into();
                self.field = 1;
                return None;
            }
        };
        let remote = match self.remote_buf.trim().parse::<u16>() {
            Ok(p) if p > 0 => p,
            _ => {
                self.error = "Remote port must be a number (1-65535).".into();
                self.field = 2;
                return None;
            }
        };
        Some((name, local, remote, self.auto_start))
    }
}

/// Render the tunnel-edit modal.
pub fn render_tunnel_edit(f: &mut Frame, sheet: &TunnelEditSheet) {
    let area = centered_rect(60, 15, f.area());
    f.render_widget(Clear, area);

    let block = Block::default()
        .title(format!("Edit tunnel — {}", sheet.original_name))
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
            Constraint::Length(3), // name
            Constraint::Length(3), // local port
            Constraint::Length(3), // remote port
            Constraint::Length(1), // auto-start
            Constraint::Length(1), // error
            Constraint::Length(1), // hint
        ])
        .split(inner);

    render_input_field(f, chunks[0], "Name", &sheet.name_buf, sheet.field == 0);
    render_input_field(f, chunks[1], "Local port", &sheet.local_buf, sheet.field == 1);
    render_input_field(f, chunks[2], "Remote port", &sheet.remote_buf, sheet.field == 2);

    let toggle_style = if sheet.field == 3 {
        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    f.render_widget(
        Paragraph::new(format!(
            "  Start automatically: [{}]  (Space toggles)",
            if sheet.auto_start { "x" } else { " " }
        ))
        .style(toggle_style),
        chunks[3],
    );

    if !sheet.error.is_empty() {
        f.render_widget(
            Paragraph::new(sheet.error.as_str()).style(Style::default().fg(Color::Red)),
            chunks[4],
        );
    }

    let hint = Paragraph::new("Tab: next field   Enter: save   Esc: cancel")
        .style(Style::default().fg(Color::DarkGray))
        .alignment(Alignment::Center);
    f.render_widget(hint, chunks[5]);
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
        ("e", "Edit ports / auto-start"),
        ("y", "Copy URL to clipboard"),
        ("d", "Delete the selected tunnel"),
        ("s / x", "Start / stop (explicit aliases)"),
        ("", "Hosts"),
        ("Space", "Start / stop the selected host"),
        ("Enter", "Details: credentials, 2FA code, test login"),
        ("a", "Add a host"),
        ("m", "Mount / unmount remote filesystem"),
        ("r", "Rotate connection pool"),
        ("", "Host details (Enter)"),
        ("c", "Show the current 2FA code"),
        ("t", "Test login with the stored credentials"),
        ("D", "Remove the host and its credentials"),
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
        sh.set_jobs(vec![job("1", "gpunode01", "R")], None);
        assert_eq!(sh.resolve_node().as_deref(), Some("gpunode01"));
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
        sh.node_buf = "  gpunode07  ".into();
        assert_eq!(sh.resolve_node().as_deref(), Some("gpunode07"));
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

    #[test]
    fn add_host_sheet_focused_buf_routes_by_field() {
        let mut sh = AddHostSheet::new();
        sh.field = 0;
        sh.focused_buf().push_str("k9");
        sh.field = 1;
        sh.focused_buf().push_str("pw");
        sh.field = 2;
        sh.focused_buf().push_str("otpauth://x");
        assert_eq!(sh.host_buf, "k9");
        assert_eq!(sh.password_buf, "pw");
        assert_eq!(sh.otpauth_buf, "otpauth://x");
        // Out-of-range field falls back to host (defensive).
        sh.field = 99;
        sh.focused_buf().push('!');
        assert_eq!(sh.host_buf, "k9!");
    }

    // ---- HostDetailSheet -------------------------------------------------

    /// A host with no 2FA secret is a SUPPORTED setup, not a fault, and the
    /// summary must not read like one — that wording is what tells someone
    /// whether to go looking for a problem.
    #[test]
    fn host_detail_describes_a_password_only_host_neutrally() {
        let mut sh = HostDetailSheet::new("k6");
        sh.loaded = true;
        sh.has_password = true;
        sh.password_length = 12;
        sh.has_otp_secret = false;
        assert_eq!(sh.password_line(), "password (12 chars)");
        let otp = sh.otp_line();
        assert!(otp.contains("password-only"), "{otp}");
        assert!(!otp.to_lowercase().contains("missing"), "{otp}");
        assert!(!otp.to_lowercase().contains("no 2fa secret found"), "{otp}");
    }

    /// A stored-but-unparseable secret is a DIFFERENT problem from having
    /// none: one needs repair, the other is fine. They must never render the
    /// same, or a corrupt secret looks like a deliberate password-only host.
    #[test]
    fn host_detail_separates_a_broken_secret_from_a_missing_one() {
        let mut sh = HostDetailSheet::new("k6");
        sh.loaded = true;
        sh.has_password = true;
        sh.has_otp_secret = true;
        sh.otp_error = "invalid base32".into();
        let broken = sh.otp_line();
        assert!(broken.contains("UNREADABLE"), "{broken}");

        let mut none = HostDetailSheet::new("k6");
        none.loaded = true;
        none.has_password = true;
        none.has_otp_secret = false;
        assert_ne!(broken, none.otp_line());
    }

    #[test]
    fn host_detail_says_loading_until_the_first_reply() {
        let sh = HostDetailSheet::new("k6");
        assert_eq!(sh.password_line(), "loading…");
        assert_eq!(sh.otp_line(), "loading…");
    }

    /// A missing password is never a valid state and must be visible as such.
    #[test]
    fn host_detail_flags_a_missing_password() {
        let mut sh = HostDetailSheet::new("k6");
        sh.loaded = true;
        sh.has_password = false;
        sh.has_otp_secret = true;
        assert!(sh.password_line().contains("NO password"));
    }

    // ---- TunnelEditSheet -------------------------------------------------

    #[test]
    fn tunnel_edit_seeds_from_the_existing_tunnel() {
        let sh = TunnelEditSheet::new("claw", 3002, 3001, true);
        assert_eq!(sh.original_name, "claw");
        assert_eq!(sh.name_buf, "claw");
        assert_eq!(sh.local_buf, "3002");
        assert_eq!(sh.remote_buf, "3001");
        assert!(sh.auto_start);
    }

    #[test]
    fn tunnel_edit_validates_and_returns_the_edited_values() {
        let mut sh = TunnelEditSheet::new("claw", 3002, 3001, false);
        sh.name_buf = "claw2".into();
        sh.local_buf = "3005".into();
        sh.remote_buf = "8080".into();
        sh.auto_start = true;
        assert_eq!(
            sh.validate(),
            Some(("claw2".to_string(), 3005, 8080, true))
        );
    }

    /// A local forward below 1024 needs privileges the daemon does not have,
    /// so it must be refused here rather than failing later at bind time.
    #[test]
    fn tunnel_edit_rejects_a_privileged_local_port() {
        let mut sh = TunnelEditSheet::new("claw", 3002, 3001, false);
        sh.local_buf = "80".into();
        assert_eq!(sh.validate(), None);
        assert!(sh.error.contains("1024"), "{}", sh.error);
        assert_eq!(sh.field, 1, "focus must move to the offending field");
    }

    #[test]
    fn tunnel_edit_rejects_junk_and_empty_fields() {
        let mut sh = TunnelEditSheet::new("claw", 3002, 3001, false);
        sh.name_buf = "  ".into();
        assert_eq!(sh.validate(), None);
        assert_eq!(sh.field, 0);

        let mut sh = TunnelEditSheet::new("claw", 3002, 3001, false);
        sh.local_buf = "abc".into();
        assert_eq!(sh.validate(), None);
        assert_eq!(sh.field, 1);

        let mut sh = TunnelEditSheet::new("claw", 3002, 3001, false);
        sh.remote_buf = "0".into();
        assert_eq!(sh.validate(), None);
        assert_eq!(sh.field, 2);
    }

    /// The toggle row has no text buffer; typing while it is focused must be a
    /// no-op rather than leaking characters into whichever field was last.
    #[test]
    fn tunnel_edit_toggle_field_has_no_text_buffer() {
        let mut sh = TunnelEditSheet::new("claw", 3002, 3001, false);
        sh.field = 3;
        assert!(sh.focused_buf().is_none());
        assert_eq!(sh.local_buf, "3002", "the ports must be untouched");
    }

    #[test]
    fn tunnel_edit_focused_buf_routes_by_field() {
        let mut sh = TunnelEditSheet::new("t", 1024, 1025, false);
        for (field, expected) in [(0usize, "t!"), (1, "1024!"), (2, "1025!")] {
            sh.field = field;
            sh.focused_buf().unwrap().push('!');
            let got = match field {
                0 => &sh.name_buf,
                1 => &sh.local_buf,
                _ => &sh.remote_buf,
            };
            assert_eq!(got, expected);
        }
    }

    // ---- ConfirmDeleteSheet ----------------------------------------------

    /// Removing a HOST also deletes its saved password and 2FA secret. That is
    /// not recoverable from inside the app, so the prompt must say so instead
    /// of reusing the tunnel wording.
    #[test]
    fn confirming_a_host_removal_warns_about_the_credentials() {
        let host = ConfirmDeleteSheet::for_host("k6");
        assert_eq!(host.target, DeleteTarget::Host);
        let q = host.question();
        assert!(q.contains("k6"), "{q}");
        assert!(q.contains("2FA secret"), "must name what else goes: {q}");

        let tunnel = ConfirmDeleteSheet::new("claw");
        assert_eq!(tunnel.target, DeleteTarget::Tunnel);
        assert!(tunnel.question().contains("Delete tunnel"));
        assert!(!tunnel.question().contains("2FA"));
    }

    // ---- Rendering -------------------------------------------------------
    //
    // A TUI cannot be driven by hand in CI, and a sheet that panics or paints
    // nothing looks identical to one that was never opened. These render into
    // an in-memory terminal and assert on the actual glyphs, which is the only
    // way to catch a layout that overflows its box or a field that silently
    // stops being drawn.

    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    /// Render one sheet into an 80x24 buffer and return its text, one line per
    /// row, with trailing padding trimmed.
    fn draw(f: impl FnOnce(&mut Frame)) -> String {
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|frame| f(frame)).unwrap();
        let buf = term.backend().buffer().clone();
        let mut out = String::new();
        for y in 0..buf.area.height {
            let mut line = String::new();
            for x in 0..buf.area.width {
                line.push_str(buf[(x, y)].symbol());
            }
            out.push_str(line.trim_end());
            out.push('\n');
        }
        out
    }

    #[test]
    fn host_detail_renders_its_state_and_actions() {
        let mut sh = HostDetailSheet::new("k6");
        sh.loaded = true;
        sh.has_password = true;
        sh.password_length = 16;
        sh.has_otp_secret = true;
        sh.otp_summary = "alice@login.example.edu".into();
        sh.auto_connect = true;
        sh.code = "217746".into();
        sh.code_seconds_left = 12;
        let out = draw(|f| render_host_detail(f, &sh));

        assert!(out.contains("Host — k6"), "{out}");
        assert!(out.contains("password (16 chars)"), "{out}");
        // The full account name must survive — truncating it loses exactly the
        // part that says WHICH 2FA account is stored.
        assert!(out.contains("alice@login.example.edu"), "{out}");
        assert!(out.contains("connects automatically"), "{out}");
        // Grouped like an authenticator, with the window countdown.
        assert!(out.contains("217 746"), "code must be grouped: {out}");
        assert!(out.contains("12s left"), "{out}");
        // Every action the sheet accepts must be discoverable on screen.
        for key in ["c:", "t:", "D:"] {
            assert!(out.contains(key), "missing {key} in hint: {out}");
        }
    }

    /// The password-only case must render as a plain statement, not as an
    /// error, and must not invite the user to press a key that cannot work.
    #[test]
    fn host_detail_renders_a_password_only_host_without_alarm() {
        let mut sh = HostDetailSheet::new("plain");
        sh.loaded = true;
        sh.has_password = true;
        sh.password_length = 8;
        sh.has_otp_secret = false;
        let out = draw(|f| render_host_detail(f, &sh));
        assert!(out.contains("password only"), "{out}");
        assert!(!out.contains("press c to show"), "no code to offer: {out}");
    }

    #[test]
    fn host_detail_renders_a_failed_test_login() {
        let mut sh = HostDetailSheet::new("k6");
        sh.loaded = true;
        sh.has_password = true;
        sh.test_ok = false;
        sh.test_result = "The server rejected the password.".into();
        let out = draw(|f| render_host_detail(f, &sh));
        assert!(out.contains("rejected the password"), "{out}");
    }

    /// While a login runs the sheet must SAY so — the RPC is a real ssh login
    /// and can take tens of seconds.
    #[test]
    fn host_detail_shows_that_a_test_is_running() {
        let mut sh = HostDetailSheet::new("k6");
        sh.loaded = true;
        sh.busy = "testing login…".into();
        let out = draw(|f| render_host_detail(f, &sh));
        assert!(out.contains("testing login"), "{out}");
    }

    #[test]
    fn tunnel_edit_renders_every_field_and_the_toggle() {
        let sh = TunnelEditSheet::new("claw", 3002, 3001, true);
        let out = draw(|f| render_tunnel_edit(f, &sh));
        assert!(out.contains("Edit tunnel — claw"), "{out}");
        assert!(out.contains("Name: claw"), "{out}");
        assert!(out.contains("Local port: 3002"), "{out}");
        assert!(out.contains("Remote port: 3001"), "{out}");
        assert!(out.contains("Start automatically: [x]"), "{out}");

        let off = TunnelEditSheet::new("claw", 3002, 3001, false);
        let out = draw(|f| render_tunnel_edit(f, &off));
        assert!(out.contains("Start automatically: [ ]"), "{out}");
    }

    #[test]
    fn tunnel_edit_renders_a_validation_error() {
        let mut sh = TunnelEditSheet::new("claw", 3002, 3001, false);
        sh.local_buf = "80".into();
        assert!(sh.validate().is_none());
        let out = draw(|f| render_tunnel_edit(f, &sh));
        assert!(out.contains("1024"), "the error must be visible: {out}");
    }

    #[test]
    fn confirm_delete_renders_the_host_warning() {
        let sh = ConfirmDeleteSheet::for_host("k6");
        let out = draw(|f| render_confirm_delete(f, &sh));
        assert!(out.contains("k6"), "{out}");
        assert!(out.contains("2FA secret"), "{out}");
        assert!(out.contains("y: yes"), "{out}");
    }

    /// The help modal is the only discovery surface for these keys, so every
    /// binding the sheets implement must appear in it.
    #[test]
    fn help_lists_the_new_bindings() {
        let keys: Vec<&str> = help_lines().iter().map(|(k, _)| *k).collect();
        for k in ["Enter", "e", "a", "c", "t", "D"] {
            assert!(keys.contains(&k), "help is missing {k:?}: {keys:?}");
        }
    }
}
