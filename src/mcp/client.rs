use super::protocol::{McpError, McpTool, McpToolResult};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};

/// MCP protocol version this client speaks during the `initialize`
/// handshake. Servers negotiating a different version are still accepted
/// (the handshake's `result` is not otherwise inspected) since Open String
/// has no fallback behavior to degrade to yet.
const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

/// A connection to one MCP server over the stdio transport: newline-
/// delimited JSON-RPC 2.0 messages, one per line, in each direction (5.1's
/// "MCP準拠の外部サーバー接続インターフェース実装"). Requests are strictly
/// sequential -- this client never has more than one in flight -- so a
/// blocking read-after-write is enough; no async runtime is pulled in.
pub struct McpClient {
    child: Option<Child>,
    stdin: Box<dyn Write + Send>,
    stdout: Box<dyn BufRead + Send>,
    next_id: u64,
}

impl McpClient {
    /// Spawns `command` as a child process and performs the `initialize`
    /// handshake over its stdio.
    pub fn connect(command: &str, args: &[String]) -> Result<Self, McpError> {
        let mut child = Command::new(command)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdin = child.stdin.take().expect("spawned with piped stdin");
        let stdout = child.stdout.take().expect("spawned with piped stdout");
        Self::from_io(
            Some(child),
            Box::new(stdin),
            Box::new(BufReader::new(stdout)),
        )
    }

    /// Builds a client from explicit I/O instead of spawning a process.
    /// `pub(crate)` so other modules' tests (e.g. the `MemoryStore` adapter
    /// in `agent::mcp_memory`) can exercise the protocol layer against
    /// canned responses without a real subprocess.
    pub(crate) fn from_io(
        child: Option<Child>,
        stdin: Box<dyn Write + Send>,
        stdout: Box<dyn BufRead + Send>,
    ) -> Result<Self, McpError> {
        let mut client = Self {
            child,
            stdin,
            stdout,
            next_id: 1,
        };
        client.initialize()?;
        Ok(client)
    }

    fn initialize(&mut self) -> Result<(), McpError> {
        self.request(
            "initialize",
            json!({
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "open-string", "version": env!("CARGO_PKG_VERSION")},
            }),
        )?;
        self.notify("notifications/initialized", json!({}))
    }

    /// Calls `tools/list` and parses the advertised tool set.
    pub fn list_tools(&mut self) -> Result<Vec<McpTool>, McpError> {
        let result = self.request("tools/list", json!({}))?;
        let tools = result
            .get("tools")
            .cloned()
            .unwrap_or(Value::Array(Vec::new()));
        serde_json::from_value(tools).map_err(Into::into)
    }

    /// Calls `tools/call` for `name` with `arguments` and parses the result.
    pub fn call_tool(&mut self, name: &str, arguments: Value) -> Result<McpToolResult, McpError> {
        let result = self.request("tools/call", json!({"name": name, "arguments": arguments}))?;
        serde_json::from_value(result).map_err(Into::into)
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value, McpError> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        loop {
            let response = self.recv()?;
            if response.get("id").and_then(Value::as_u64) != Some(id) {
                // Not the response this call is waiting on (e.g. a
                // server-initiated notification); this client only ever
                // has one request outstanding, so skip and keep reading
                // rather than treating it as a protocol violation.
                continue;
            }
            if let Some(error) = response.get("error") {
                return Err(McpError::Rpc(error.to_string()));
            }
            return Ok(response.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<(), McpError> {
        self.send(&json!({"jsonrpc": "2.0", "method": method, "params": params}))
    }

    fn send(&mut self, value: &Value) -> Result<(), McpError> {
        let line = serde_json::to_string(value)?;
        writeln!(self.stdin, "{line}")?;
        self.stdin.flush()?;
        Ok(())
    }

    fn recv(&mut self) -> Result<Value, McpError> {
        let mut line = String::new();
        let bytes_read = self.stdout.read_line(&mut line)?;
        if bytes_read == 0 {
            return Err(McpError::Closed);
        }
        serde_json::from_str(&line).map_err(Into::into)
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        // 4.2.5/5.5's failure-isolation requirement starts here: a hung or
        // misbehaving Extension process must not outlive the client that
        // owns it.
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::{Arc, Mutex};

    struct SharedBuffer(Arc<Mutex<Vec<u8>>>);

    impl Write for SharedBuffer {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().write(buf)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Builds a client wired to canned response lines instead of a real
    /// process: line 1 always answers the `initialize` handshake, any
    /// further lines answer subsequent calls in order. Returns the buffer
    /// everything the client wrote to "stdin" so tests can assert on the
    /// outgoing requests too.
    fn client_with_canned_responses(lines: &[&str]) -> (McpClient, Arc<Mutex<Vec<u8>>>) {
        let written = Arc::new(Mutex::new(Vec::new()));
        let stdin = SharedBuffer(written.clone());
        let stdout = Cursor::new(format!("{}\n", lines.join("\n")).into_bytes());
        let client = McpClient::from_io(None, Box::new(stdin), Box::new(stdout))
            .expect("handshake against canned responses should succeed");
        (client, written)
    }

    #[test]
    fn connect_performs_the_initialize_handshake() {
        let (_client, written) = client_with_canned_responses(&[
            r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05"}}"#,
        ]);
        let sent = String::from_utf8(written.lock().unwrap().clone()).unwrap();
        assert!(sent.contains("\"method\":\"initialize\""));
        assert!(sent.contains("\"method\":\"notifications/initialized\""));
    }

    #[test]
    fn list_tools_parses_the_tools_array() {
        let (mut client, _written) = client_with_canned_responses(&[
            r#"{"jsonrpc":"2.0","id":1,"result":{}}"#,
            r#"{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"memory_save","description":"Save a memory","inputSchema":{}}]}}"#,
        ]);
        let tools = client.list_tools().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "memory_save");
    }

    #[test]
    fn call_tool_parses_text_content() {
        let (mut client, _written) = client_with_canned_responses(&[
            r#"{"jsonrpc":"2.0","id":1,"result":{}}"#,
            r#"{"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":"saved"}],"isError":false}}"#,
        ]);
        let result = client
            .call_tool("memory_save", json!({"key": "x"}))
            .unwrap();
        assert_eq!(result.text(), "saved");
        assert!(!result.is_error);
    }

    #[test]
    fn rpc_error_response_surfaces_as_an_error() {
        let (mut client, _written) = client_with_canned_responses(&[
            r#"{"jsonrpc":"2.0","id":1,"result":{}}"#,
            r#"{"jsonrpc":"2.0","id":2,"error":{"code":-32601,"message":"Method not found"}}"#,
        ]);
        let err = client.list_tools().unwrap_err();
        assert!(matches!(err, McpError::Rpc(_)));
    }

    #[test]
    fn an_unrelated_message_is_skipped_while_awaiting_a_response() {
        let (mut client, _written) = client_with_canned_responses(&[
            r#"{"jsonrpc":"2.0","id":1,"result":{}}"#,
            r#"{"jsonrpc":"2.0","method":"notifications/progress","params":{}}"#,
            r#"{"jsonrpc":"2.0","id":2,"result":{"tools":[]}}"#,
        ]);
        let tools = client.list_tools().unwrap();
        assert!(tools.is_empty());
    }

    #[test]
    fn closed_connection_before_the_handshake_responds_is_an_error() {
        let written = Arc::new(Mutex::new(Vec::new()));
        let stdin = SharedBuffer(written);
        let stdout = Cursor::new(Vec::new());
        let result = McpClient::from_io(None, Box::new(stdin), Box::new(stdout));
        assert!(matches!(result, Err(McpError::Closed)));
    }
}
