//! Shared data-gathering for 4.3's TUI and GUI: both surfaces render the
//! same `DashboardSnapshot`, which is what "TUIと機能等価" actually means
//! in practice -- one function decides what counts as the dashboard, and
//! the two front ends just format it differently.

use crate::agent::{self, FileMemoryStore};
use crate::auth::{AnthropicApiKeyProvider, AuthProvider};
use crate::health::{self, HealthReport};
use crate::mcp::{self, McpServerConfig};
use crate::permission::{self, FileAuditLogger, PermissionLevel};
use crate::session::{
    self, FileSessionRegistry, FileWorkspaceRegistry, Session, SessionRegistry, Workspace,
    WorkspaceRegistry,
};
use crate::skills;
use std::path::Path;

/// Claude Sonnet 4.6's context window, used to evaluate the Ctx Agent's
/// percentage-based trigger/target thresholds (4.7.5) and to render the
/// dashboard's token-consumption figure as a fraction of it. Core has no
/// Models API call wired in to look this up at runtime (4.2.4), so it is
/// hardcoded alongside `llm::client::DEFAULT_MODEL`.
pub const MEDIATOR_CONTEXT_WINDOW_TOKENS: usize = 1_000_000;

/// Cap on how many trailing audit log lines the dashboard surfaces; this is
/// a live tail, not an archive browser, so older entries are left to the
/// log file itself.
const AUDIT_LOG_TAIL_LINES: usize = 50;

pub struct ExtensionSummary {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub enabled: bool,
    pub required_permission_level: Option<PermissionLevel>,
}

pub struct SkillSummary {
    pub name: String,
    pub description: String,
}

pub struct TokenUsage {
    pub used: usize,
    pub window: usize,
}

impl TokenUsage {
    pub fn percent(&self) -> u8 {
        if self.window == 0 {
            return 0;
        }
        ((self.used.saturating_mul(100)) / self.window).min(100) as u8
    }
}

pub struct DashboardSnapshot {
    pub permission_level: PermissionLevel,
    pub auth_configured: bool,
    pub sessions: Vec<Session>,
    pub workspaces: Vec<Workspace>,
    pub current_workspace: Option<Workspace>,
    pub extensions: Vec<ExtensionSummary>,
    pub skills: Vec<SkillSummary>,
    pub health: HealthReport,
    pub token_usage: Option<TokenUsage>,
    pub recent_audit_log: Vec<String>,
}

/// Gathers everything the dashboard needs in one call. Best-effort: a store
/// that fails to read (e.g. a workspace whose state directory doesn't exist
/// yet) contributes its default/empty value rather than failing the whole
/// snapshot, since "nothing to show yet" is itself useful dashboard state.
pub fn gather(workspace: Option<&Path>) -> DashboardSnapshot {
    let permission_level = crate::permission_store_for(workspace)
        .and_then(|store| {
            store
                .load()
                .map_err(|e| format!("failed to read permission level: {e}"))
        })
        .unwrap_or_default();

    let auth_configured = AnthropicApiKeyProvider::for_workspace(workspace)
        .load()
        .map(|stored| stored.is_some())
        .unwrap_or(false);

    let sessions = FileSessionRegistry::for_workspace(workspace)
        .and_then(|registry| registry.list())
        .unwrap_or_default();

    let workspaces = FileWorkspaceRegistry::new()
        .and_then(|registry| registry.list())
        .unwrap_or_default();
    let current_workspace = FileWorkspaceRegistry::new()
        .and_then(|registry| registry.current())
        .unwrap_or(None);

    let extensions = mcp::load(workspace)
        .map(|config| {
            config
                .mcp_servers
                .into_iter()
                .map(
                    |(name, entry): (String, McpServerConfig)| ExtensionSummary {
                        name,
                        command: entry.command,
                        args: entry.args,
                        enabled: !entry.disabled,
                        required_permission_level: entry.required_permission_level,
                    },
                )
                .collect()
        })
        .unwrap_or_default();

    let skills = skills::load_skills(workspace)
        .into_iter()
        .map(|skill| SkillSummary {
            name: skill.name,
            description: skill.description,
        })
        .collect();

    let health = health::run_health_check(workspace, permission_level);

    let token_usage = most_recent_active_session(&sessions)
        .and_then(|session| session_token_usage(workspace, session.id));

    let recent_audit_log = FileAuditLogger::new()
        .ok()
        .map(|logger| tail_lines(logger.path(), AUDIT_LOG_TAIL_LINES))
        .unwrap_or_default();

    DashboardSnapshot {
        permission_level,
        auth_configured,
        sessions,
        workspaces,
        current_workspace,
        extensions,
        skills,
        health,
        token_usage,
        recent_audit_log,
    }
}

