use super::client::McpClient;
use super::config::{McpConfig, McpServerConfig, load, save};
use std::path::Path;

/// One server's outcome from a `setup`/`check_for_updates` pass (4.2.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LifecycleCheckResult {
    pub name: String,
    pub outcome: LifecycleOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LifecycleOutcome {
    /// Connected; the server's self-reported version did not change.
    Unchanged {
        version: Option<String>,
    },
    /// Connected; the server's self-reported version differs from the
    /// last known-good one. Surfaced to the user rather than silently
    /// applied, since Open String cannot itself distinguish "intended
    /// upgrade" from "unexpected drift."
    VersionChanged {
        previous: Option<String>,
        current: Option<String>,
    },
    /// Connection failed; the config's `lastKnownVersion`/`lastCheckedAt`
    /// are left untouched, which is the rollback 4.2.5 asks for in a world
    /// where MCP has no server-side upgrade/downgrade RPC to roll back
    /// against directly (4.2.5's failure isolation also applies here: a
    /// failed check never blocks the other configured servers from being
    /// checked, nor blocks Core's own startup).
    Failed {
        reason: String,
    },
    Skipped,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Connects to every enabled server in `workspace`'s `.mcp.json` once,
/// without regard to `autoUpdate`/`updateCheckIntervalHours` -- this is the
/// "新規ワークスペース作成時に対応Extensionの自動セットアップを実行する
/// 仕組み" half of 4.2.5: a smoke test that the configured servers are
/// actually reachable right after a workspace is created, not a periodic
/// recheck.
pub fn setup_workspace_extensions(
    workspace: Option<&Path>,
) -> Result<Vec<LifecycleCheckResult>, String> {
    let mut config = load(workspace)?;
    let results = run_checks(&mut config, now_secs(), true);
    save(workspace, &config)?;
    Ok(results)
}

/// Re-checks every enabled, `autoUpdate`-enabled server whose
/// `updateCheckIntervalHours` has elapsed (or has none set) since
/// `lastCheckedAt`, recording any version drift detected via the
/// `initialize` handshake's `serverInfo` (4.2.5: periodic version check).
/// Open String has no background daemon, so "periodic" here means "checked
/// the next time this is invoked" -- callers wire this into a point Core
/// already runs regularly (e.g. `chat` startup, mirroring 4.6's health
/// check) rather than a true OS-level scheduler.
pub fn check_for_updates(workspace: Option<&Path>) -> Result<Vec<LifecycleCheckResult>, String> {
    let mut config = load(workspace)?;
    let results = run_checks(&mut config, now_secs(), false);
    save(workspace, &config)?;
    Ok(results)
}

fn run_checks(
    config: &mut McpConfig,
    now: u64,
    ignore_schedule: bool,
) -> Vec<LifecycleCheckResult> {
    let mut results = Vec::new();
    for (name, entry) in config.mcp_servers.iter_mut() {
        if entry.disabled {
            results.push(LifecycleCheckResult {
                name: name.clone(),
                outcome: LifecycleOutcome::Skipped,
            });
            continue;
        }
        if !ignore_schedule && !is_due(entry, now) {
            results.push(LifecycleCheckResult {
                name: name.clone(),
                outcome: LifecycleOutcome::Skipped,
            });
            continue;
        }

        let outcome = match McpClient::connect(&entry.command, &entry.args) {
            Ok(client) => {
                let current = client.server_info().map(|(_, version)| version.to_string());
                let previous = entry.last_known_version.clone();
                entry.last_checked_at = Some(now);
                if current.is_some() {
                    entry.last_known_version = current.clone();
                }
                if current.is_some() && current != previous {
                    LifecycleOutcome::VersionChanged { previous, current }
                } else {
                    LifecycleOutcome::Unchanged { version: current }
                }
            }
            Err(e) => LifecycleOutcome::Failed {
                reason: e.to_string(),
            },
        };
        results.push(LifecycleCheckResult {
            name: name.clone(),
            outcome,
        });
    }
    results
}

fn is_due(entry: &McpServerConfig, now: u64) -> bool {
    if !entry.auto_update {
        return false;
    }
    match (entry.update_check_interval_hours, entry.last_checked_at) {
        (Some(hours), Some(last)) => now.saturating_sub(last) >= hours * 3600,
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(command: &str) -> McpServerConfig {
        McpServerConfig {
            command: command.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn disabled_servers_are_skipped_without_connecting() {
        let mut config = McpConfig::default();
        config.mcp_servers.insert(
            "x".to_string(),
            McpServerConfig {
                disabled: true,
                ..entry("definitely-not-a-real-command")
            },
        );
        let results = run_checks(&mut config, 1000, true);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].outcome, LifecycleOutcome::Skipped);
    }

    #[test]
    fn failed_connection_leaves_last_known_version_untouched() {
        let mut config = McpConfig::default();
        config.mcp_servers.insert(
            "x".to_string(),
            McpServerConfig {
                last_known_version: Some("1.0.0".to_string()),
                last_checked_at: Some(500),
                ..entry("definitely-not-a-real-command")
            },
        );
        let results = run_checks(&mut config, 1000, true);
        assert!(matches!(
            results[0].outcome,
            LifecycleOutcome::Failed { .. }
        ));
        assert_eq!(
            config.mcp_servers["x"].last_known_version,
            Some("1.0.0".to_string())
        );
        assert_eq!(config.mcp_servers["x"].last_checked_at, Some(500));
    }

    #[test]
    fn is_due_respects_the_configured_interval() {
        let entry = McpServerConfig {
            update_check_interval_hours: Some(24),
            last_checked_at: Some(0),
            ..entry("x")
        };
        assert!(!is_due(&entry, 3600));
        assert!(is_due(&entry, 24 * 3600));
    }

    #[test]
    fn is_due_is_false_when_auto_update_is_disabled() {
        let entry = McpServerConfig {
            auto_update: false,
            ..entry("x")
        };
        assert!(!is_due(&entry, 999_999));
    }
}
