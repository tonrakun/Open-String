mod audit_log;
mod danger;
mod file_store;
mod workspace_store;

pub use audit_log::{AuditDecision, AuditEntry, AuditLogger, FileAuditLogger};
pub use danger::{DangerKind, classify as classify_danger};
pub use file_store::FilePermissionStore;
pub use workspace_store::WorkspacePermissionStore;

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

/// Serialized as the same kebab-case string `as_str()`/`parse()` use
/// everywhere else (CLI flags, audit log lines), rather than a derived
/// PascalCase variant name, so an Extension config's
/// `requiredPermissionLevel` reads the same as `permission status`'s
/// output (5.1's permission-scope compatibility check).
impl serde::Serialize for PermissionLevel {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> serde::Deserialize<'de> for PermissionLevel {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        PermissionLevel::parse(&s)
            .ok_or_else(|| serde::de::Error::custom(format!("invalid permission level: {s}")))
    }
}

/// Whether an operation can proceed without asking the user first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionDecision {
    AutoAllow,
    RequireConfirmation,
}

impl PermissionLevel {
    /// Combines the active level with an operation's danger classification
    /// (4.1's common filter) and whether it's read-only into the
    /// confirmation policy each level's doc comment promises. This is the
    /// pre-check the Mediator runs before delegating a task to a Sub Agent
    /// (4.7.1); Sub Agents never call this themselves.
    ///
    /// `MiddlePermission`'s directory/command whitelist judgment isn't
    /// implemented yet, so it currently falls back to the same
    /// dangerous-operation gate as `LowSecurity`.
    pub fn decide(self, danger: &[DangerKind], read_only: bool) -> PermissionDecision {
        match self {
            PermissionLevel::GodMode => PermissionDecision::AutoAllow,
            PermissionLevel::LowSecurity | PermissionLevel::MiddlePermission => {
                if danger.is_empty() {
                    PermissionDecision::AutoAllow
                } else {
                    PermissionDecision::RequireConfirmation
                }
            }
            PermissionLevel::HighProtect => {
                if read_only {
                    PermissionDecision::AutoAllow
                } else {
                    PermissionDecision::RequireConfirmation
                }
            }
        }
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

    #[test]
    fn god_mode_always_auto_allows() {
        for danger in [vec![], vec![DangerKind::Delete]] {
            for read_only in [true, false] {
                assert_eq!(
                    PermissionLevel::GodMode.decide(&danger, read_only),
                    PermissionDecision::AutoAllow
                );
            }
        }
    }

    #[test]
    fn low_security_and_middle_permission_gate_on_danger_only() {
        for level in [
            PermissionLevel::LowSecurity,
            PermissionLevel::MiddlePermission,
        ] {
            assert_eq!(level.decide(&[], false), PermissionDecision::AutoAllow);
            assert_eq!(level.decide(&[], true), PermissionDecision::AutoAllow);
            assert_eq!(
                level.decide(&[DangerKind::Delete], false),
                PermissionDecision::RequireConfirmation
            );
            assert_eq!(
                level.decide(&[DangerKind::Delete], true),
                PermissionDecision::RequireConfirmation
            );
        }
    }

    #[test]
    fn high_protect_only_auto_allows_read_only_operations() {
        assert_eq!(
            PermissionLevel::HighProtect.decide(&[], true),
            PermissionDecision::AutoAllow
        );
        assert_eq!(
            PermissionLevel::HighProtect.decide(&[DangerKind::Delete], true),
            PermissionDecision::AutoAllow
        );
        assert_eq!(
            PermissionLevel::HighProtect.decide(&[], false),
            PermissionDecision::RequireConfirmation
        );
        assert_eq!(
            PermissionLevel::HighProtect.decide(&[DangerKind::Delete], false),
            PermissionDecision::RequireConfirmation
        );
    }
}