fn most_recent_active_session(sessions: &[Session]) -> Option<&Session> {
    sessions
        .iter()
        .filter(|s| s.is_active())
        .max_by_key(|s| s.started_at)
        .or_else(|| sessions.iter().max_by_key(|s| s.started_at))
}

fn session_token_usage(workspace: Option<&Path>, session_id: u64) -> Option<TokenUsage> {
    let memory_dir = session::memory_dir_for(workspace).ok()?;
    let history = FileMemoryStore::at(memory_dir)
        .load_latest(&format!("session-{session_id}"))
        .ok()??;
    Some(TokenUsage {
        used: agent::estimate_history_tokens(&history),
        window: MEDIATOR_CONTEXT_WINDOW_TOKENS,
    })
}

fn tail_lines(path: &Path, n: usize) -> Vec<String> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let lines: Vec<String> = raw.lines().map(str::to_string).collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].to_vec()
}

/// `permission::classify_danger` re-export point for front ends that need
/// to decide whether an action they're about to take needs a confirmation
/// dialog (4.3's "危険操作確認ダイアログのレンダリング"), without each one
/// re-importing the whole `permission` module surface.
pub fn requires_confirmation(level: PermissionLevel, operation: &str) -> bool {
    let danger = permission::classify_danger(operation);
    level.decide(&danger, false) == permission::PermissionDecision::RequireConfirmation
}

/// Shared by the TUI and GUI settings screens (4.3's "TUIと機能等価").
pub fn set_extension_enabled(
    workspace: Option<&Path>,
    name: &str,
    enabled: bool,
) -> Result<(), String> {
    let mut config = mcp::load(workspace)?;
    let entry = config
        .mcp_servers
        .get_mut(name)
        .ok_or_else(|| format!("no such extension: {name}"))?;
    entry.disabled = !enabled;
    mcp::save(workspace, &config)
}

/// Shared by the TUI and GUI settings screens (4.3's "TUIと機能等価").
pub fn remove_extension(workspace: Option<&Path>, name: &str) -> Result<(), String> {
    let mut config = mcp::load(workspace)?;
    config.mcp_servers.remove(name);
    mcp::save(workspace, &config)
}

/// A dangerous action staged for confirmation (4.3's "危険操作確認ダイアロ
/// グのレンダリング"). Each variant carries what `apply_pending_action`
/// needs to actually perform the action once the user confirms; shared by
/// the TUI and GUI so the two front ends apply identical semantics.
pub enum PendingAction {
    SetPermissionLevel(PermissionLevel),
    SetExtensionEnabled { name: String, enabled: bool },
    RemoveExtension(String),
    Logout,
}

pub fn apply_pending_action(workspace: Option<&Path>, action: PendingAction) -> String {
    match action {
        PendingAction::SetPermissionLevel(level) => match crate::permission_store_for(workspace) {
            Ok(store) => match store.set(level) {
                Ok(()) => format!("Permission level set to {level}."),
                Err(e) => format!("Failed to set permission level: {e}"),
            },
            Err(e) => e,
        },
        PendingAction::SetExtensionEnabled { name, enabled } => {
            match set_extension_enabled(workspace, &name, enabled) {
                Ok(()) => format!(
                    "Extension \"{name}\" {}.",
                    if enabled { "enabled" } else { "disabled" }
                ),
                Err(e) => format!("Failed to update \"{name}\": {e}"),
            }
        }
        PendingAction::RemoveExtension(name) => match remove_extension(workspace, &name) {
            Ok(()) => format!("Extension \"{name}\" removed."),
            Err(e) => format!("Failed to remove \"{name}\": {e}"),
        },
        PendingAction::Logout => match AnthropicApiKeyProvider::for_workspace(workspace).clear() {
            Ok(()) => "Logged out.".to_string(),
            Err(e) => format!("Failed to log out: {e}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gather_does_not_panic_for_a_fresh_workspace() {
        let workspace = std::env::temp_dir().join("open-string-dashboard-test");
        let _ = std::fs::remove_dir_all(&workspace);
        std::fs::create_dir_all(&workspace).unwrap();

        let snapshot = gather(Some(&workspace));
        assert!(snapshot.sessions.is_empty());
        assert!(snapshot.extensions.is_empty());

        std::fs::remove_dir_all(&workspace).ok();
    }

    #[test]
    fn token_usage_percent_caps_at_100() {
        let usage = TokenUsage {
            used: 2_000_000,
            window: 1_000_000,
        };
        assert_eq!(usage.percent(), 100);
    }

    #[test]
    fn requires_confirmation_is_false_for_god_mode() {
        assert!(!requires_confirmation(
            PermissionLevel::GodMode,
            "delete everything"
        ));
    }

    #[test]
    fn requires_confirmation_is_true_for_a_dangerous_op_under_low_security() {
        assert!(requires_confirmation(
            PermissionLevel::LowSecurity,
            "delete the workspace"
        ));
    }
}
