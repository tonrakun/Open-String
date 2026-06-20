//! 4.3's TUI: initial setup wizard, a settings screen (permission level,
//! Extension management, auth management), and a dashboard (sessions,
//! workspace status, token consumption, health check results), plus a
//! shared real-time operation log tail and dangerous-operation
//! confirmation dialog rendering. Built on `ratatui`/`crossterm` rather
//! than a heavier toolkit to keep the single-binary footprint small (6.2).

use crate::auth::{AnthropicApiKeyProvider, AuthProvider, validate_api_key_format};
use crate::dashboard::{self, DashboardSnapshot, PendingAction};
use crate::permission::PermissionLevel;
use crate::session::{FileWorkspaceRegistry, WorkspaceRegistry};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, List, ListItem, Paragraph};
use std::io::Stdout;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const PERMISSION_LEVELS: [PermissionLevel; 4] = [
    PermissionLevel::GodMode,
    PermissionLevel::LowSecurity,
    PermissionLevel::MiddlePermission,
    PermissionLevel::HighProtect,
];

const SNAPSHOT_REFRESH_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Screen {
    Setup,
    Dashboard,
    Settings,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SetupStep {
    ApiKey,
    PermissionLevel,
    Workspace,
    Done,
}

struct SetupState {
    step: SetupStep,
    api_key_input: String,
    level_index: usize,
    workspace_path_input: String,
    workspace_name_input: String,
    editing_workspace_name: bool,
}

impl Default for SetupState {
    fn default() -> Self {
        Self {
            step: SetupStep::ApiKey,
            api_key_input: String::new(),
            level_index: PERMISSION_LEVELS
                .iter()
                .position(|l| *l == PermissionLevel::HighProtect)
                .unwrap_or(0),
            workspace_path_input: String::new(),
            workspace_name_input: String::new(),
            editing_workspace_name: false,
        }
    }
}

struct ConfirmDialog {
    summary: String,
    action: dashboard::PendingAction,
}

struct App {
    screen: Screen,
    workspace: Option<PathBuf>,
    snapshot: DashboardSnapshot,
    last_refresh: Instant,
    setup: SetupState,
    selected_extension: usize,
    confirm: Option<ConfirmDialog>,
    status: String,
    should_quit: bool,
}

impl App {
    fn new(workspace: Option<PathBuf>) -> Self {
        let snapshot = dashboard::gather(workspace.as_deref());
        let screen = if snapshot.auth_configured {
            Screen::Dashboard
        } else {
            Screen::Setup
        };
        Self {
            screen,
            workspace,
            snapshot,
            last_refresh: Instant::now(),
            setup: SetupState::default(),
            selected_extension: 0,
            confirm: None,
            status: String::new(),
            should_quit: false,
        }
    }

    fn refresh(&mut self) {
        self.snapshot = dashboard::gather(self.workspace.as_deref());
        self.last_refresh = Instant::now();
    }

    fn maybe_refresh(&mut self) {
        if self.last_refresh.elapsed() >= SNAPSHOT_REFRESH_INTERVAL {
            self.refresh();
        }
    }

    /// Stages `action` behind a confirmation dialog when the current
    /// permission level would require one for `operation` (reusing the
    /// exact same `classify_danger`/`PermissionLevel::decide` gate every
    /// other Open String surface goes through), applying it immediately
    /// otherwise.
    fn perform(
        &mut self,
        operation: &str,
        action: dashboard::PendingAction,
        summary: impl Into<String>,
    ) {
        if dashboard::requires_confirmation(self.snapshot.permission_level, operation) {
            self.confirm = Some(ConfirmDialog {
                summary: summary.into(),
                action,
            });
        } else {
            self.apply(action);
        }
    }

    fn apply(&mut self, action: dashboard::PendingAction) {
        self.status = dashboard::apply_pending_action(self.workspace.as_deref(), action);
        self.refresh();
    }
}

/// Restores the terminal on drop so an early return (or, with `?`
/// propagating out of `run`, an error) never leaves the user's shell stuck
/// in raw/alternate-screen mode.
struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalGuard {
    fn new() -> Result<Self, String> {
        enable_raw_mode().map_err(|e| e.to_string())?;
        let mut stdout = std::io::stdout();
        crossterm::execute!(stdout, EnterAlternateScreen).map_err(|e| e.to_string())?;
        let terminal = Terminal::new(CrosstermBackend::new(stdout)).map_err(|e| e.to_string())?;
        Ok(Self { terminal })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = crossterm::execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
    }
}

pub fn run(workspace: Option<&Path>) -> Result<(), String> {
    let mut guard = TerminalGuard::new()?;
    let mut app = App::new(workspace.map(Path::to_path_buf));

    while !app.should_quit {
        guard
            .terminal
            .draw(|frame| draw(frame, &app))
            .map_err(|e| e.to_string())?;

        if event::poll(Duration::from_millis(250)).map_err(|e| e.to_string())?
            && let Event::Key(key) = event::read().map_err(|e| e.to_string())?
            && key.kind == KeyEventKind::Press
        {
            handle_key(&mut app, key.code);
        }
        app.maybe_refresh();
    }
    Ok(())
}

fn handle_key(app: &mut App, key: KeyCode) {
    if let Some(dialog) = app.confirm.take() {
        match key {
            KeyCode::Char('y') | KeyCode::Char('Y') => app.apply(dialog.action),
            _ => app.status = "Declined.".to_string(),
        }
        return;
    }

    if key == KeyCode::Char('q') && app.screen != Screen::Setup {
        app.should_quit = true;
        return;
    }

    match app.screen {
        Screen::Setup => handle_setup_key(app, key),
        Screen::Dashboard => match key {
            KeyCode::Char('2') => app.screen = Screen::Settings,
            KeyCode::Char('r') => app.refresh(),
            _ => {}
        },
        Screen::Settings => handle_settings_key(app, key),
    }

    if matches!(app.screen, Screen::Dashboard | Screen::Settings) {
        match key {
            KeyCode::Char('1') => app.screen = Screen::Dashboard,
            KeyCode::Char('2') => app.screen = Screen::Settings,
            _ => {}
        }
    }
}

fn handle_setup_key(app: &mut App, key: KeyCode) {
    let setup = &mut app.setup;
    match setup.step {
        SetupStep::ApiKey => match key {
            KeyCode::Enter => {
                if validate_api_key_format(&setup.api_key_input) {
                    match AnthropicApiKeyProvider::new().store(&setup.api_key_input) {
                        Ok(()) => {
                            setup.api_key_input.clear();
                            setup.step = SetupStep::PermissionLevel;
                            app.status.clear();
                        }
                        Err(e) => app.status = format!("Failed to store API key: {e}"),
                    }
                } else {
                    app.status = "That doesn't look like a valid Anthropic API key.".to_string();
                }
            }
            KeyCode::Backspace => {
                setup.api_key_input.pop();
            }
            KeyCode::Char(c) => setup.api_key_input.push(c),
            _ => {}
        },
        SetupStep::PermissionLevel => match key {
            KeyCode::Up => {
                setup.level_index = setup.level_index.saturating_sub(1);
            }
            KeyCode::Down => {
                setup.level_index = (setup.level_index + 1).min(PERMISSION_LEVELS.len() - 1);
            }
            KeyCode::Enter => {
                let level = PERMISSION_LEVELS[setup.level_index];
                match crate::permission_store_for(app.workspace.as_deref()).and_then(|s| {
                    s.set(level)
                        .map_err(|e| format!("failed to set permission level: {e}"))
                }) {
                    Ok(()) => {
                        setup.step = SetupStep::Workspace;
                        app.status.clear();
                    }
                    Err(e) => app.status = e,
                }
            }
            _ => {}
        },
        SetupStep::Workspace => match key {
            KeyCode::Tab => setup.editing_workspace_name = !setup.editing_workspace_name,
            KeyCode::Backspace => {
                if setup.editing_workspace_name {
                    setup.workspace_name_input.pop();
                } else {
                    setup.workspace_path_input.pop();
                }
            }
            KeyCode::Char(c) => {
                if setup.editing_workspace_name {
                    setup.workspace_name_input.push(c);
                } else {
                    setup.workspace_path_input.push(c);
                }
            }
            KeyCode::Enter => {
                if setup.workspace_path_input.trim().is_empty() {
                    setup.step = SetupStep::Done;
                    app.refresh();
                    return;
                }
                let path = PathBuf::from(setup.workspace_path_input.trim());
                let name = (!setup.workspace_name_input.trim().is_empty())
                    .then(|| setup.workspace_name_input.trim().to_string());
                match FileWorkspaceRegistry::new().and_then(|r| r.create(&path, name)) {
                    Ok(_) => {
                        app.workspace = Some(path);
                        setup.step = SetupStep::Done;
                        app.refresh();
                    }
                    Err(e) => app.status = format!("Failed to create workspace: {e}"),
                }
            }
            KeyCode::Esc => {
                setup.step = SetupStep::Done;
                app.refresh();
            }
            _ => {}
        },
        SetupStep::Done => {
            app.screen = Screen::Dashboard;
        }
    }
}

fn handle_settings_key(app: &mut App, key: KeyCode) {
    let extension_count = app.snapshot.extensions.len();
    match key {
        KeyCode::Up if extension_count > 0 => {
            app.selected_extension = app.selected_extension.saturating_sub(1);
        }
        KeyCode::Down if extension_count > 0 => {
            app.selected_extension = (app.selected_extension + 1).min(extension_count - 1);
        }
        KeyCode::Char('p') => {
            let current = app.snapshot.permission_level;
            let idx = PERMISSION_LEVELS
                .iter()
                .position(|l| *l == current)
                .unwrap_or(0);
            let next = PERMISSION_LEVELS[(idx + 1) % PERMISSION_LEVELS.len()];
            app.perform(
                &format!("permission set {next}"),
                PendingAction::SetPermissionLevel(next),
                format!("Change permission level to {next}?"),
            );
        }
        KeyCode::Char('e') if extension_count > 0 => {
            let ext = &app.snapshot.extensions[app.selected_extension];
            let (name, enabled) = (ext.name.clone(), !ext.enabled);
            app.perform(
                &format!("toggle extension {name}"),
                PendingAction::SetExtensionEnabled {
                    name: name.clone(),
                    enabled,
                },
                format!(
                    "{} extension \"{name}\"?",
                    if enabled { "Enable" } else { "Disable" }
                ),
            );
        }
        KeyCode::Char('x') if extension_count > 0 => {
            let name = app.snapshot.extensions[app.selected_extension].name.clone();
            app.perform(
                &format!("delete extension {name}"),
                PendingAction::RemoveExtension(name.clone()),
                format!("Remove extension \"{name}\"? This cannot be undone."),
            );
        }
        KeyCode::Char('l') => {
            app.perform(
                "logout",
                PendingAction::Logout,
                "Log out (remove the stored Anthropic API key)?".to_string(),
            );
        }
        _ => {}
    }
}

fn draw(frame: &mut ratatui::Frame, app: &App) {
    match app.screen {
        Screen::Setup => draw_setup(frame, app),
        Screen::Dashboard => draw_dashboard(frame, app),
        Screen::Settings => draw_settings(frame, app),
    }
    if let Some(dialog) = &app.confirm {
        draw_confirm_dialog(frame, dialog);
    }
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

fn draw_confirm_dialog(frame: &mut ratatui::Frame, dialog: &ConfirmDialog) {
    let area = centered_rect(60, 30, frame.area());
    let block = Block::default()
        .title(" Confirm dangerous operation ")
        .borders(Borders::ALL)
        .style(Style::default().fg(Color::Yellow));
    let text = vec![
        Line::from(dialog.summary.clone()),
        Line::from(""),
        Line::from(vec![
            Span::styled("y", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" = confirm   "),
            Span::styled(
                "any other key",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(" = decline"),
        ]),
    ];
    frame.render_widget(Paragraph::new(text).block(block), area);
}

fn draw_setup(frame: &mut ratatui::Frame, app: &App) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(2)])
        .split(area);

    let body = match app.setup.step {
        SetupStep::ApiKey => Paragraph::new(vec![
            Line::from("Open String setup -- step 1/3: Anthropic API key"),
            Line::from(""),
            Line::from(format!("> {}", "*".repeat(app.setup.api_key_input.len()))),
            Line::from(""),
            Line::from("Paste your API key and press Enter."),
        ]),
        SetupStep::PermissionLevel => {
            let lines: Vec<Line> = PERMISSION_LEVELS
                .iter()
                .enumerate()
                .map(|(i, level)| {
                    let marker = if i == app.setup.level_index {
                        "> "
                    } else {
                        "  "
                    };
                    Line::from(format!("{marker}{level}"))
                })
                .collect();
            let mut text = vec![
                Line::from("Open String setup -- step 2/3: default permission level"),
                Line::from(""),
            ];
            text.extend(lines);
            text.push(Line::from(""));
            text.push(Line::from("Up/Down to choose, Enter to confirm."));
            Paragraph::new(text)
        }
        SetupStep::Workspace => Paragraph::new(vec![
            Line::from("Open String setup -- step 3/3: workspace (optional)"),
            Line::from(""),
            Line::from(format!("path: {}", app.setup.workspace_path_input)),
            Line::from(format!("name: {}", app.setup.workspace_name_input)),
            Line::from(""),
            Line::from(
                "Tab to switch field, Enter to create (or leave path empty to skip), Esc to skip.",
            ),
        ]),
        SetupStep::Done => Paragraph::new(vec![
            Line::from("Setup complete."),
            Line::from(""),
            Line::from("Press any key to open the dashboard."),
        ]),
    };
    frame.render_widget(
        body.block(Block::default().borders(Borders::ALL)),
        chunks[0],
    );
    frame.render_widget(Paragraph::new(app.status.clone()), chunks[1]);
}

