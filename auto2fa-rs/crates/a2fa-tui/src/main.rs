//! `ssh2fa-tui` — ratatui terminal UI for the SSH2FA daemon.
//!
//! Architecture:
//!   - `AppModel` (app.rs): pure, I/O-free view-model.
//!   - `client.rs`: Unix-socket RPC + event subscription.
//!   - `views/`: ratatui render functions (stateless).
//!   - `main.rs`: crossterm setup, event loop, IPC dispatch.
//!
//! CPU-burn avoidance:
//!   The event loop calls `poll(250ms)` — it only redraws when a crossterm
//!   key event arrives OR when a daemon event is pushed onto the channel.
//!   There is NO continuous high-FPS render loop.

mod app;
mod client;
mod notify;
mod views;

use std::io;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{
        disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
    },
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    widgets::{Block, Borders, Paragraph},
    Terminal,
};
use serde_json::Value;

use app::{tunnel_url, AppModel, InputMode, Pane, SheetKind};
use views::{
    hosts::render_hosts,
    logs::render_logs,
    sheets::{
        render_add_host, render_confirm_delete, render_help, render_host_detail,
        render_new_tunnel, render_node_picker, render_tunnel_edit, AddHostSheet,
        ConfirmDeleteSheet, DeleteTarget, HostDetailSheet, NewTunnelSheet, NodePickerSheet,
        SqueueJob, TunnelEditSheet,
    },
    tunnels::render_tunnels,
};

// ---------------------------------------------------------------------------
// Sheet state carried alongside AppModel
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Sheets {
    add_host: Option<AddHostSheet>,
    host_detail: Option<HostDetailSheet>,
    new_tunnel: Option<NewTunnelSheet>,
    tunnel_edit: Option<TunnelEditSheet>,
    node_picker: Option<NodePickerSheet>,
    confirm_delete: Option<ConfirmDeleteSheet>,
}

/// Result of a long-running action, delivered back to the UI thread.
///
/// "Test login" runs a REAL ssh login, which the daemon bounds at 60 s. Doing
/// that inline froze the whole TUI for up to a minute with no key handling and
/// no explanation — the one place where a blocking RPC is long enough to look
/// like a hang rather than a pause. It runs on a worker thread instead and
/// sends its verdict here; the UI keeps redrawing and stays quittable.
enum SheetResult {
    TestLogin {
        host: String,
        ok: bool,
        message: String,
    },
}

// ---------------------------------------------------------------------------
// Main entry
// ---------------------------------------------------------------------------

fn main() {
    if let Err(e) = run() {
        eprintln!("ssh2fa-tui error: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    // Terminal setup — also install a panic hook that restores the terminal
    // before printing the panic, so the user's shell is not left in raw mode.
    enable_raw_mode().context("enable raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)
        .context("enter alternate screen")?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("create terminal")?;

    // Install panic hook to restore terminal on panic.
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            LeaveAlternateScreen,
            DisableMouseCapture
        );
        original_hook(info);
    }));

    let result = event_loop(&mut terminal);

    // Always restore terminal.
    disable_raw_mode().ok();
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )
    .ok();
    terminal.show_cursor().ok();

    result
}

// ---------------------------------------------------------------------------
// Event loop
// ---------------------------------------------------------------------------

