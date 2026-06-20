use crate::hotreload::{FileHotReloadLog, HotReloadLog};
use crate::mcp;
use crate::permission::PermissionLevel;
use crate::session;
use std::path::Path;

/// 4.6's "エラー検知時の自動分類（致命的/警告/情報）". `Fatal` is reserved
/// for things that need a human's attention and cannot be silently worked
/// around; it never means Core itself stops running, since Extension/
/// config failures must not take down Core's own functionality (4.2.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Fatal,
    Warning,
    Info,
}

#[derive(Debug, Clone)]
pub struct HealthCheckItem {
    pub name: String,
    pub severity: Severity,
    pub message: String,
    /// Whether a self-repair action was taken for this item (4.6's "自己
    /// 修正ロジック").
    pub repaired: bool,
}

#[derive(Debug, Clone)]
pub struct HealthReport {
    pub items: Vec<HealthCheckItem>,
}

impl HealthReport {
    pub fn has_fatal(&self) -> bool {
        self.items.iter().any(|i| i.severity == Severity::Fatal)
    }
}

const EXTENSION_CONNECT_ATTEMPTS: usize = 2;

/// Runs Core's health check (4.6): binary integrity, `.mcp.json` integrity
/// (self-repaired when `level` permits, per "自己修復処理自体の権限レベル
/// 適用"), and Extension connectivity (retried once for transient
/// failures, per "自動リトライ機構"). Intended to run at startup and at
/// whatever other points Core already runs regularly -- there is no
/// background scheduler, so "periodic" health checks piggyback on those
/// existing touchpoints (e.g. `chat` startup) rather than a dedicated
/// daemon.
pub fn run_health_check(workspace: Option<&Path>, level: PermissionLevel) -> HealthReport {
    let mut items = vec![
        check_binary_integrity(),
        check_config_integrity(workspace, level),
        check_hot_reload(workspace),
    ];
    items.extend(check_extension_connectivity(workspace));
    HealthReport { items }
}

/// 5.5's "ホットリロード処理自体もセルフヘルスチェック層の監視対象に含める":
/// surfaces the most recent reload attempt recorded by `ConfigWatcher`/
/// `FileHotReloadLog` (driven from `main::chat`'s REPL loop). No recorded
/// activity is not a problem -- a session that never had a config change
/// to react to has nothing to report -- so it's `Info`, not `Warning`.
fn check_hot_reload(workspace: Option<&Path>) -> HealthCheckItem {
    let path = match session::hotreload_log_path_for(workspace) {
        Ok(path) => path,
        Err(e) => {
            return HealthCheckItem {
                name: "hot_reload".to_string(),
                severity: Severity::Warning,
                message: format!("could not resolve the hot reload log path: {e}"),
                repaired: false,
            };
        }
    };

    match FileHotReloadLog::at(path).recent(1) {
        Ok(events) if events.is_empty() => HealthCheckItem {
            name: "hot_reload".to_string(),
            severity: Severity::Info,
            message: "no hot reload activity recorded yet".to_string(),
            repaired: false,
        },
        Ok(events) => {
            let last = events.last().expect("checked non-empty above");
            if last.success {
                HealthCheckItem {
                    name: "hot_reload".to_string(),
                    severity: Severity::Info,
                    message: format!("last reload of \"{}\" succeeded", last.source),
                    repaired: false,
                }
            } else {
                HealthCheckItem {
                    name: "hot_reload".to_string(),
                    severity: Severity::Warning,
                    message: format!(
                        "last reload of \"{}\" failed and fell back to the previous config: {}",
                        last.source, last.message
                    ),
                    repaired: false,
                }
            }
        }
        Err(e) => HealthCheckItem {
            name: "hot_reload".to_string(),
            severity: Severity::Warning,
            message: format!("failed to read the hot reload log: {e}"),
            repaired: false,
        },
    }
}

