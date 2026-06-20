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
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Gauge, List, ListItem, Paragraph};
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

/// Calm cyan/gray theme shared by every screen: cyan marks the active
/// element, dark gray recedes, and red/yellow/green stay reserved for
/// genuine severity signals (danger level, health checks).
const ACCENT: Color = Color::Cyan;
const MUTED: Color = Color::DarkGray;
const SUCCESS: Color = Color::Green;
const DANGER: Color = Color::Red;
const WARNING: Color = Color::Yellow;

/// A panel with a rounded, muted border and an accent-colored title --
/// the baseline chrome for every bordered block outside the setup wizard's
/// current step.
fn panel(title: &str) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(MUTED))
        .title(Span::styled(
            format!(" {title} "),
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ))
}

/// Same chrome as `panel`, but with an accent-colored border to mark the
/// one focused/active region on screen (the wizard's current step).
fn panel_focused(title: &str) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(ACCENT))
        .title(Span::styled(
            format!(" {title} "),
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ))
}

fn level_color(level: PermissionLevel) -> Color {
    match level {
        PermissionLevel::GodMode => DANGER,
        PermissionLevel::LowSecurity => WARNING,
        PermissionLevel::MiddlePermission => ACCENT,
        PermissionLevel::HighProtect => SUCCESS,
    }
}

fn level_description(level: PermissionLevel) -> &'static str {
    match level {
        PermissionLevel::GodMode => "All operations allowed, no confirmation. Not recommended.",
        PermissionLevel::LowSecurity => {
            "Most operations allowed; irreversible actions need confirmation."
        }
        PermissionLevel::MiddlePermission => {
            "Directory/command whitelist; anything outside it needs confirmation."
        }
        PermissionLevel::HighProtect => {
            "Nearly every operation needs confirmation. Safest default."
        }
    }
}

fn header_lines() -> Vec<Line<'static>> {
    vec![
        Line::from(Span::styled(
            "OPEN STRING",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            "secure multi-agent assistant -- first-time setup",
            Style::default().fg(MUTED),
        )),
    ]
}