fn event_loop(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    let mut app = AppModel::new();
    let mut sheets = Sheets::default();

    // Paint ONE frame before the initial (blocking, up to 30 s/RPC) loads —
    // a wedged daemon previously meant a frozen BLANK alternate screen with
    // no indication of what was happening.
    app.status_msg = "connecting to daemon…".to_string();
    terminal.draw(|f| render_frame(f, &app, &sheets))?;
    app.status_msg.clear();

    // Initial load.
    match client::rpc("list_hosts", serde_json::json!({})) {
        Ok(v) => {
            if let Ok(hosts) = serde_json::from_value(v) {
                app.set_hosts(hosts);
            }
        }
        Err(e) => {
            app.status_msg = format!("list_hosts failed: {e}");
        }
    }
    match client::rpc("list_tunnels", serde_json::json!({})) {
        Ok(v) => {
            if let Ok(tunnels) = serde_json::from_value(v) {
                // Initial load: seed last_seen_status without spamming toasts /
                // notifications for tunnels that are already alive.
                app.set_tunnels(tunnels);
                app.toast = None;
                app.status_msg.clear();
            }
        }
        Err(e) => {
            app.status_msg = format!("list_tunnels failed: {e}");
        }
    }

    // Fetch initial log tail.
    refresh_logs(&mut app);

    // Spawn the event subscriber on a background thread with a RECONNECT
    // loop. The old single `subscribe` call exited silently on daemon
    // restart (or if the daemon wasn't up yet) — live updates died for the
    // rest of the session with zero indication. The synthetic events let the
    // UI report the gap and re-fetch full snapshots on reconnect.
    let (tx, rx): (mpsc::Sender<Value>, Receiver<Value>) = mpsc::channel();
    {
        let tx2 = tx.clone();
        std::thread::spawn(move || loop {
            let _ = client::subscribe(tx2.clone());
            // Receiver gone → the TUI is shutting down; stop retrying.
            if tx2
                .send(serde_json::json!({ "event": "__events_disconnected" }))
                .is_err()
            {
                return;
            }
            std::thread::sleep(Duration::from_secs(2));
        });
    }

    // Results of long-running sheet actions, posted back from worker threads.
    let (results_tx, results_rx): (mpsc::Sender<SheetResult>, Receiver<SheetResult>) =
        mpsc::channel();

    // Render the loaded state.
    terminal.draw(|f| render_frame(f, &app, &sheets))?;

    loop {
        // Drain any pending daemon events (non-blocking), COALESCED: an event
        // storm (post-wake flapping) queued one full list RPC per event —
        // K events = K sequential 30s-timeout RPCs on this UI thread. Collect
        // flags across the whole drain, then refresh each list at most once.
        let mut flags = EventFlags::default();
        while let Ok(event) = rx.try_recv() {
            flags.collect(&event);
        }
        let got_daemon_event = flags.any();
        flags.apply(&mut app);

        // Apply any finished long-running action.
        let mut got_result = false;
        while let Ok(res) = results_rx.try_recv() {
            got_result = true;
            apply_sheet_result(res, &mut sheets);
        }

        // Poll for crossterm key/resize events (250 ms timeout).
        // This is the sole render-triggering mechanism — no busy loop.
        let got_key = event::poll(Duration::from_millis(250))?;
        let mut needs_redraw = got_daemon_event || got_result;

        if got_key {
            // poll() guarantees exactly ONE event is buffered. Read it once and
            // match on that single value — calling event::read() a second time
            // (e.g. for a Mouse event falling through to an else-if) would block
            // the UI thread on a phantom read of an empty buffer.
            match event::read()? {
                Event::Key(key) => {
                    needs_redraw = true;
                    handle_key(key, &mut app, &mut sheets, &results_tx);
                }
                Event::Resize(_, _) => {
                    needs_redraw = true;
                }
                // Mouse capture is enabled but unused; ignore these (and any
                // focus/paste events) without a second read.
                Event::Mouse(_)
                | Event::FocusGained
                | Event::FocusLost
                | Event::Paste(_) => {}
            }
        }

        // Drain events that may have arrived during key handling (coalesced
        // the same way as the top-of-loop drain).
        let mut flags = EventFlags::default();
        while let Ok(ev) = rx.try_recv() {
            flags.collect(&ev);
        }
        if flags.any() {
            needs_redraw = true;
        }
        flags.apply(&mut app);

        // Auto-clear an expired toast (loop wakes at least every 250 ms).
        if app.expire_toast() {
            app.status_msg.clear();
            needs_redraw = true;
        }

        if app.should_quit {
            break;
        }

        if needs_redraw {
            terminal.draw(|f| render_frame(f, &app, &sheets))?;
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Key handler
// ---------------------------------------------------------------------------

fn handle_key(
    key: event::KeyEvent,
    app: &mut AppModel,
    sheets: &mut Sheets,
    results: &mpsc::Sender<SheetResult>,
) {
    // Ctrl+C — handle BEFORE the modal matches: the sheet branches match
    // KeyCode::Char without checking modifiers, so Ctrl+C used to insert a
    // literal 'c' into the focused buffer (raw mode = no SIGINT either).
    // In a modal it cancels the modal (like Esc); in Normal mode it quits.
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        match app.input_mode {
            InputMode::Normal => app.should_quit = true,
            InputMode::Filter => app.cancel_filter(),
            InputMode::Sheet(_) => {
                // Clear EVERY sheet — a new one added here and forgotten would
                // survive the cancel and reappear over the next modal.
                *sheets = Sheets::default();
                app.input_mode = InputMode::Normal;
            }
        }
        return;
    }

    // ----- Sheet / modal input modes -----
    match app.input_mode {
        InputMode::Sheet(SheetKind::AddHost) => {
            if let Some(ref mut sh) = sheets.add_host {
                match key.code {
                    KeyCode::Esc => {
                        sheets.add_host = None;
                        app.input_mode = InputMode::Normal;
                    }
                    // Field navigation (3 fields: host / password / otpauth).
                    KeyCode::Tab | KeyCode::Down => {
                        sh.field = (sh.field + 1) % AddHostSheet::FIELD_COUNT;
                    }
                    KeyCode::BackTab | KeyCode::Up => {
                        sh.field =
                            (sh.field + AddHostSheet::FIELD_COUNT - 1) % AddHostSheet::FIELD_COUNT;
                    }
                    KeyCode::Enter => {
                        let host = sh.host_buf.trim().to_string();
                        let password = sh.password_buf.clone();
                        let otpauth = sh.otpauth_buf.trim().to_string();
                        // Validate locally — the daemon mandates a host and a
                        // password (the old host-only submit ALWAYS failed
                        // bad_params). The 2FA secret is OPTIONAL: an account
                        // without 2FA is added by leaving that field blank.
                        if host.is_empty() {
                            sh.error = "Host alias cannot be empty.".to_string();
                            sh.field = 0;
                        } else if password.is_empty() {
                            sh.error = "SSH password cannot be empty.".to_string();
                            sh.field = 1;
                        } else {
                            let res = client::rpc(
                                "host_add",
                                serde_json::json!({
                                    "host": host,
                                    "password": password,
                                    "otpauth_url": otpauth,
                                    "auto_connect": true,
                                }),
                            );
                            match res {
                                Ok(_) => {
                                    app.status_msg = format!("Added host {host}");
                                    refresh_hosts(app);
                                    sheets.add_host = None;
                                    app.input_mode = InputMode::Normal;
                                }
                                Err(e) => {
                                    // Keep the sheet open so the user can fix
                                    // the bad field instead of retyping all.
                                    sh.error = format!("host_add failed: {e}");
                                }
                            }
                        }
                    }
                    KeyCode::Backspace => {
                        sh.focused_buf().pop();
                        sh.error.clear();
                    }
                    KeyCode::Char(c) => {
                        sh.focused_buf().push(c);
                        sh.error.clear();
                    }
                    _ => {}
                }
            }
            return;
        }

        InputMode::Sheet(SheetKind::NewTunnel) => {
            if let Some(ref mut sh) = sheets.new_tunnel {
                match key.code {
                    KeyCode::Esc => {
                        sheets.new_tunnel = None;
                        app.input_mode = InputMode::Normal;
                    }
                    KeyCode::Tab => {
                        sh.field = if sh.field == 0 { 1 } else { 0 };
                    }
                    KeyCode::Enter => {
                        if let Some((name, port)) = sh.validate() {
                            let res = client::rpc(
                                "tunnel_add",
                                serde_json::json!({
                                    "name": name,
                                    "local_port": port,
                                    "remote_port": port,
                                }),
                            );
                            match res {
                                Ok(_) => {
                                    app.status_msg = format!("Added tunnel {name}");
                                    refresh_tunnels(app);
                                }
                                Err(e) => {
                                    app.status_msg = format!("tunnel_add failed: {e}");
                                }
                            }
                            sheets.new_tunnel = None;
                            app.input_mode = InputMode::Normal;
                        }
                    }
                    KeyCode::Backspace => {
                        if sh.field == 0 {
                            sh.name_buf.pop();
                        } else {
                            sh.port_buf.pop();
                        }
                        sh.error.clear();
                    }
                    KeyCode::Char(c) => {
                        if sh.field == 0 {
                            sh.name_buf.push(c);
                        } else {
                            sh.port_buf.push(c);
                        }
                        sh.error.clear();
                    }
                    _ => {}
                }
            }
            return;
        }

        InputMode::Sheet(SheetKind::NodePicker) => {
            if let Some(ref mut sh) = sheets.node_picker {
                if sh.custom {
                    // ----- custom free-text entry sub-mode -----
                    match key.code {
                        KeyCode::Esc => {
                            sheets.node_picker = None;
                            app.input_mode = InputMode::Normal;
                        }
                        KeyCode::Enter => {
                            if let Some(node) = sh.resolve_node() {
                                let name = sh.tunnel_name.clone();
                                let user = sh.user.clone();
                                submit_node(app, &name, &node, &user);
                                sheets.node_picker = None;
                                app.input_mode = InputMode::Normal;
                            }
                        }
                        KeyCode::Backspace => {
                            sh.node_buf.pop();
                            sh.error.clear();
                        }
                        KeyCode::Char(c) => {
                            sh.node_buf.push(c);
                            sh.error.clear();
                        }
                        _ => {}
                    }
                } else {
                    // ----- list-pick mode -----
                    match key.code {
                        KeyCode::Esc | KeyCode::Char('q') => {
                            sheets.node_picker = None;
                            app.input_mode = InputMode::Normal;
                        }
                        KeyCode::Down | KeyCode::Char('j') => sh.move_down(),
                        KeyCode::Up | KeyCode::Char('k') => sh.move_up(),
                        KeyCode::Char('c') => sh.enter_custom(),
                        KeyCode::Char('r') => {
                            let jump = sh.jump.clone();
                            let preselect = sh.selected_node();
                            load_picker_jobs(sh, jump.as_deref(), preselect.as_deref());
                        }
                        KeyCode::Enter => {
                            if let Some(node) = sh.resolve_node() {
                                let name = sh.tunnel_name.clone();
                                let user = sh.user.clone();
                                submit_node(app, &name, &node, &user);
                                sheets.node_picker = None;
                                app.input_mode = InputMode::Normal;
                            }
                        }
                        _ => {}
                    }
                }
            }
            return;
        }

        InputMode::Sheet(SheetKind::HostDetail) => {
            if let Some(ref mut sh) = sheets.host_detail {
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => {
                        sheets.host_detail = None;
                        app.input_mode = InputMode::Normal;
                    }
                    // Show the live code. Read on demand, never on open: the
                    // daemon has to read the stored secret to derive it, and
                    // doing that for every host you merely LOOK at is what
                    // caused the credential-prompt storms on macOS.
                    KeyCode::Char('c') => {
                        sh.error.clear();
                        match client::rpc("host_totp", serde_json::json!({ "host": sh.host })) {
                            Ok(v) => {
                                sh.code = v["code"].as_str().unwrap_or_default().to_string();
                                sh.code_seconds_left =
                                    v["seconds_remaining"].as_u64().unwrap_or(0) as u32;
                            }
                            Err(e) => sh.error = format!("no code: {e}"),
                        }
                    }
                    // Test the STORED credentials — sending neither secret is
                    // what makes the daemon test what it has, so the TUI never
                    // handles them.
                    KeyCode::Char('t') if sh.busy.is_empty() => {
                        sh.busy = "testing login… (this runs a real ssh login)".into();
                        sh.test_result.clear();
                        sh.error.clear();
                        // Off the UI thread — see SheetResult. Guarded on an
                        // empty `busy` so holding `t` cannot stack logins and
                        // trip the server's failed-attempt rate limiting.
                        let host = sh.host.clone();
                        let tx = results.clone();
                        std::thread::spawn(move || {
                            let msg = match client::rpc(
                                "host_test_credentials",
                                serde_json::json!({ "host": host }),
                            ) {
                                Ok(v) => {
                                    let ok = v["ok"].as_bool().unwrap_or(false);
                                    let reason =
                                        v["reason"].as_str().unwrap_or_default().to_string();
                                    SheetResult::TestLogin {
                                        host,
                                        ok,
                                        message: if ok {
                                            "login succeeded with the stored credentials".into()
                                        } else if reason.is_empty() {
                                            "login failed".into()
                                        } else {
                                            reason
                                        },
                                    }
                                }
                                Err(e) => SheetResult::TestLogin {
                                    host,
                                    ok: false,
                                    message: format!("test could not run: {e}"),
                                },
                            };
                            let _ = tx.send(msg);
                        });
                    }
                    // Capital D, not d: this deletes credentials, so it must
                    // not share a key with the tunnel-pane delete.
                    KeyCode::Char('D') => {
                        sheets.confirm_delete = Some(ConfirmDeleteSheet::for_host(&sh.host));
                        app.input_mode = InputMode::Sheet(SheetKind::ConfirmDelete);
                    }
                    _ => {}
                }
            }
            return;
        }

        InputMode::Sheet(SheetKind::TunnelEdit) => {
            if let Some(ref mut sh) = sheets.tunnel_edit {
                match key.code {
                    KeyCode::Esc => {
                        sheets.tunnel_edit = None;
                        app.input_mode = InputMode::Normal;
                    }
                    KeyCode::Tab | KeyCode::Down => {
                        sh.field = (sh.field + 1) % TunnelEditSheet::FIELD_COUNT;
                    }
                    KeyCode::BackTab | KeyCode::Up => {
                        sh.field = (sh.field + TunnelEditSheet::FIELD_COUNT - 1)
                            % TunnelEditSheet::FIELD_COUNT;
                    }
                    KeyCode::Char(' ') if sh.field == 3 => {
                        sh.auto_start = !sh.auto_start;
                    }
                    KeyCode::Enter => {
                        if let Some((name, local, remote, auto)) = sh.validate() {
                            app.status_msg = apply_tunnel_edit(sh, name, local, remote, auto);
                            sheets.tunnel_edit = None;
                            app.input_mode = InputMode::Normal;
                            refresh_tunnels(app);
                        }
                    }
                    KeyCode::Backspace => {
                        if let Some(buf) = sh.focused_buf() {
                            buf.pop();
                        }
                        sh.error.clear();
                    }
                    KeyCode::Char(c) => {
                        if let Some(buf) = sh.focused_buf() {
                            buf.push(c);
                            sh.error.clear();
                        }
                    }
                    _ => {}
                }
            }
            return;
        }

        InputMode::Sheet(SheetKind::ConfirmDelete) => {
            if let Some(ref sh) = sheets.confirm_delete {
                match key.code {
                    KeyCode::Char('y') => {
                        let name = sh.name.clone();
                        match sh.target {
                            DeleteTarget::Tunnel => {
                                match client::rpc(
                                    "tunnel_remove",
                                    serde_json::json!({ "name": name }),
                                ) {
                                    Ok(_) => {
                                        app.status_msg = format!("deleted {name}");
                                        refresh_tunnels(app);
                                    }
                                    Err(e) => {
                                        app.status_msg = format!("delete failed: {e}");
                                    }
                                }
                            }
                            DeleteTarget::Host => {
                                match client::rpc(
                                    "host_remove",
                                    serde_json::json!({ "host": name }),
                                ) {
                                    Ok(v) => {
                                        // The daemon reports separately whether the
                                        // credentials could be deleted; a host can be
                                        // removed while its Keychain/vault entry
                                        // survives, and saying "removed" flatly would
                                        // hide a secret still sitting on disk.
                                        let creds_gone = v["credentials_deleted"]
                                            .as_bool()
                                            .unwrap_or(true);
                                        app.status_msg = if creds_gone {
                                            format!("removed {name} and its credentials")
                                        } else {
                                            format!(
                                                "removed {name}, but its saved credentials \
                                                 could NOT be deleted — remove them manually"
                                            )
                                        };
                                        refresh_hosts(app);
                                    }
                                    Err(e) => {
                                        app.status_msg = format!("remove failed: {e}");
                                    }
                                }
                                // The detail sheet is about a host that may no
                                // longer exist — close it rather than leave stale
                                // credentials on screen.
                                sheets.host_detail = None;
                            }
                        }
                        sheets.confirm_delete = None;
                        app.input_mode = InputMode::Normal;
                    }
                    KeyCode::Char('n') | KeyCode::Esc | KeyCode::Char('q') => {
                        sheets.confirm_delete = None;
                        app.input_mode = InputMode::Normal;
                    }
                    _ => {}
                }
            }
            return;
        }

        InputMode::Sheet(SheetKind::Help) => {
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?') => {
                    app.input_mode = InputMode::Normal;
                }
                _ => {}
            }
            return;
        }

        InputMode::Filter => {
            match key.code {
                KeyCode::Enter => app.commit_filter(),
                // Esc ABORTS (every other Esc in this file cancels) — it used
                // to commit, so escaping a junk partial filter applied it and
                // could hide every row.
                KeyCode::Esc => app.cancel_filter(),
                KeyCode::Backspace => {
                    app.filter_buf.pop();
                }
                KeyCode::Char(c) => {
                    app.filter_buf.push(c);
                }
                _ => {}
            }
            return;
        }

        InputMode::Normal => {}
    }

    // ----- Normal mode -----
    match key.code {
        // Quit
        KeyCode::Char('q') => {
            app.should_quit = true;
        }
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.should_quit = true;
        }

        // Navigation
        KeyCode::Down | KeyCode::Char('j') => app.move_down(),
        KeyCode::Up | KeyCode::Char('k') => app.move_up(),

        // Switch pane
        KeyCode::Tab => app.cycle_focus(),

        // Logs view (Rust-only addition, kept on `l`). Re-fetch on ENTRY —
        // there is no log event stream, so without this the pane showed the
        // tail as of TUI launch forever.
        KeyCode::Char('l') => {
            if app.focus == Pane::Logs {
                app.focus = Pane::Tunnels;
            } else {
                app.focus = Pane::Logs;
                refresh_logs(app);
            }
        }

        // Filter
        KeyCode::Char('/') => {
            app.filter_buf = app.filter.clone();
            app.input_mode = InputMode::Filter;
        }

        // Help (global)
        KeyCode::Char('?') => {
            app.input_mode = InputMode::Sheet(SheetKind::Help);
        }

        // New tunnel (global: `t` or Ctrl+n, parity with Python).
        KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            sheets.new_tunnel = Some(NewTunnelSheet::new());
            app.input_mode = InputMode::Sheet(SheetKind::NewTunnel);
        }
        KeyCode::Char('t') => {
            sheets.new_tunnel = Some(NewTunnelSheet::new());
            app.input_mode = InputMode::Sheet(SheetKind::NewTunnel);
        }

        // ----- Tunnel-pane actions -----
        // Explicit start/stop aliases (Rust-only convenience; Space toggles).
        KeyCode::Char('s') if app.focus == Pane::Tunnels => {
            if let Some(t) = app.selected_tunnel() {
                let name = t.name.clone();
                match client::rpc("tunnel_start", serde_json::json!({ "name": name })) {
                    Ok(_) => app.status_msg = format!("Started {name}"),
                    Err(e) => app.status_msg = format!("start failed: {e}"),
                }
                refresh_tunnels(app);
            }
        }
        KeyCode::Char('x') if app.focus == Pane::Tunnels => {
            if let Some(t) = app.selected_tunnel() {
                let name = t.name.clone();
                // Mark "user stopped" ONLY when the tunnel is currently alive
                // AND the stop RPC succeeded. The old unconditional pre-RPC
                // mark left a stale flag when aborting a starting/failed
                // tunnel — a LATER genuine drop then consumed it and the real
                // outage was reported as a quiet "stopped".
                let was_alive = t.status.to_string() == "alive";
                match client::rpc("tunnel_stop", serde_json::json!({ "name": name })) {
                    Ok(_) => {
                        if was_alive {
                            app.mark_user_stopped(&name);
                        }
                        app.status_msg = format!("Stopped {name}");
                    }
                    Err(e) => app.status_msg = format!("stop failed: {e}"),
                }
                refresh_tunnels(app);
            }
        }
        // Space toggles a tunnel (start/stop); Enter opens the node picker.
        KeyCode::Char(' ') if app.focus == Pane::Tunnels => {
            if let Some(t) = app.selected_tunnel() {
                let name = t.name.clone();
                let was_alive = t.status.to_string() == "alive";
                match client::rpc("tunnel_toggle", serde_json::json!({ "name": name })) {
                    Ok(_) => {
                        // Alive + toggle succeeded = an intentional stop.
                        if was_alive {
                            app.mark_user_stopped(&name);
                        }
                        app.status_msg = format!("Toggled {name}");
                    }
                    Err(e) => app.status_msg = format!("toggle failed: {e}"),
                }
                refresh_tunnels(app);
            }
        }
        KeyCode::Enter if app.focus == Pane::Tunnels => {
            open_node_picker(app, sheets);
        }
        KeyCode::Char('d') if app.focus == Pane::Tunnels => {
            if let Some(t) = app.selected_tunnel() {
                sheets.confirm_delete = Some(ConfirmDeleteSheet::new(&t.name));
                app.input_mode = InputMode::Sheet(SheetKind::ConfirmDelete);
            }
        }
        KeyCode::Char('y') if app.focus == Pane::Tunnels => {
            if let Some(t) = app.selected_tunnel() {
                let url = tunnel_url(t.local_port, t.url_path.as_deref());
                notify::copy_to_clipboard(&url);
                app.status_msg = format!("copied {url}");
            }
        }

        // ----- Host-pane actions -----
        KeyCode::Char(' ') if app.focus == Pane::Hosts => {
            if let Some(h) = app.selected_host() {
                let name = h.host.clone();
                match client::rpc("host_toggle", serde_json::json!({ "host": name })) {
                    Ok(_) => app.status_msg = format!("Toggled {name}"),
                    Err(e) => app.status_msg = format!("host_toggle failed: {e}"),
                }
                refresh_hosts(app);
            }
        }
        KeyCode::Char('m') if app.focus == Pane::Hosts => {
            if let Some(h) = app.selected_host() {
                let name = h.host.clone();
                match client::rpc("host_mount_toggle", serde_json::json!({ "host": name })) {
                    Ok(_) => app.status_msg = format!("Toggled mount for {name}"),
                    Err(e) => app.status_msg = format!("mount failed: {e}"),
                }
                refresh_hosts(app);
            }
        }
        KeyCode::Char('r') if app.focus == Pane::Hosts => {
            if let Some(h) = app.selected_host() {
                let name = h.host.clone();
                match client::rpc("host_rotate", serde_json::json!({ "host": name })) {
                    Ok(_) => app.status_msg = format!("Rotated {name}"),
                    Err(e) => app.status_msg = format!("rotate failed: {e}"),
                }
                refresh_hosts(app);
            }
        }
        KeyCode::Char('a') if app.focus == Pane::Hosts => {
            sheets.add_host = Some(AddHostSheet::new());
            app.input_mode = InputMode::Sheet(SheetKind::AddHost);
        }
        // Enter on a HOST opens its details. On a tunnel, Enter already means
        // "pick a compute node" (handled above), so the panes stay distinct.
        KeyCode::Enter if app.focus == Pane::Hosts => {
            open_host_detail(app, sheets);
        }
        KeyCode::Char('e') if app.focus == Pane::Tunnels => {
            open_tunnel_edit(app, sheets);
        }

        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Node-picker helpers