fn severity_color(severity: crate::health::Severity) -> Color {
    match severity {
        crate::health::Severity::Fatal => Color::Red,
        crate::health::Severity::Warning => Color::Yellow,
        crate::health::Severity::Info => Color::Green,
    }
}

fn draw_dashboard(frame: &mut ratatui::Frame, app: &App) {
    let area = frame.area();
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(8),
            Constraint::Length(2),
        ])
        .split(area);

    frame.render_widget(tab_bar(Screen::Dashboard), rows[0]);

    let usage = app
        .snapshot
        .token_usage
        .as_ref()
        .map(|u| (u.percent(), format!("{}/{} tokens", u.used, u.window)))
        .unwrap_or((0, "no active session".to_string()));
    let gauge = Gauge::default()
        .block(
            Block::default()
                .title("Token consumption")
                .borders(Borders::ALL),
        )
        .gauge_style(Style::default().fg(Color::Cyan))
        .percent(u16::from(usage.0))
        .label(usage.1);
    frame.render_widget(gauge, rows[1]);

    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(34),
            Constraint::Percentage(33),
            Constraint::Percentage(33),
        ])
        .split(rows[2]);

    let sessions: Vec<ListItem> = app
        .snapshot
        .sessions
        .iter()
        .map(|s| {
            let state = if s.is_active() { "active" } else { "ended" };
            ListItem::new(format!(
                "#{} {} [{}]",
                s.id,
                s.label.clone().unwrap_or_default(),
                state
            ))
        })
        .collect();
    frame.render_widget(
        List::new(sessions).block(Block::default().title("Sessions").borders(Borders::ALL)),
        columns[0],
    );

    let mut workspace_items: Vec<ListItem> = app
        .snapshot
        .workspaces
        .iter()
        .map(|w| {
            let marker = app
                .snapshot
                .current_workspace
                .as_ref()
                .filter(|cur| cur.path == w.path)
                .map(|_| "* ")
                .unwrap_or("  ");
            ListItem::new(format!("{marker}{} ({})", w.name, w.path.display()))
        })
        .collect();
    if workspace_items.is_empty() {
        workspace_items.push(ListItem::new("(no workspaces registered)"));
    }
    frame.render_widget(
        List::new(workspace_items)
            .block(Block::default().title("Workspaces").borders(Borders::ALL)),
        columns[1],
    );

    let health_items: Vec<ListItem> = app
        .snapshot
        .health
        .items
        .iter()
        .map(|item| {
            ListItem::new(Line::from(Span::styled(
                format!(
                    "[{}] {}: {}",
                    severity_label(item.severity),
                    item.name,
                    item.message
                ),
                Style::default().fg(severity_color(item.severity)),
            )))
        })
        .collect();
    frame.render_widget(
        List::new(health_items).block(Block::default().title("Health check").borders(Borders::ALL)),
        columns[2],
    );

    let log_items: Vec<ListItem> = app
        .snapshot
        .recent_audit_log
        .iter()
        .rev()
        .map(|line| ListItem::new(line.clone()))
        .collect();
    frame.render_widget(
        List::new(log_items).block(
            Block::default()
                .title("Operation log (live)")
                .borders(Borders::ALL),
        ),
        rows[3],
    );

    frame.render_widget(
        Paragraph::new("1: dashboard  2: settings  r: refresh now  q: quit"),
        rows[4],
    );
}