/// Renders the 3-step wizard progress as `1 API Key › 2 Permission › 3
/// Workspace`, marking completed steps with a green check and the current
/// step in reversed accent. `Done` reports every step complete.
fn step_indicator(current: SetupStep) -> Line<'static> {
    const STEPS: [(SetupStep, &str); 3] = [
        (SetupStep::ApiKey, "API Key"),
        (SetupStep::PermissionLevel, "Permission"),
        (SetupStep::Workspace, "Workspace"),
    ];
    let order = |s: SetupStep| match s {
        SetupStep::ApiKey => 0,
        SetupStep::PermissionLevel => 1,
        SetupStep::Workspace => 2,
        SetupStep::Done => 3,
    };
    let current_idx = order(current);
    let mut spans = Vec::new();
    for (i, (step, label)) in STEPS.iter().enumerate() {
        let idx = order(*step);
        if idx < current_idx {
            spans.push(Span::styled(
                format!(" ✓ {label} "),
                Style::default().fg(SUCCESS),
            ));
        } else if idx == current_idx {
            spans.push(Span::styled(
                format!(" {} {label} ", i + 1),
                Style::default()
                    .fg(ACCENT)
                    .add_modifier(Modifier::BOLD | Modifier::REVERSED),
            ));
        } else {
            spans.push(Span::styled(
                format!(" {} {label} ", i + 1),
                Style::default().fg(MUTED),
            ));
        }
        if i + 1 < STEPS.len() {
            spans.push(Span::styled(" › ", Style::default().fg(MUTED)));
        }
    }
    Line::from(spans)
}

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
    let workspace = app.workspace.clone();
    let setup = &mut app.setup;
    match setup.step {
        SetupStep::ApiKey => match key {
            KeyCode::Enter => {
                if validate_api_key_format(&setup.api_key_input) {
                    match AnthropicApiKeyProvider::for_workspace(workspace.as_deref())
                        .store(&setup.api_key_input)
                    {
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
        .title(Span::styled(
            " Confirm dangerous operation ",
            Style::default().fg(WARNING).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(WARNING));
    let text = vec![
        Line::from(dialog.summary.clone()),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "y",
                Style::default().fg(SUCCESS).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" = confirm   "),
            Span::styled(
                "any other key",
                Style::default().fg(MUTED).add_modifier(Modifier::BOLD),
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
        .constraints([
            Constraint::Length(2),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(area);

    frame.render_widget(
        Paragraph::new(header_lines()).alignment(Alignment::Center),
        chunks[0],
    );
    frame.render_widget(
        Paragraph::new(step_indicator(app.setup.step)).alignment(Alignment::Center),
        chunks[1],
    );

    let body = match app.setup.step {
        SetupStep::ApiKey => {
            let masked = "•".repeat(app.setup.api_key_input.chars().count());
            Paragraph::new(vec![
                Line::from(Span::styled(
                    "Anthropic API key",
                    Style::default().add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from(vec![
                    Span::styled("› ", Style::default().fg(ACCENT)),
                    Span::raw(masked),
                    Span::styled("█", Style::default().fg(ACCENT)),
                ]),
                Line::from(""),
                Line::from(Span::styled(
                    "Paste your API key and press Enter.",
                    Style::default().fg(MUTED),
                )),
            ])
        }
        SetupStep::PermissionLevel => {
            let mut text = vec![
                Line::from(Span::styled(
                    "Default permission level",
                    Style::default().add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
            ];
            for (i, level) in PERMISSION_LEVELS.iter().enumerate() {
                let level = *level;
                let selected = i == app.setup.level_index;
                let color = level_color(level);
                let marker = if selected { "› " } else { "  " };
                let style = if selected {
                    Style::default()
                        .fg(color)
                        .add_modifier(Modifier::BOLD | Modifier::REVERSED)
                } else {
                    Style::default().fg(color)
                };
                text.push(Line::from(Span::styled(format!("{marker}{level}"), style)));
                text.push(Line::from(Span::styled(
                    format!("    {}", level_description(level)),
                    Style::default().fg(MUTED),
                )));
            }
            text.push(Line::from(""));
            text.push(Line::from(Span::styled(
                "Up/Down to choose, Enter to confirm.",
                Style::default().fg(MUTED),
            )));
            Paragraph::new(text)
        }
        SetupStep::Workspace => {
            let path_active = !app.setup.editing_workspace_name;
            let field_style = |active: bool| {
                if active {
                    Style::default().fg(ACCENT)
                } else {
                    Style::default().fg(MUTED)
                }
            };
            Paragraph::new(vec![
                Line::from(Span::styled(
                    "Workspace (optional)",
                    Style::default().add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from(vec![
                    Span::styled("path  › ", field_style(path_active)),
                    Span::raw(app.setup.workspace_path_input.clone()),
                ]),
                Line::from(vec![
                    Span::styled("name  › ", field_style(!path_active)),
                    Span::raw(app.setup.workspace_name_input.clone()),
                ]),
                Line::from(""),
                Line::from(Span::styled(
                    "Tab to switch field, Enter to create (empty path to skip), Esc to skip.",
                    Style::default().fg(MUTED),
                )),
            ])
        }
        SetupStep::Done => Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled(
                "✓ Setup complete",
                Style::default().fg(SUCCESS).add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "Press any key to open the dashboard.",
                Style::default().fg(MUTED),
            )),
        ])
        .alignment(Alignment::Center),
    };

    let title = match app.setup.step {
        SetupStep::ApiKey => "Step 1 of 3",
        SetupStep::PermissionLevel => "Step 2 of 3",
        SetupStep::Workspace => "Step 3 of 3",
        SetupStep::Done => "Ready",
    };
    frame.render_widget(body.block(panel_focused(title)), chunks[3]);
    let status_style = if app.status.is_empty() {
        Style::default().fg(MUTED)
    } else {
        Style::default().fg(WARNING)
    };
    frame.render_widget(
        Paragraph::new(Span::styled(app.status.clone(), status_style)).alignment(Alignment::Center),
        chunks[4],
    );
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
    let gauge_color = if usage.0 >= 90 {
        DANGER
    } else if usage.0 >= 70 {
        WARNING
    } else {
        SUCCESS
    };
    let gauge = Gauge::default()
        .block(panel("Token consumption"))
        .gauge_style(Style::default().fg(gauge_color))
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
    frame.render_widget(List::new(sessions).block(panel("Sessions")), columns[0]);

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
        List::new(workspace_items).block(panel("Workspaces")),
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
        List::new(health_items).block(panel("Health check")),
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
        List::new(log_items).block(panel("Operation log (live)")),
        rows[3],
    );

    frame.render_widget(
        Paragraph::new(Span::styled(
            "1: dashboard  2: settings  r: refresh now  q: quit",
            Style::default().fg(MUTED),
        )),
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
                Style::default()
                    .fg(Color::Black)
                    .bg(ACCENT)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(format!(" {label} "), Style::default().fg(MUTED))
        }
    };
    Paragraph::new(Line::from(vec![
        Span::styled(
            " OPEN STRING ",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        mk("1:Dashboard", active == Screen::Dashboard),
        Span::raw(" "),
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

    let auth_color = if app.snapshot.auth_configured {
        SUCCESS
    } else {
        MUTED
    };
    let auth_line = if app.snapshot.auth_configured {
        "Anthropic API key: configured"
    } else {
        "Anthropic API key: not configured"
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::raw("Permission level: "),
                Span::styled(
                    app.snapshot.permission_level.to_string(),
                    Style::default()
                        .fg(level_color(app.snapshot.permission_level))
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled("  (press p to cycle)", Style::default().fg(MUTED)),
            ]),
            Line::from(vec![
                Span::styled(auth_line, Style::default().fg(auth_color)),
                Span::styled("  (press l to log out)", Style::default().fg(MUTED)),
            ]),
        ])
        .block(panel("Account")),
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
                "› "
            } else {
                "  "
            };
            let state_color = if ext.enabled { SUCCESS } else { MUTED };
            let state = if ext.enabled { "enabled" } else { "disabled" };
            let requirement = ext
                .required_permission_level
                .map(|level| format!(", requires {level}"))
                .unwrap_or_default();
            let line = Line::from(vec![
                Span::raw(marker),
                Span::styled(
                    ext.name.clone(),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(" ["),
                Span::styled(state, Style::default().fg(state_color)),
                Span::raw(format!(
                    "]: {} {}{requirement}",
                    ext.command,
                    ext.args.join(" ")
                )),
            ]);
            let item = ListItem::new(line);
            if i == app.selected_extension {
                item.style(Style::default().add_modifier(Modifier::REVERSED))
            } else {
                item
            }
        })
        .collect();
    frame.render_widget(
        List::new(items).block(panel("Extensions (e/x/Up/Down)")),
        settings_columns[0],
    );

    let skill_items: Vec<ListItem> = app
        .snapshot
        .skills
        .iter()
        .map(|skill| ListItem::new(format!("{}: {}", skill.name, skill.description)))
        .collect();
    frame.render_widget(
        List::new(skill_items).block(panel("SKILLS")),
        settings_columns[1],
    );

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(app.status.clone(), Style::default().fg(WARNING)),
            Span::styled(
                "  |  1: dashboard  e: enable/disable  x: remove  p: cycle level  l: logout  q: quit",
                Style::default().fg(MUTED),
            ),
        ])),
        rows[3],
    );
}
