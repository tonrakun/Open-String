use super::{DangerKind, PermissionError, PermissionLevel, classify_danger};
use std::fmt;
use std::io::Write as _;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditDecision {
    Allowed,
    Denied,
    ConfirmedByUser,
    DeclinedByUser,
}

impl AuditDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            AuditDecision::Allowed => "allowed",
            AuditDecision::Denied => "denied",
            AuditDecision::ConfirmedByUser => "confirmed-by-user",
            AuditDecision::DeclinedByUser => "declined-by-user",
        }
    }
}

impl fmt::Display for AuditDecision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

pub struct AuditEntry<'a> {
    pub level: PermissionLevel,
    pub operation: &'a str,
    pub decision: AuditDecision,
}

/// Records permission decisions. There is currently no setting to disable
/// logging, so god mode's "forced log recording" requirement (6.3) holds
/// trivially: every entry, including god mode ones, always goes through
/// `record`.
pub trait AuditLogger {
    fn record(&self, entry: &AuditEntry) -> Result<(), PermissionError>;
}

/// Appends one line per decision to a log file under the OS config
/// directory, in the same place as `FilePermissionStore` keeps the active
/// level.
pub struct FileAuditLogger {
    path: PathBuf,
}

impl FileAuditLogger {
    pub fn new() -> Result<Self, PermissionError> {
        let dir = dirs::config_dir()
            .ok_or(PermissionError::NoConfigDir)?
            .join("open-string");
        Ok(Self {
            path: dir.join("audit.log"),
        })
    }

    /// Exposes the backing log file path for readers outside this module
    /// (4.3's "操作ログのリアルタイム表示" dashboard data).
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl AuditLogger for FileAuditLogger {
    fn record(&self, entry: &AuditEntry) -> Result<(), PermissionError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let danger = classify_danger(entry.operation);
        let line = format_line(timestamp, entry, &danger);

        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        file.write_all(line.as_bytes())?;
        Ok(())
    }
}

fn format_line(timestamp: u64, entry: &AuditEntry, danger: &[DangerKind]) -> String {
    let danger_str = if danger.is_empty() {
        "none".to_string()
    } else {
        danger
            .iter()
            .map(|k| k.as_str())
            .collect::<Vec<_>>()
            .join(",")
    };
    format!(
        "{timestamp}\t{}\t{}\t{}\t{}\n",
        entry.level, entry.decision, danger_str, entry.operation
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_line_marks_no_danger_as_none() {
        let entry = AuditEntry {
            level: PermissionLevel::HighProtect,
            operation: "read config",
            decision: AuditDecision::Allowed,
        };
        let line = format_line(1000, &entry, &[]);
        assert_eq!(line, "1000\thigh-protect\tallowed\tnone\tread config\n");
    }

    #[test]
    fn format_line_joins_multiple_danger_kinds() {
        let entry = AuditEntry {
            level: PermissionLevel::LowSecurity,
            operation: "delete and charge",
            decision: AuditDecision::ConfirmedByUser,
        };
        let line = format_line(1000, &entry, &[DangerKind::Delete, DangerKind::Billing]);
        assert_eq!(
            line,
            "1000\tlow-security\tconfirmed-by-user\tdelete,billing\tdelete and charge\n"
        );
    }
}