fn severity_label(severity: crate::health::Severity) -> &'static str {
    match severity {
        crate::health::Severity::Fatal => "FATAL",
        crate::health::Severity::Warning => "WARN",
        crate::health::Severity::Info => "INFO",
    }
}

fn tab_bar(active: Screen) -> Paragraph<'static> {
    let mk = |label: &str, is_active: bool| {
        if is_active {
            Span::styled(
                format!(" {label} "),
                Style::default().add_modifier(Modifier::REVERSED),
            )
        } else {
            Span::raw(format!(" {label} "))
        }
    };
    Paragraph::new(Line::from(vec![
        mk("1:Dashboard", active == Screen::Dashboard),
        mk("2:Settings", active == Screen::Settings),
    ]))
}

fn draw_settings(frame: &mut ratatui::Frame, app: &App) {
    let area = frame.area();
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(2),
        ])
        .split(area);

    frame.render_widget(tab_bar(Screen::Settings), rows[0]);

    let auth_line = if app.snapshot.auth_configured {
        "Anthropic API key: configured"
    } else {
        "Anthropic API key: not configured"
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(format!(
                "Permission level: {}  (press p to cycle)",
                app.snapshot.permission_level
            )),
            Line::from(format!("{auth_line}  (press l to log out)")),
        ])
        .block(Block::default().title("Account").borders(Borders::ALL)),
        rows[1],
    );

    let settings_columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
        .split(rows[2]);

    let items: Vec<ListItem> = app
        .snapshot
        .extensions
        .iter()
        .enumerate()
        .map(|(i, ext)| {
            let marker = if i == app.selected_extension {
                "> "
            } else {
                "  "
            };
            let state = if ext.enabled { "enabled" } else { "disabled" };
            let requirement = ext
                .required_permission_level
                .map(|level| format!(", requires {level}"))
                .unwrap_or_default();
            ListItem::new(format!(
                "{marker}{} [{state}]: {} {}{requirement}",
                ext.name,
                ext.command,
                ext.args.join(" ")
            ))
        })
        .collect();
    frame.render_widget(
        List::new(items).block(
            Block::default()
                .title("Extensions (e/x/Up/Down)")
                .borders(Borders::ALL),
        ),
        settings_columns[0],
    );

    let skill_items: Vec<ListItem> = app
        .snapshot
        .skills
        .iter()
        .map(|skill| ListItem::new(format!("{}: {}", skill.name, skill.description)))
        .collect();
    frame.render_widget(
        List::new(skill_items).block(Block::default().title("SKILLS").borders(Borders::ALL)),
        settings_columns[1],
    );

    frame.render_widget(
        Paragraph::new(format!(
            "{}  |  1: dashboard  e: enable/disable  x: remove  p: cycle level  l: logout  q: quit",
            app.status
        )),
        rows[3],
    );
}
