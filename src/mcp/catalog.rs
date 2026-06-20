//! A short, curated list of well-known MCP servers `extension install`
//! can set up by name without the caller having to know the exact
//! `command`/`args` `extension add` would otherwise require, plus a
//! per-user/per-workspace local catalog file for entries Open String
//! itself doesn't ship (5.3's "既知の(キュレーションされた)サードパーティ
//! Extensionをワンコマンドで導入できるカタログ機能"). The built-in list is
//! treated as trusted the same way the bundled t0k3n-mcp Extension is
//! (`bundled::is_trusted_extension_name`); local catalog entries are not,
//! since Open String never vetted them.

use super::config::McpServerConfig;
use crate::permission::PermissionLevel;
use serde::Deserialize;
use std::path::{Path, PathBuf};

pub const LOCAL_CATALOG_FILE: &str = "extension_catalog.json";

/// One installable MCP server: enough to build a `McpServerConfig` from
/// just a name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogEntry {
    pub name: String,
    pub description: String,
    pub command: String,
    pub args: Vec<String>,
    pub required_permission_level: Option<PermissionLevel>,
    pub memory_save_tool: Option<String>,
    pub memory_index_tool: Option<String>,
}

impl CatalogEntry {
    pub fn to_server_config(&self) -> McpServerConfig {
        McpServerConfig {
            command: self.command.clone(),
            args: self.args.clone(),
            required_permission_level: self.required_permission_level,
            memory_save_tool: self.memory_save_tool.clone(),
            memory_index_tool: self.memory_index_tool.clone(),
            ..McpServerConfig::default()
        }
    }
}

/// `npx` itself is a `.cmd` shim on Windows; spawning the bare name there
/// fails with "program not found" since process creation doesn't resolve
/// shim extensions the way a shell would (the same reason
/// `bundled::installed_binary_path` resolves a concrete `.exe` rather than
/// relying on a bare name).
fn npx_command() -> &'static str {
    if cfg!(windows) { "npx.cmd" } else { "npx" }
}

fn npx_entry(name: &str, package: &str, description: &str) -> CatalogEntry {
    CatalogEntry {
        name: name.to_string(),
        description: description.to_string(),
        command: npx_command().to_string(),
        args: vec!["-y".to_string(), package.to_string()],
        required_permission_level: None,
        memory_save_tool: None,
        memory_index_tool: None,
    }
}

/// The Extensions Open String itself curates and trusts (5.3). All three
/// are official `modelcontextprotocol/servers` reference implementations,
/// launched the same `npx -y <package>` way the bundled t0k3n-mcp Extension
/// already is.
pub fn builtin_catalog() -> Vec<CatalogEntry> {
    vec![
        npx_entry(
            "mcp-memory",
            "@modelcontextprotocol/server-memory",
            "Official reference MCP server: a simple knowledge-graph memory store",
        ),
        npx_entry(
            "mcp-sequential-thinking",
            "@modelcontextprotocol/server-sequential-thinking",
            "Official reference MCP server: structured step-by-step reasoning tool",
        ),
        npx_entry(
            "mcp-filesystem",
            "@modelcontextprotocol/server-filesystem",
            "Official reference MCP server: sandboxed filesystem access (pass an allowed \
             directory as an extra arg via `extension add`/local catalog if you need one \
             other than the current directory)",
        ),
    ]
}

#[derive(Debug, Deserialize)]
struct LocalCatalogEntry {
    name: String,
    #[serde(default)]
    description: String,
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default, rename = "requiredPermissionLevel")]
    required_permission_level: Option<PermissionLevel>,
    #[serde(default, rename = "memorySaveTool")]
    memory_save_tool: Option<String>,
    #[serde(default, rename = "memoryIndexTool")]
    memory_index_tool: Option<String>,
}

impl From<LocalCatalogEntry> for CatalogEntry {
    fn from(entry: LocalCatalogEntry) -> Self {
        Self {
            name: entry.name,
            description: entry.description,
            command: entry.command,
            args: entry.args,
            required_permission_level: entry.required_permission_level,
            memory_save_tool: entry.memory_save_tool,
            memory_index_tool: entry.memory_index_tool,
        }
    }
}

fn local_catalog_path(workspace: Option<&Path>) -> PathBuf {
    match workspace {
        Some(dir) => dir.join(LOCAL_CATALOG_FILE),
        None => PathBuf::from(LOCAL_CATALOG_FILE),
    }
}

/// Reads the user-maintained catalog file, if any. A missing or malformed
/// file reads as an empty catalog (fail-soft, same as `extensions.json`)
/// rather than an error, since this is a convenience feature on top of
/// `extension add`, not a required config file.
pub fn local_catalog(workspace: Option<&Path>) -> Vec<CatalogEntry> {
    let path = local_catalog_path(workspace);
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let Ok(entries) = serde_json::from_str::<Vec<LocalCatalogEntry>>(&raw) else {
        return Vec::new();
    };
    entries.into_iter().map(CatalogEntry::from).collect()
}

/// Resolves `name` against the built-in catalog first, then the local
/// catalog file, so a user-defined entry can't shadow (or be confused
/// with) a curated one.
pub fn find(name: &str, workspace: Option<&Path>) -> Option<CatalogEntry> {
    builtin_catalog()
        .into_iter()
        .find(|e| e.name == name)
        .or_else(|| {
            local_catalog(workspace)
                .into_iter()
                .find(|e| e.name == name)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_resolves_a_builtin_entry_by_name() {
        let entry = find("mcp-memory", None).expect("builtin entry should resolve");
        assert_eq!(entry.command, npx_command());
        assert_eq!(
            entry.args,
            vec![
                "-y".to_string(),
                "@modelcontextprotocol/server-memory".to_string()
            ]
        );
    }

    #[test]
    fn find_returns_none_for_an_unknown_name() {
        assert!(find("definitely-not-a-known-extension", None).is_none());
    }

    #[test]
    fn local_catalog_reads_a_well_formed_file() {
        let dir = std::env::temp_dir().join(format!(
            "open-string-catalog-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(LOCAL_CATALOG_FILE),
            r#"[{"name": "my-server", "command": "my-mcp-server", "args": ["--flag"]}]"#,
        )
        .unwrap();

        let entries = local_catalog(Some(&dir));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "my-server");
        assert_eq!(entries[0].command, "my-mcp-server");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn local_catalog_is_empty_when_the_file_is_missing() {
        let dir = std::env::temp_dir().join("open-string-catalog-test-missing");
        std::fs::remove_dir_all(&dir).ok();
        assert!(local_catalog(Some(&dir)).is_empty());
    }

    #[test]
    fn find_prefers_the_builtin_entry_over_a_same_named_local_one() {
        let dir = std::env::temp_dir().join(format!(
            "open-string-catalog-test-shadow-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(LOCAL_CATALOG_FILE),
            r#"[{"name": "mcp-memory", "command": "something-else"}]"#,
        )
        .unwrap();

        let entry = find("mcp-memory", Some(&dir)).unwrap();
        assert_eq!(entry.command, npx_command());

        std::fs::remove_dir_all(&dir).ok();
    }
}