fn check_binary_integrity() -> HealthCheckItem {
    match std::env::current_exe() {
        Ok(path) if path.is_file() => HealthCheckItem {
            name: "binary".to_string(),
            severity: Severity::Info,
            message: format!("running from {}", path.display()),
            repaired: false,
        },
        Ok(path) => HealthCheckItem {
            name: "binary".to_string(),
            severity: Severity::Fatal,
            message: format!("executable path {} is not a regular file", path.display()),
            repaired: false,
        },
        Err(e) => HealthCheckItem {
            name: "binary".to_string(),
            severity: Severity::Warning,
            message: format!("could not determine the running executable's path: {e}"),
            repaired: false,
        },
    }
}

/// Self-repair (rewriting a corrupt config back to defaults) is itself a
/// risky write, so it requires more than the default high-protect level
/// (4.6's "middle permission以上を要求").
fn can_self_repair(level: PermissionLevel) -> bool {
    !matches!(level, PermissionLevel::HighProtect)
}

fn check_config_integrity(workspace: Option<&Path>, level: PermissionLevel) -> HealthCheckItem {
    let path = mcp::config_path(workspace);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(_) => {
            // A missing file is not corruption: `mcp::load` already treats
            // it as an empty default config.
            return HealthCheckItem {
                name: "config".to_string(),
                severity: Severity::Info,
                message: format!("no {} present (using defaults)", path.display()),
                repaired: false,
            };
        }
    };

    if serde_json::from_str::<serde_json::Value>(&raw).is_ok() {
        return HealthCheckItem {
            name: "config".to_string(),
            severity: Severity::Info,
            message: format!("{} is valid", path.display()),
            repaired: false,
        };
    }

    if !can_self_repair(level) {
        return HealthCheckItem {
            name: "config".to_string(),
            severity: Severity::Fatal,
            message: format!(
                "{} is corrupt and the active permission level ({level}) does not allow \
                 automatic repair; escalating to the user",
                path.display()
            ),
            repaired: false,
        };
    }

    let backup_path = backup_path_for(&path);
    let repaired = std::fs::rename(&path, &backup_path).is_ok()
        && mcp::save(workspace, &Default::default()).is_ok();
    HealthCheckItem {
        name: "config".to_string(),
        severity: if repaired {
            Severity::Warning
        } else {
            Severity::Fatal
        },
        message: if repaired {
            format!(
                "{} was corrupt; backed up to {} and restored defaults",
                path.display(),
                backup_path.display()
            )
        } else {
            format!(
                "{} is corrupt and automatic repair failed; escalating to the user",
                path.display()
            )
        },
        repaired,
    }
}

fn backup_path_for(path: &Path) -> std::path::PathBuf {
    let mut backup = path.as_os_str().to_os_string();
    backup.push(".corrupt");
    backup.into()
}

