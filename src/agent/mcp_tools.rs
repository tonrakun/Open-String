use crate::llm::ToolDefinition;
use crate::mcp::{self, McpClient};
use crate::permission::PermissionLevel;
use std::path::Path;
use std::sync::{Arc, Mutex};

/// One tool sourced from a connected MCP server: its Claude-facing
/// definition plus a handle back to the client that serves it, so a Sub
/// Agent's `tool_use` call can be routed to the right server. Covers 4.7.2's
/// remaining "作業系ツール（...外部MCP呼び出し等）の実行に専従する" and "
/// t0k3n-mcp等のExtensionを「作業効率化用途」で呼び出す" -- both are just
/// this same generic "call whatever tool a connected, permission-compatible
/// server advertises" mechanism, since Open String has no special-cased
/// knowledge of any one server's tool names.
#[derive(Clone)]
pub struct McpToolSource {
    pub definition: ToolDefinition,
    pub client: Arc<Mutex<McpClient>>,
    /// Whether this tool comes from the bundled, officially verified
    /// Extension (`mcp::bundled::is_trusted_extension_name`) rather than a
    /// third-party server. Untrusted sources are sandboxed (5.3): excluded
    /// from read-only tasks entirely, and gated per-call by permission
    /// level otherwise (`ClaudeTaskExecutor::execute_tool`).
    pub trusted: bool,
}

/// Connects to every enabled, permission-compatible `.mcp.json` server for
/// `workspace` and collects the tools each one advertises. A server that
/// fails to connect or list its tools is skipped with a warning rather
/// than failing the whole call (4.2.5's Extension-failure isolation) --
/// the Sub Agent simply doesn't get that server's tools for this task.
pub fn connect_workspace_tools(
    workspace: Option<&Path>,
    level: PermissionLevel,
) -> Vec<McpToolSource> {
    let Ok(config) = mcp::load(workspace) else {
        return Vec::new();
    };

    let mut sources = Vec::new();
    for (name, entry) in &config.mcp_servers {
        if entry.disabled || !entry.is_compatible_with(level) {
            continue;
        }
        let mut client = match McpClient::connect(&entry.command, &entry.args) {
            Ok(client) => client,
            Err(e) => {
                eprintln!("warning: failed to connect to extension \"{name}\" for tool use: {e}");
                continue;
            }
        };
        let tools = match client.list_tools() {
            Ok(tools) => tools,
            Err(e) => {
                eprintln!(
                    "warning: connected to extension \"{name}\" but failed to list its tools: {e}"
                );
                continue;
            }
        };
        let client = Arc::new(Mutex::new(client));
        let trusted = mcp::is_trusted_extension_name(name);
        for tool in tools {
            sources.push(McpToolSource {
                definition: ToolDefinition {
                    name: tool.name,
                    description: tool.description,
                    input_schema: tool.input_schema,
                },
                client: client.clone(),
                trusted,
            });
        }
    }
    sources
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_no_tools_when_no_extension_config_exists() {
        let workspace = std::env::temp_dir().join("open-string-mcp-tools-no-config-test");
        let _ = std::fs::remove_dir_all(&workspace);
        std::fs::create_dir_all(&workspace).unwrap();
        assert!(connect_workspace_tools(Some(&workspace), PermissionLevel::GodMode).is_empty());
        std::fs::remove_dir_all(&workspace).ok();
    }

    #[test]
    fn skips_disabled_and_incompatible_servers_without_connecting() {
        let workspace = std::env::temp_dir().join("open-string-mcp-tools-skip-test");
        let _ = std::fs::remove_dir_all(&workspace);
        std::fs::create_dir_all(&workspace).unwrap();

        let mut config = mcp::load(Some(&workspace)).unwrap();
        config.mcp_servers.insert(
            "disabled-server".to_string(),
            mcp::McpServerConfig {
                command: "definitely-not-a-real-command".to_string(),
                disabled: true,
                ..Default::default()
            },
        );
        config.mcp_servers.insert(
            "too-permissive".to_string(),
            mcp::McpServerConfig {
                command: "definitely-not-a-real-command".to_string(),
                required_permission_level: Some(PermissionLevel::GodMode),
                ..Default::default()
            },
        );
        mcp::save(Some(&workspace), &config).unwrap();

        // Both entries are filtered out before a connection is ever
        // attempted, so this returns empty rather than hanging/erroring on
        // a nonexistent command.
        assert!(connect_workspace_tools(Some(&workspace), PermissionLevel::HighProtect).is_empty());

        std::fs::remove_dir_all(&workspace).ok();
    }
}
