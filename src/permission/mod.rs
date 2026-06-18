mod audit_log;
mod danger;
mod file_store;

pub use audit_log::{AuditDecision, AuditEntry, AuditLogger, FileAuditLogger};
pub use danger::{DangerKind, classify as classify_danger};
pub use file_store::FilePermissionStore;

use std::io;

/// The four operation permission tiers. Each tier controls how much an
/// operation must be confirmed by the user before Open String runs it.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum PermissionLevel {
    /// All operations allowed, no confirmation. Disabled by default; must
    /// be explicitly enabled and is reconfirmed on every launch.
    GodMode,
    /// Most operations allowed; only irreversible actions (delete, send,
    /// billing, publish) require confirmation.
    LowSecurity,
    /// Directory/command whitelist; anything outside the whitelist
    /// requires confirmation.
    MiddlePermission,
    /// Nearly every operation requires confirmation; only read-only
    /// operations are auto-allowed.
    #[default]
    HighProtect,
}

impl PermissionLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            PermissionLevel::GodMode => "god-mode",
            PermissionLevel::LowSecurity => "low-security",
            PermissionLevel::MiddlePermission => "middle-permission",
            PermissionLevel::HighProtect => "high-protect",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "god-mode" => Some(PermissionLevel::GodMode),
            "low-security" => Some(PermissionLevel::LowSecurity),
            "middle-permission" => Some(PermissionLevel::MiddlePermission),
            "high-protect" => Some(PermissionLevel::HighProtect),
            _ => None,
        }
    }
}

impl std::fmt::Display for PermissionLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PermissionError {
    #[error("could not determine the OS config directory")]
    NoConfigDir,
    #[error("permission config I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("invalid permission level stored on disk: {0}")]
    InvalidLevel(String),
}

/// Abstraction over where the active permission level is persisted, so
/// the storage backend can change (e.g. per-workspace config) without
/// touching the CLI layer.
pub trait PermissionStore {
    fn load(&self) -> Result<PermissionLevel, PermissionError>;
    fn set(&self, level: PermissionLevel) -> Result<(), PermissionError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_level_is_high_protect() {
        assert_eq!(PermissionLevel::default(), PermissionLevel::HighProtect);
    }

    #[test]
    fn as_str_round_trips_through_parse() {
        for level in [
            PermissionLevel::GodMode,
            PermissionLevel::LowSecurity,
            PermissionLevel::MiddlePermission,
            PermissionLevel::HighProtect,
        ] {
            assert_eq!(PermissionLevel::parse(level.as_str()), Some(level));
        }
    }

    #[test]
    fn parse_rejects_unknown_values() {
        assert_eq!(PermissionLevel::parse("nonsense"), None);
    }
}
