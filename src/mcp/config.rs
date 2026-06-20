use crate::permission::PermissionLevel;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

pub const MCP_CONFIG_FILE: &str = ".mcp.json";

/// One server entry in `.mcp.json`'s `mcpServers` map: how to spawn it,
/// whether it is currently enabled, and the permission level Core must be
/// at before connecting to it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpServerConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub disabled: bool,
    /// Minimum permission level required before Core will connect to this
    /// server (5.1's "Extensionが要求する権限とCoreの権限レベルの整合性
    /// チェック"). `None` means the server carries no extra requirement of
    /// its own beyond the per-call TaskScope/PermissionLevel gating every
    /// tool already goes through (4.7.1).
    #[serde(default, rename = "requiredPermissionLevel")]
    pub required_permission_level: Option<PermissionLevel>,
    /// Names of this server's tools the Mediator should call for "状態管理
    /// 用途" (4.7.1): persisting full conversation history and recording a
    /// searchable summary, mirroring `MemoryStore::save_history`/
    /// `record_index_entry`. Both must be set for this server to be used
    /// as a state-management backend; servers that don't expose memory
    /// tools simply leave these `None` and are never picked for that role.
    #[serde(default, rename = "memorySaveTool")]
    pub memory_save_tool: Option<String>,
    #[serde(default, rename = "memoryIndexTool")]
    pub memory_index_tool: Option<String>,
    /// Lifecycle settings (4.2.5), user-overridable per server.
    #[serde(default = "default_auto_update", rename = "autoUpdate")]
    pub auto_update: bool,
    /// Minimum hours between version-drift checks for this server. `None`
    /// means every `extension check-updates` run checks it unconditionally.
    #[serde(default, rename = "updateCheckIntervalHours")]
    pub update_check_interval_hours: Option<u64>,
    /// The server's self-reported version (`initialize`'s `serverInfo`) as
    /// of the last successful check. Only ever advanced by a *successful*
    /// reconnect (`check_for_updates`); a failed check leaves this at the
    /// last known-good value instead of clearing it, which is the rollback
    /// 4.2.5 asks for in a world where MCP has no server-side "upgrade"
    /// or "downgrade" RPC to actually roll back against.
    #[serde(default, rename = "lastKnownVersion")]
    pub last_known_version: Option<String>,
    #[serde(default, rename = "lastCheckedAt")]
    pub last_checked_at: Option<u64>,
}

fn default_auto_update() -> bool {
    true
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            command: String::new(),
            args: Vec::new(),
            disabled: false,
            required_permission_level: None,
            memory_save_tool: None,
            memory_index_tool: None,
            auto_update: default_auto_update(),
            update_check_interval_hours: None,
            last_known_version: None,
            last_checked_at: None,
        }
    }
}

impl McpServerConfig {
    /// Whether Core's active permission level satisfies this server's own
    /// requirement (5.1's compatibility check). Ranks god mode as the most
    /// permissive and high-protect as the least, matching the ordering
    /// `PermissionLevel::decide` already treats them with.
    pub fn is_compatible_with(&self, current: PermissionLevel) -> bool {
        match self.required_permission_level {
            None => true,
            Some(required) => current.permissiveness_rank() >= required.permissiveness_rank(),
        }
    }
}

/// The `.mcp.json`-equivalent config (5.1/5.2/5.3): every MCP server Core
/// knows how to connect to, keyed by a user-chosen name.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpConfig {
    #[serde(rename = "mcpServers", default)]
    pub mcp_servers: BTreeMap<String, McpServerConfig>,
}

/// Resolves the `.mcp.json` path: workspace-scoped when a workspace is
/// given (so two workspaces never share Extension configuration, 4.2.3),
/// otherwise the global default in the current directory.
pub fn config_path(workspace: Option<&Path>) -> PathBuf {
    match workspace {
        Some(dir) => dir.join(MCP_CONFIG_FILE),
        None => PathBuf::from(MCP_CONFIG_FILE),
    }
}

pub fn load(workspace: Option<&Path>) -> Result<McpConfig, String> {
    let path = config_path(workspace);
    match std::fs::read_to_string(&path) {
        Ok(raw) => {
            serde_json::from_str(&raw).map_err(|e| format!("invalid {}: {e}", path.display()))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(McpConfig::default()),
        Err(e) => Err(e.to_string()),
    }
}

pub fn save(workspace: Option<&Path>, config: &McpConfig) -> Result<(), String> {
    let path = config_path(workspace);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(config).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_workspace() -> PathBuf {
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = env::temp_dir().join(format!("open-string-mcp-config-test-{id}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn load_returns_default_when_no_config_file_exists() {
        let workspace = temp_workspace();
        let config = load(Some(&workspace)).unwrap();
        assert!(config.mcp_servers.is_empty());
        std::fs::remove_dir_all(&workspace).ok();
    }

    #[test]
    fn save_then_load_round_trips_a_server_entry() {
        let workspace = temp_workspace();
        let mut config = McpConfig::default();
        config.mcp_servers.insert(
            "t0k3n-mcp".to_string(),
            McpServerConfig {
                command: "npx".to_string(),
                args: vec!["-y".to_string(), "t0k3n-mcp".to_string()],
                required_permission_level: Some(PermissionLevel::LowSecurity),
                memory_save_tool: Some("memory_save".to_string()),
                memory_index_tool: Some("memory_index".to_string()),
                ..Default::default()
            },
        );

        save(Some(&workspace), &config).unwrap();
        let loaded = load(Some(&workspace)).unwrap();
        assert_eq!(loaded, config);

        std::fs::remove_dir_all(&workspace).ok();
    }

    #[test]
    fn is_compatible_with_allows_only_at_or_above_the_required_level() {
        let entry = McpServerConfig {
            command: "x".to_string(),
            required_permission_level: Some(PermissionLevel::LowSecurity),
            ..Default::default()
        };
        assert!(!entry.is_compatible_with(PermissionLevel::HighProtect));
        assert!(!entry.is_compatible_with(PermissionLevel::MiddlePermission));
        assert!(entry.is_compatible_with(PermissionLevel::LowSecurity));
        assert!(entry.is_compatible_with(PermissionLevel::GodMode));
    }

    #[test]
    fn is_compatible_with_is_unconditional_when_no_requirement_is_set() {
        let entry = McpServerConfig {
            command: "x".to_string(),
            ..Default::default()
        };
        assert!(entry.is_compatible_with(PermissionLevel::HighProtect));
    }
}