// ---------------------------------------------------------------------------

/// Choose the jump host to use for node discovery for the given tunnel.
///
/// Prefers a master-ready host that is also in the tunnel's `jump_candidates`
/// (if it has any); otherwise the first master-ready host; otherwise the
/// tunnel's active_jump as a last resort.
fn pick_jump_for_tunnel(app: &AppModel, name: &str) -> Option<String> {
    let t = app.tunnels.iter().find(|t| t.name == name)?;

    // First, a master-ready host matching the candidate constraint.
    let ready = app.hosts.iter().filter(|h| h.is_master_ready);
    if let Some(cands) = &t.jump_candidates {
        if let Some(h) = ready.clone().find(|h| cands.contains(&h.host)) {
            return Some(h.host.clone());
        }
    }
    if let Some(h) = app.hosts.iter().find(|h| h.is_master_ready) {
        return Some(h.host.clone());
    }
    t.active_jump.clone()
}

/// Fold a finished worker result into the sheet that asked for it.
///
/// The sheet may have been closed, or reopened on a DIFFERENT host, while the
/// login was running — matching on the host name is what stops a slow result
/// from landing on the wrong one.
fn apply_sheet_result(res: SheetResult, sheets: &mut Sheets) {
    match res {
        SheetResult::TestLogin { host, ok, message } => {
            if let Some(sh) = sheets.host_detail.as_mut().filter(|s| s.host == host) {
                sh.busy.clear();
                sh.test_ok = ok;
                sh.test_result = message;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Host-detail helpers
// ---------------------------------------------------------------------------

/// Open the host-detail sheet for the focused host and load what is stored.
fn open_host_detail(app: &mut AppModel, sheets: &mut Sheets) {
    let Some(h) = app.selected_host() else { return };
    let mut sh = HostDetailSheet::new(&h.host);
    load_host_credentials(&mut sh);
    sheets.host_detail = Some(sh);
    app.input_mode = InputMode::Sheet(SheetKind::HostDetail);
}

/// Fill the sheet from `host_credentials` — metadata only, never the secrets.
fn load_host_credentials(sh: &mut HostDetailSheet) {
    match client::rpc("host_credentials", serde_json::json!({ "host": sh.host })) {
        Ok(v) => {
            sh.loaded = true;
            sh.error.clear();
            sh.has_password = v["has_password"].as_bool().unwrap_or(false);
            sh.password_length = v["password_length"].as_u64().unwrap_or(0) as usize;
            sh.has_otp_secret = v["has_otp_secret"].as_bool().unwrap_or(false);
            sh.auto_connect = v["auto_connect"].as_bool().unwrap_or(false);
            sh.otp_error = v["otp_error"].as_str().unwrap_or_default().to_string();
            // Describe the secret the way the macOS view does: issuer/account
            // first, then any non-default TOTP parameters worth knowing about.
            let otp = &v["otp"];
            let mut parts: Vec<String> = Vec::new();
            for k in ["issuer", "account"] {
                if let Some(s) = otp[k].as_str().filter(|s| !s.is_empty()) {
                    parts.push(s.to_string());
                }
            }
            let mut summary = parts.join(" · ");
            let algo = otp["algorithm"].as_str().unwrap_or("SHA1");
            let digits = otp["digits"].as_u64().unwrap_or(6);
            let period = otp["period"].as_u64().unwrap_or(30);
            if algo != "SHA1" || digits != 6 || period != 30 {
                let extra = format!("{algo}/{digits}/{period}s");
                summary = if summary.is_empty() {
                    extra
                } else {
                    format!("{summary} ({extra})")
                };
            }
            sh.otp_summary = summary;
        }
        Err(e) => {
            sh.loaded = true;
            sh.error = format!("could not read stored credentials: {e}");
        }
    }
}

/// Open the tunnel-edit sheet for the focused tunnel.
fn open_tunnel_edit(app: &mut AppModel, sheets: &mut Sheets) {
    let Some(t) = app.selected_tunnel() else { return };
    sheets.tunnel_edit = Some(TunnelEditSheet::new(
        &t.name,
        t.local_port,
        t.remote_port,
        t.auto_start,
    ));
    app.input_mode = InputMode::Sheet(SheetKind::TunnelEdit);
}

/// Apply an edited tunnel: rename, ports, auto-start. Returns a status line.
///
/// Each change is a separate daemon call, so they are applied in an order that
/// cannot strand the others: RENAME LAST. The port/auto-start calls address
/// the tunnel by its current name, and renaming first would make every one of
/// them miss.
fn apply_tunnel_edit(sh: &TunnelEditSheet, name: String, local: u16, remote: u16, auto: bool) -> String {
    let target = sh.original_name.clone();

    if let Err(e) = client::rpc(
        "tunnel_set_ports",
        serde_json::json!({ "name": target, "local_port": local, "remote_port": remote }),
    ) {
        return format!("ports unchanged: {e}");
    }
    if auto != sh.auto_start {
        // NOTE: the daemon names this flag `value`, not `auto_start`; sending
        // the wrong key would be read as "absent" and silently default to
        // false, i.e. the toggle would appear to work and change nothing.
        if let Err(e) = client::rpc(
            "tunnel_set_autostart",
            serde_json::json!({ "name": target, "value": auto }),
        ) {
            return format!("ports saved, auto-start unchanged: {e}");
        }
    }
    if name != target {
        if let Err(e) = client::rpc(
            "tunnel_rename",
            serde_json::json!({ "old": target, "new": name }),
        ) {
            return format!("saved, but rename failed: {e}");
        }
        return format!("Saved and renamed to {name}");
    }
    format!("Saved {target}")
}

/// Open the squeue-backed node picker for the focused tunnel.
fn open_node_picker(app: &mut AppModel, sheets: &mut Sheets) {
    let (name, user, preselect) = match app.selected_tunnel() {
        Some(t) => (
            t.name.clone(),
            t.last_user
                .clone()
                .unwrap_or_else(|| std::env::var("USER").unwrap_or_default()),
            t.last_node.clone(),
        ),
        None => return,
    };

    let jump = pick_jump_for_tunnel(app, &name);
    let mut sh = NodePickerSheet::new(&name, jump.clone(), user);

    if jump.is_none() {
        sh.error =
            "no connected jump host — press c for custom, or start a host first".to_string();
    } else {
        load_picker_jobs(&mut sh, jump.as_deref(), preselect.as_deref());
    }

    sheets.node_picker = Some(sh);
    app.input_mode = InputMode::Sheet(SheetKind::NodePicker);
}

/// Fetch squeue jobs for `jump` and load them into the picker sheet.
/// On error or empty, sets an informative message instead of crashing.
fn load_picker_jobs(sh: &mut NodePickerSheet, jump: Option<&str>, preselect: Option<&str>) {
    let host = match jump {
        Some(h) => h,
        None => {
            sh.error =
                "no connected jump host — press c for custom, r to retry".to_string();
            sh.jobs.clear();
            return;
        }
    };
    match client::rpc("discover_nodes", serde_json::json!({ "host": host })) {
        Ok(v) => {
            let jobs = parse_squeue_jobs(&v);
            sh.set_jobs(jobs, preselect);
        }
        Err(e) => {
            sh.jobs.clear();
            sh.sel = 0;
            sh.error = format!("squeue failed: {e} — press c for custom, r to retry");
        }
    }
}

/// Parse the `discover_nodes` JSON array into `SqueueJob`s.
fn parse_squeue_jobs(v: &Value) -> Vec<SqueueJob> {
    let arr = match v.as_array() {
        Some(a) => a,
        None => return Vec::new(),
    };
    arr.iter()
        .map(|j| SqueueJob {
            jobid: j.get("jobid").and_then(Value::as_str).unwrap_or("").to_string(),
            partition: j
                .get("partition")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            name: j.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
            state: j.get("state").and_then(Value::as_str).unwrap_or("").to_string(),
            time: j.get("time").and_then(Value::as_str).unwrap_or("").to_string(),
            node: j.get("node").and_then(Value::as_str).unwrap_or("").to_string(),
        })
        .collect()
}

/// Submit the chosen node via `tunnel_set_node` and refresh.
fn submit_node(app: &mut AppModel, name: &str, node: &str, user: &str) {
    let res = client::rpc(
        "tunnel_set_node",
        serde_json::json!({ "name": name, "node": node, "user": user }),
    );
    match res {
        Ok(_) => {
            app.status_msg = format!("Set node {node} for {name}");
            refresh_tunnels(app);
        }
        Err(e) => {
            app.status_msg = format!("tunnel_set_node failed: {e}");
        }
    }
}

// ---------------------------------------------------------------------------
// IPC refresh helpers
// ---------------------------------------------------------------------------

fn refresh_tunnels(app: &mut AppModel) {
    if let Ok(v) = client::rpc("list_tunnels", serde_json::json!({})) {
        if let Ok(tunnels) = serde_json::from_value(v) {
            let natives = app.set_tunnels(tunnels);
            for (title, msg) in natives {
                notify::system_notify(&title, &msg);
            }
        }
    }
}

fn refresh_hosts(app: &mut AppModel) {
    if let Ok(v) = client::rpc("list_hosts", serde_json::json!({})) {
        if let Ok(hosts) = serde_json::from_value(v) {
            app.set_hosts(hosts);
        }
    }
}

/// Re-fetch the daemon log tail and REPLACE the buffer (best-effort).
/// (Param key is "lines" — the daemon ignores unknown keys, so the old "n"
/// silently fell back to the default.)
fn refresh_logs(app: &mut AppModel) {
    if let Ok(v) = client::rpc("log_tail", serde_json::json!({ "lines": 200 })) {
        if let Some(lines) = v.get("lines").and_then(Value::as_array) {
            let ls: Vec<String> = lines
                .iter()
                .filter_map(Value::as_str)
                .map(String::from)
                .collect();
            app.set_logs(ls);
        }
    }
}

// ---------------------------------------------------------------------------
// Daemon event application
// ---------------------------------------------------------------------------

/// Flags accumulated over one channel drain, applied as at most ONE refresh
/// per list. The daemon emits (proto/event.rs) tunnel_status_changed /
/// host_status_changed / notification; the subscriber thread adds the
/// synthetic __subscribed / __events_disconnected markers.
#[derive(Default)]
struct EventFlags {
    tunnels: bool,
    hosts: bool,
    subscribed: bool,
    disconnected: bool,
}

impl EventFlags {
    fn collect(&mut self, event: &Value) {
        match event.get("event").and_then(Value::as_str).unwrap_or("") {
            "tunnel_status_changed" => self.tunnels = true,
            "host_status_changed" => self.hosts = true,
            // (Re)connected: every event during the gap was missed — both
            // lists may be arbitrarily stale.
            "__subscribed" => self.subscribed = true,
            "__events_disconnected" => self.disconnected = true,
            _ => {}
        }
    }

    fn any(&self) -> bool {
        self.tunnels || self.hosts || self.subscribed || self.disconnected
    }

    fn apply(self, app: &mut AppModel) {
        if self.tunnels || self.subscribed {
            refresh_tunnels(app);
        }
        if self.hosts || self.subscribed {
            refresh_hosts(app);
        }
        if self.disconnected && !self.subscribed {
            app.status_msg =
                "live updates disconnected — reconnecting… (data may be stale)".to_string();
        }
    }
}

// ---------------------------------------------------------------------------
// Renderer
// ---------------------------------------------------------------------------

fn render_frame(
    f: &mut ratatui::Frame,
    app: &AppModel,
    sheets: &Sheets,
) {
    let size = f.area();

    // Top-level layout: main area + status bar.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(5),    // main
            Constraint::Length(1), // status bar
        ])
        .split(size);

    let main_area = chunks[0];
    let status_area = chunks[1];

    // Split main area: hosts (upper) + tunnels (lower) or logs (full).
    if app.focus == Pane::Logs {
        render_logs(f, main_area, app);
    } else {
        let split = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
            .split(main_area);
        render_hosts(f, split[0], app);
        render_tunnels(f, split[1], app);
    }

    // Status bar.
    render_status_bar(f, status_area, app);

    // Modal overlays (drawn last so they appear on top).
    match app.input_mode {
        InputMode::Sheet(SheetKind::AddHost) => {
            if let Some(ref sh) = sheets.add_host {
                render_add_host(f, sh);
            }
        }
        InputMode::Sheet(SheetKind::HostDetail) => {
            if let Some(ref sh) = sheets.host_detail {
                render_host_detail(f, sh);
            }
        }
        InputMode::Sheet(SheetKind::NewTunnel) => {
            if let Some(ref sh) = sheets.new_tunnel {
                render_new_tunnel(f, sh);
            }
        }
        InputMode::Sheet(SheetKind::TunnelEdit) => {
            if let Some(ref sh) = sheets.tunnel_edit {
                render_tunnel_edit(f, sh);
            }
        }
        InputMode::Sheet(SheetKind::NodePicker) => {
            if let Some(ref sh) = sheets.node_picker {
                render_node_picker(f, sh);
            }
        }
        InputMode::Sheet(SheetKind::ConfirmDelete) => {
            if let Some(ref sh) = sheets.confirm_delete {
                render_confirm_delete(f, sh);
            }
        }
        InputMode::Sheet(SheetKind::Help) => {
            render_help(f);
        }
        InputMode::Filter => {
            render_filter_bar(f, status_area, app);
        }
        InputMode::Normal => {}
    }
}

fn render_status_bar(f: &mut ratatui::Frame, area: ratatui::layout::Rect, app: &AppModel) {
    let help =
        " q:quit  t:new  ?:help  Tab:switch  Spc:toggle  Enter:node  y:yank  d:del  m:mount  r:rot  l:logs  /:filter";

    // An active toast is shown with its severity color; otherwise the help line.
    if let Some(toast) = &app.toast {
        let para = Paragraph::new(format!(" {}", toast.text))
            .style(Style::default().fg(toast.severity.color()))
            .block(Block::default());
        f.render_widget(para, area);
        return;
    }

    let msg = if app.status_msg.is_empty() {
        help.to_string()
    } else {
        format!("{} │ {}", app.status_msg, help)
    };

    let para = Paragraph::new(msg.as_str())
        .style(Style::default().fg(Color::DarkGray))
        .block(Block::default());
    f.render_widget(para, area);
}

fn render_filter_bar(f: &mut ratatui::Frame, area: ratatui::layout::Rect, app: &AppModel) {
    let text = format!("Filter: {}█", app.filter_buf);
    let para = Paragraph::new(text.as_str())
        .style(Style::default().fg(Color::Yellow))
        .block(Block::default().borders(Borders::NONE));
    f.render_widget(para, area);
}
