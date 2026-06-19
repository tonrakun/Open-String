use super::ctx_agent::MemoryStore;
use crate::llm::Message;
use crate::mcp::{self, McpClient};
use crate::permission::PermissionLevel;
use std::path::Path;
use std::sync::Mutex;

/// Adapts an MCP server's memory tools to the Mediator's `MemoryStore`
/// trait, so the Mediator can call out to a connected Extension for
/// "状態管理用途" (4.7.1's "Mediatorはt0k3n-mcp等のExtensionを「状態管理
/// 用途」で自ら呼び出す（memory_save/get、session_snapshot/restore等）")
/// instead of only writing to its own local `FileMemoryStore`. The
/// underlying client is behind a `Mutex` since `MemoryStore`'s methods take
/// `&self` but an MCP request/response round trip needs `&mut McpClient`.
pub struct McpMemoryStore {
    client: Mutex<McpClient>,
    save_tool: String,
    index_tool: String,
}

impl McpMemoryStore {
    pub fn new(
        client: McpClient,
        save_tool: impl Into<String>,
        index_tool: impl Into<String>,
    ) -> Self {
        Self {
            client: Mutex::new(client),
            save_tool: save_tool.into(),
            index_tool: index_tool.into(),
        }
    }
}

impl MemoryStore for McpMemoryStore {
    fn save_history(&self, label: &str, history: &[Message]) -> Result<(), String> {
        let history_json = serde_json::to_value(history).map_err(|e| e.to_string())?;
        let mut client = self
            .client
            .lock()
            .map_err(|_| "MCP memory client lock poisoned".to_string())?;
        client
            .call_tool(
                &self.save_tool,
                serde_json::json!({"label": label, "history": history_json}),
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn record_index_entry(&self, label: &str, summary: &str) -> Result<(), String> {
        let mut client = self
            .client
            .lock()
            .map_err(|_| "MCP memory client lock poisoned".to_string())?;
        client
            .call_tool(
                &self.index_tool,
                serde_json::json!({"label": label, "summary": summary}),
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

/// Looks for the first enabled, permission-compatible `.mcp.json` server
/// that declares both a `memorySaveTool` and a `memoryIndexTool`, connects
/// to it, and wraps it as a `MemoryStore`. Returns `None` (not an error)
/// when no such server is configured, or when every candidate fails to
/// connect -- callers fall back to the local `FileMemoryStore`, so one
/// unreachable Extension never blocks the Mediator from running (4.2.5's
/// "Extension障害時のフェイルセーフ").
pub fn connect_for_state_management(
    workspace: Option<&Path>,
    level: PermissionLevel,
) -> Option<Box<dyn MemoryStore + Sync>> {
    let config = mcp::load(workspace).ok()?;
    for (name, entry) in &config.mcp_servers {
        if entry.disabled || !entry.is_compatible_with(level) {
            continue;
        }
        let (Some(save_tool), Some(index_tool)) =
            (&entry.memory_save_tool, &entry.memory_index_tool)
        else {
            continue;
        };

        match McpClient::connect(&entry.command, &entry.args) {
            Ok(client) => {
                return Some(Box::new(McpMemoryStore::new(
                    client,
                    save_tool.clone(),
                    index_tool.clone(),
                )));
            }
            Err(e) => {
                eprintln!(
                    "warning: failed to connect to extension \"{name}\" for state management: {e}; trying the next candidate"
                );
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, Cursor, Write};

    struct SharedBuffer(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl Write for SharedBuffer {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().write(buf)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn fake_client(responses: &[&str]) -> McpClient {
        let written = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let stdin: Box<dyn Write + Send> = Box::new(SharedBuffer(written));
        let stdout: Box<dyn BufRead + Send> = Box::new(Cursor::new(
            format!("{}\n", responses.join("\n")).into_bytes(),
        ));
        McpClient::from_io(None, stdin, stdout).expect("handshake against canned responses")
    }

    #[test]
    fn save_history_calls_the_configured_save_tool() {
        let client = fake_client(&[
            r#"{"jsonrpc":"2.0","id":1,"result":{}}"#,
            r#"{"jsonrpc":"2.0","id":2,"result":{"content":[],"isError":false}}"#,
        ]);
        let store = McpMemoryStore::new(client, "memory_save", "memory_index");
        let history = vec![Message::user_text("hello")];
        store.save_history("session-1", &history).unwrap();
    }

    #[test]
    fn record_index_entry_calls_the_configured_index_tool() {
        let client = fake_client(&[
            r#"{"jsonrpc":"2.0","id":1,"result":{}}"#,
            r#"{"jsonrpc":"2.0","id":2,"result":{"content":[],"isError":false}}"#,
        ]);
        let store = McpMemoryStore::new(client, "memory_save", "memory_index");
        store
            .record_index_entry("session-1", "summary text")
            .unwrap();
    }

    #[test]
    fn save_history_surfaces_an_rpc_error() {
        let client = fake_client(&[
            r#"{"jsonrpc":"2.0","id":1,"result":{}}"#,
            r#"{"jsonrpc":"2.0","id":2,"error":{"code":-32601,"message":"Method not found"}}"#,
        ]);
        let store = McpMemoryStore::new(client, "memory_save", "memory_index");
        assert!(store.save_history("session-1", &[]).is_err());
    }

    #[test]
    fn connect_for_state_management_returns_none_without_a_config() {
        let workspace = std::env::temp_dir().join("open-string-mcp-memory-no-config-test");
        let _ = std::fs::remove_dir_all(&workspace);
        std::fs::create_dir_all(&workspace).unwrap();
        assert!(connect_for_state_management(Some(&workspace), PermissionLevel::GodMode).is_none());
        std::fs::remove_dir_all(&workspace).ok();
    }
}
