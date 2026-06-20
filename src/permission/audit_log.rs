use super::{DangerKind, PermissionError, PermissionLevel, classify_danger};
use std::fmt;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Above this size, `record` rotates the log to `audit.log.1` before
/// appending, so a long-running gateway/chat process never grows the file
/// without bound. Chosen generously relative to one tab-separated line per
/// decision -- this is a safety cap, not a tuning knob exposed to users.
const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

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

    /// The rotated backup path (`<path>.1`) `record` writes to once
    /// `path` exceeds `MAX_LOG_BYTES`. Exposed so `audit export
    /// --include-rotated` can read it too.
    pub fn rotated_path(&self) -> PathBuf {
        rotated_path_for(&self.path)
    }

    /// Renames `path` to `path.1` (overwriting any previous backup) once it
    /// exceeds `MAX_LOG_BYTES`, so the active log restarts empty. A missing
    /// file is not a rotation candidate; any rename failure is surfaced
    /// rather than silently dropped, since losing audit entries silently
    /// would defeat 6.3's "god mode利用時の...ログ強制記録" guarantee.
    fn rotate_if_needed(&self) -> Result<(), PermissionError> {
        let size = match std::fs::metadata(&self.path) {
            Ok(meta) => meta.len(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        if size <= MAX_LOG_BYTES {
            return Ok(());
        }
        std::fs::rename(&self.path, self.rotated_path())?;
        Ok(())
    }
}

fn rotated_path_for(path: &Path) -> PathBuf {
    let mut rotated = path.as_os_str().to_owned();
    rotated.push(".1");
    PathBuf::from(rotated)
}

impl AuditLogger for FileAuditLogger {
    fn record(&self, entry: &AuditEntry) -> Result<(), PermissionError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        self.rotate_if_needed()?;
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

/// One audit log line, parsed back out of `format_line`'s tab-separated
/// format for `audit export` (the level/decision/danger fields are kept as
/// the same strings `as_str()` produces rather than re-parsed back into
/// enums, since export only ever re-displays them).
#[derive(Debug, Clone, serde::Serialize)]
pub struct ParsedEntry {
    pub timestamp: u64,
    pub level: String,
    pub decision: String,
    pub danger_kinds: Vec<String>,
    pub operation: String,
}

/// Parses one `format_line` line. Returns `None` for a line that doesn't
/// have the expected 5 tab-separated fields (e.g. a trailing blank line),
/// rather than failing the whole read over one malformed entry.
fn parse_line(line: &str) -> Option<ParsedEntry> {
    let mut fields = line.splitn(5, '\t');
    let timestamp = fields.next()?.parse().ok()?;
    let level = fields.next()?.to_string();
    let decision = fields.next()?.to_string();
    let danger = fields.next()?;
    let operation = fields.next()?.to_string();
    let danger_kinds = if danger == "none" {
        Vec::new()
    } else {
        danger.split(',').map(|s| s.to_string()).collect()
    };
    Some(ParsedEntry {
        timestamp,
        level,
        decision,
        danger_kinds,
        operation,
    })
}

/// Reads and parses every entry in `path`, in file order. A missing file
/// reads as empty rather than an error (e.g. nothing has been logged yet,
/// or the rotated backup doesn't exist).
pub fn read_entries(path: &Path) -> Result<Vec<ParsedEntry>, String> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.to_string()),
    };
    Ok(raw.lines().filter_map(parse_line).collect())
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

    #[test]
    fn parse_line_round_trips_format_line() {
        let entry = AuditEntry {
            level: PermissionLevel::LowSecurity,
            operation: "delete and charge",
            decision: AuditDecision::ConfirmedByUser,
        };
        let line = format_line(1000, &entry, &[DangerKind::Delete, DangerKind::Billing]);
        let parsed = parse_line(line.trim_end()).expect("valid line parses");
        assert_eq!(parsed.timestamp, 1000);
        assert_eq!(parsed.level, "low-security");
        assert_eq!(parsed.decision, "confirmed-by-user");
        assert_eq!(parsed.danger_kinds, vec!["delete", "billing"]);
        assert_eq!(parsed.operation, "delete and charge");
    }

    #[test]
    fn parse_line_reports_no_danger_as_an_empty_list() {
        let parsed = parse_line("1000\thigh-protect\tallowed\tnone\tread config").unwrap();
        assert!(parsed.danger_kinds.is_empty());
    }

    #[test]
    fn parse_line_rejects_malformed_lines() {
        assert!(parse_line("not enough fields").is_none());
    }

    #[test]
    fn read_entries_returns_empty_for_a_missing_file() {
        let path = std::env::temp_dir().join("open-string-audit-log-test-missing.log");
        std::fs::remove_file(&path).ok();
        assert_eq!(read_entries(&path).unwrap().len(), 0);
    }

    #[test]
    fn read_entries_parses_every_line_in_order() {
        let path = std::env::temp_dir().join("open-string-audit-log-test-read.log");
        std::fs::write(
            &path,
            "1000\thigh-protect\tallowed\tnone\tfirst\n2000\thigh-protect\tdenied\tnone\tsecond\n",
        )
        .unwrap();
        let entries = read_entries(&path).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].operation, "first");
        assert_eq!(entries[1].operation, "second");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn record_rotates_the_log_once_it_exceeds_the_size_cap() {
        let dir = std::env::temp_dir().join(format!(
            "open-string-audit-log-test-rotate-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("audit.log");
        std::fs::write(&path, "x".repeat((MAX_LOG_BYTES + 1) as usize)).unwrap();

        let logger = FileAuditLogger { path: path.clone() };
        logger
            .record(&AuditEntry {
                level: PermissionLevel::HighProtect,
                operation: "after rotation",
                decision: AuditDecision::Allowed,
            })
            .unwrap();

        let rotated = rotated_path_for(&path);
        assert!(rotated.is_file(), "oversized log should be rotated");
        assert_eq!(
            std::fs::metadata(&rotated).unwrap().len(),
            MAX_LOG_BYTES + 1
        );
        let active = std::fs::read_to_string(&path).unwrap();
        assert!(active.contains("after rotation"));
        assert!(active.len() < (MAX_LOG_BYTES + 1) as usize);

        std::fs::remove_dir_all(&dir).ok();
    }
}