/// Extensions are checked for reachability only; a connection failure
/// never escalates past `Warning`, since Core must keep functioning with
/// an Extension down (4.2.5's failure isolation).
fn check_extension_connectivity(workspace: Option<&Path>) -> Vec<HealthCheckItem> {
    let config = match mcp::load(workspace) {
        Ok(config) => config,
        Err(e) => {
            return vec![HealthCheckItem {
                name: "extensions".to_string(),
                severity: Severity::Warning,
                message: format!("failed to read .mcp.json for connectivity check: {e}"),
                repaired: false,
            }];
        }
    };

    config
        .mcp_servers
        .iter()
        .filter(|(_, entry)| !entry.disabled)
        .map(|(name, entry)| {
            let mut last_error = String::new();
            for _ in 0..EXTENSION_CONNECT_ATTEMPTS {
                match mcp::McpClient::connect(&entry.command, &entry.args) {
                    Ok(client) => {
                        // 5.3's protocol-version compatibility check: a
                        // server is reachable but speaking a different MCP
                        // protocol version is a Warning, not a Fatal --
                        // Core still functions with that Extension simply
                        // unusable (4.2.5's failure isolation).
                        return if client.is_protocol_compatible() {
                            HealthCheckItem {
                                name: format!("extension:{name}"),
                                severity: Severity::Info,
                                message: "reachable".to_string(),
                                repaired: false,
                            }
                        } else {
                            HealthCheckItem {
                                name: format!("extension:{name}"),
                                severity: Severity::Warning,
                                message: format!(
                                    "reachable but negotiated protocol version {} differs from Core's {}",
                                    client.negotiated_protocol_version().unwrap_or("unknown"),
                                    mcp::McpClient::supported_protocol_version()
                                ),
                                repaired: false,
                            }
                        };
                    }
                    Err(e) => last_error = e.to_string(),
                }
            }
            HealthCheckItem {
                name: format!("extension:{name}"),
                severity: Severity::Warning,
                message: format!(
                    "unreachable after {EXTENSION_CONNECT_ATTEMPTS} attempt(s): {last_error}"
                ),
                repaired: false,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_workspace() -> std::path::PathBuf {
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = env::temp_dir().join(format!("open-string-health-test-{id}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn binary_integrity_passes_for_the_running_test_executable() {
        let item = check_binary_integrity();
        assert_eq!(item.severity, Severity::Info);
    }

    #[test]
    fn missing_config_is_reported_as_info_not_corruption() {
        let workspace = temp_workspace();
        let item = check_config_integrity(Some(&workspace), PermissionLevel::HighProtect);
        assert_eq!(item.severity, Severity::Info);
        std::fs::remove_dir_all(&workspace).ok();
    }

    #[test]
    fn corrupt_config_is_repaired_when_permission_level_allows_it() {
        let workspace = temp_workspace();
        std::fs::write(mcp::config_path(Some(&workspace)), "{ not json").unwrap();

        let item = check_config_integrity(Some(&workspace), PermissionLevel::LowSecurity);
        assert!(item.repaired);
        assert_eq!(item.severity, Severity::Warning);
        assert!(mcp::load(Some(&workspace)).unwrap().mcp_servers.is_empty());

        std::fs::remove_dir_all(&workspace).ok();
    }

    #[test]
    fn corrupt_config_escalates_instead_of_repairing_under_high_protect() {
        let workspace = temp_workspace();
        let config_path = mcp::config_path(Some(&workspace));
        std::fs::write(&config_path, "{ not json").unwrap();

        let item = check_config_integrity(Some(&workspace), PermissionLevel::HighProtect);
        assert!(!item.repaired);
        assert_eq!(item.severity, Severity::Fatal);
        // The corrupt file is left exactly as found for the user to inspect.
        assert_eq!(std::fs::read_to_string(&config_path).unwrap(), "{ not json");

        std::fs::remove_dir_all(&workspace).ok();
    }

    #[test]
    fn extension_connectivity_is_empty_when_no_servers_are_configured() {
        let workspace = temp_workspace();
        assert!(check_extension_connectivity(Some(&workspace)).is_empty());
        std::fs::remove_dir_all(&workspace).ok();
    }

    #[test]
    fn hot_reload_check_is_info_when_nothing_has_been_recorded() {
        let workspace = temp_workspace();
        let item = check_hot_reload(Some(&workspace));
        assert_eq!(item.severity, Severity::Info);
        std::fs::remove_dir_all(&workspace).ok();
    }

    #[test]
    fn hot_reload_check_warns_after_a_failed_reload() {
        let workspace = temp_workspace();
        let log_path = session::hotreload_log_path_for(Some(&workspace)).unwrap();
        FileHotReloadLog::at(log_path)
            .record(crate::hotreload::ReloadEvent::now(
                "mcp config",
                false,
                "parse error",
            ))
            .unwrap();

        let item = check_hot_reload(Some(&workspace));
        assert_eq!(item.severity, Severity::Warning);
        assert!(item.message.contains("parse error"));
        std::fs::remove_dir_all(&workspace).ok();
    }

    #[test]
    fn has_fatal_detects_a_fatal_item_among_others() {
        let report = HealthReport {
            items: vec![
                HealthCheckItem {
                    name: "a".to_string(),
                    severity: Severity::Info,
                    message: String::new(),
                    repaired: false,
                },
                HealthCheckItem {
                    name: "b".to_string(),
                    severity: Severity::Fatal,
                    message: String::new(),
                    repaired: false,
                },
            ],
        };
        assert!(report.has_fatal());
    }
}
