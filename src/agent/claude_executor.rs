use super::mcp_tools::McpToolSource;
use super::system_prompt::{ExtensionInfo, SystemPromptBuilder};
use super::tools;
use super::{Task, TaskExecutor, TaskResult, TaskScope};
use crate::llm::{ClaudeClient, ClaudeError, ContentBlock, Message, ToolDefinition};
use crate::permission::{PermissionDecision, classify_danger};

/// Upper bound on tool-call round trips for a single task, so a confused or
/// looping model can't keep a disposable Sub Agent running forever (4.7.2).
const MAX_TOOL_ITERATIONS: usize = 8;

/// Real Sub Agent executor backed by the Claude API (replaces
/// `EchoTaskExecutor` for production use). Drives a tool-use loop so the
/// Sub Agent can actually perform file operations and run commands, not
/// just produce text (4.7.2). The system prompt sent to the model is
/// assembled from fragments rather than fixed (4.2.1): the narration-ban
/// and permission-level rules always apply, an Extension fragment is added
/// only for Extensions actually connected to this executor, and the
/// read-only suffix is added only when the scope warrants it.
pub struct ClaudeTaskExecutor<'a> {
    client: &'a ClaudeClient,
    extensions: Vec<ExtensionInfo>,
    mcp_tools: Vec<McpToolSource>,
}

impl<'a> ClaudeTaskExecutor<'a> {
    pub fn new(client: &'a ClaudeClient) -> Self {
        Self {
            client,
            extensions: Vec::new(),
            mcp_tools: Vec::new(),
        }
    }

    /// Registers the Extensions connected for this run so their usage
    /// instructions are injected into the system prompt (4.2.1). Extensions
    /// not passed here contribute no prompt fragment.
    pub fn with_extensions(mut self, extensions: Vec<ExtensionInfo>) -> Self {
        self.extensions = extensions;
        self
    }

    /// Registers tools sourced from connected MCP servers (4.7.2's "外部MCP
    /// 呼び出し"/"t0k3n-mcp等のExtensionを「作業効率化用途」で呼び出す"):
    /// offered to the model alongside the built-in tools, and routed back
    /// to the server that advertised them when called.
    pub fn with_mcp_tools(mut self, mcp_tools: Vec<McpToolSource>) -> Self {
        self.mcp_tools = mcp_tools;
        self
    }

    /// Routes a `tool_use` call to the built-in dispatcher unless `name`
    /// matches a connected MCP server's advertised tool, in which case the
    /// call goes to that server instead (4.7.2).
    ///
    /// 5.3's sandboxing: a tool sourced from a third-party (non-bundled)
    /// Extension is not trusted the way the official one is, so each call
    /// is additionally classified for danger and checked against `scope`'s
    /// permission level before it ever reaches the server. A disposable Sub
    /// Agent cannot itself satisfy an interactive confirmation, so anything
    /// short of `AutoAllow` is denied outright rather than silently granted.
    fn execute_tool(
        &self,
        name: &str,
        input: &serde_json::Value,
        scope: &TaskScope,
    ) -> (String, bool) {
        if let Some(source) = self
            .mcp_tools
            .iter()
            .find(|source| source.definition.name == name)
        {
            if !source.trusted {
                let danger = classify_danger(&format!("call mcp tool {name} with input {input}"));
                if scope.permission_level.decide(&danger, scope.is_read_only())
                    != PermissionDecision::AutoAllow
                {
                    return (
                        format!(
                            "sandboxed: third-party extension tool \"{name}\" requires a higher \
                             permission level or explicit confirmation, which Sub Agent execution \
                             cannot grant; denied."
                        ),
                        true,
                    );
                }
            }

            let mut client = match source.client.lock() {
                Ok(client) => client,
                Err(_) => return ("MCP client lock poisoned".to_string(), true),
            };
            return match client.call_tool(name, input.clone()) {
                Ok(result) => (result.text(), result.is_error),
                Err(e) => (e.to_string(), true),
            };
        }

        match tools::execute(name, input) {
            Ok(content) => (content, false),
            Err(content) => (content, true),
        }
    }
}

impl TaskExecutor for ClaudeTaskExecutor<'_> {
    fn execute(&self, task: &Task, scope: &TaskScope) -> TaskResult {
        if task.description.trim().is_empty() {
            return TaskResult::failure("task description is empty");
        }

        // The Mediator computes `scope` (permission level + allowed tool
        // names) before a Sub Agent is ever generated (4.7.1); this
        // executor only renders that scope into the model-facing system
        // prompt and tool list, it does not decide tool access itself
        // (4.7.2's "tool access itself is scope-limited").
        let system = SystemPromptBuilder::new(scope.permission_level, scope.is_read_only())
            .with_scope_description(scope.describe())
            .with_extensions(&self.extensions)
            .build();

        // Extension-sourced tools (4.7.2) are offered alongside the
        // built-ins regardless of `scope.allowed_tools`: access to them is
        // already gated upstream by each server's `requiredPermissionLevel`
        // (5.1) at connection time, not by the task's read-only flag, since
        // Open String has no per-tool semantics for arbitrary third-party
        // tools to apply that flag to -- except that 5.3's sandboxing keeps
        // *untrusted* (non-bundled) Extension tools out of read-only tasks
        // entirely, mirroring how `write_file`/`run_command` are excluded
        // from `READ_ONLY_TOOLS` below. Trusted (bundled) tools are always
        // offered; untrusted ones offered outside read-only tasks are still
        // gated per-call in `execute_tool`.
        let available_tools: Vec<ToolDefinition> = scope
            .allowed_tools
            .iter()
            .filter_map(|name| tool_for_name(name))
            .chain(
                self.mcp_tools
                    .iter()
                    .filter(|source| source.trusted || !scope.is_read_only())
                    .map(|source| source.definition.clone()),
            )
            .collect();

        let mut messages = vec![Message::user_text(&task.description)];

        for _ in 0..MAX_TOOL_ITERATIONS {
            let response = match self.client.send(&system, &messages, &available_tools) {
                Ok(response) => response,
                Err(ClaudeError::Api { status: 401, .. }) => {
                    return TaskResult::failure(
                        "claude api error: authentication failed; check the stored API key (`auth login`)",
                    );
                }
                Err(err) => return TaskResult::failure(format!("claude api error: {err}")),
            };

            if response.stop_reason != "tool_use" {
                let text: String = response
                    .blocks
                    .into_iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text { text } => Some(text),
                        _ => None,
                    })
                    .collect();
                return if text.is_empty() {
                    TaskResult::failure("sub agent produced no text result")
                } else {
                    TaskResult::success(text)
                };
            }

            let tool_uses: Vec<(String, String, serde_json::Value)> = response
                .blocks
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::ToolUse { id, name, input } => {
                        Some((id.clone(), name.clone(), input.clone()))
                    }
                    _ => None,
                })
                .collect();

            messages.push(Message::assistant_blocks(response.blocks));

            let tool_results = tool_uses
                .into_iter()
                .map(|(id, name, input)| {
                    let (content, is_error) = self.execute_tool(&name, &input, scope);
                    ContentBlock::ToolResult {
                        tool_use_id: id,
                        content,
                        is_error,
                    }
                })
                .collect();
            messages.push(Message::user_blocks(tool_results));
        }

        TaskResult::failure(
            "sub agent exceeded the tool-call iteration limit without completing the task",
        )
    }
}

fn tool_for_name(name: &str) -> Option<ToolDefinition> {
    match name {
        "read_file" => Some(tools::read_file_tool()),
        "write_file" => Some(tools::write_file_tool()),
        "run_command" => Some(tools::run_command_tool()),
        "fetch_url" => Some(tools::fetch_url_tool()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::TaskOutcome;
    use crate::permission::PermissionLevel;
    use httpmock::Method::POST;
    use httpmock::MockServer;

    #[test]
    fn fails_on_empty_description_without_calling_the_api() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST).path("/v1/messages");
            then.status(200);
        });

        let client = ClaudeClient::new("sk-ant-test").with_base_url(server.base_url());
        let executor = ClaudeTaskExecutor::new(&client);
        let task = Task::new("  ");
        let scope = TaskScope::for_task(&task, PermissionLevel::GodMode);

        let result = executor.execute(&task, &scope);

        assert_eq!(result.outcome, TaskOutcome::Failure);
        mock.assert_hits(0);
    }

    #[test]
    fn read_only_task_sends_the_read_only_system_suffix() {
        let task = Task::read_only("inspect config");
        let scope = TaskScope::for_task(&task, PermissionLevel::HighProtect);

        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST).path("/v1/messages").matches(|req| {
                body_contains(req, "disposable Sub Agent")
                    && body_contains(req, "high protect")
                    && body_contains(req, "Tools available for this task: read_file, fetch_url")
                    && body_contains(req, "read-only: do not perform")
            });
            then.status(200).json_body(serde_json::json!({
                "content": [{"type": "text", "text": "done"}]
            }));
        });

        let client = ClaudeClient::new("sk-ant-test").with_base_url(server.base_url());
        let executor = ClaudeTaskExecutor::new(&client);

        let result = executor.execute(&task, &scope);

        mock.assert();
        assert_eq!(result.outcome, TaskOutcome::Success);
        assert_eq!(result.summary, "done");
    }

    #[test]
    fn api_error_becomes_a_failure_result() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/v1/messages");
            then.status(500).json_body(serde_json::json!({
                "error": {"message": "internal error"}
            }));
        });

        let client = ClaudeClient::new("sk-ant-test").with_base_url(server.base_url());
        let executor = ClaudeTaskExecutor::new(&client);
        let task = Task::new("do something");
        let scope = TaskScope::for_task(&task, PermissionLevel::GodMode);

        let result = executor.execute(&task, &scope);

        assert_eq!(result.outcome, TaskOutcome::Failure);
        assert!(result.summary.contains("internal error"));
    }

    fn body_contains(req: &httpmock::prelude::HttpMockRequest, needle: &str) -> bool {
        req.body
            .as_deref()
            .map(|b| String::from_utf8_lossy(b).contains(needle))
            .unwrap_or(false)
    }

    /// Builds an `McpToolSource` backed by a real `McpClient` handshaken
    /// against canned JSON-RPC responses (no subprocess): the first line
    /// answers `initialize`, the second answers the one `tools/call` a test
    /// may make. Tests that never reach the actual server call (e.g. the
    /// sandboxing denial, which returns before locking the client) only
    /// need the handshake to succeed.
    fn mcp_tool_source(name: &str, trusted: bool) -> McpToolSource {
        let stdout = std::io::Cursor::new(
            format!(
                "{}\n{}\n",
                r#"{"jsonrpc":"2.0","id":1,"result":{}}"#,
                r#"{"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":"ok"}],"isError":false}}"#
            )
            .into_bytes(),
        );
        let client =
            crate::mcp::McpClient::from_io(None, Box::new(std::io::sink()), Box::new(stdout))
                .expect("handshake against canned responses should succeed");
        McpToolSource {
            definition: ToolDefinition {
                name: name.to_string(),
                description: String::new(),
                input_schema: serde_json::json!({"type": "object"}),
            },
            client: std::sync::Arc::new(std::sync::Mutex::new(client)),
            trusted,
        }
    }

    #[test]
    fn drives_a_tool_use_loop_then_returns_the_final_text() {
        let path = std::env::temp_dir().join("open_string_claude_executor_test_read.txt");
        std::fs::write(&path, "hello from disk").unwrap();
        let path_str = path.to_string_lossy().replace('\\', "\\\\");

        let server = MockServer::start();
        let tool_use_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/v1/messages")
                .matches(|req| !body_contains(req, "tool_result"));
            then.status(200).json_body(serde_json::json!({
                "content": [{"type": "tool_use", "id": "toolu_1", "name": "read_file", "input": {"path": path_str}}],
                "stop_reason": "tool_use"
            }));
        });
        let final_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/v1/messages")
                .matches(|req| body_contains(req, "tool_result"));
            then.status(200).json_body(serde_json::json!({
                "content": [{"type": "text", "text": "file read successfully"}],
                "stop_reason": "end_turn"
            }));
        });

        let client = ClaudeClient::new("sk-ant-test").with_base_url(server.base_url());
        let executor = ClaudeTaskExecutor::new(&client);
        let task = Task::new("read the file");
        let scope = TaskScope::for_task(&task, PermissionLevel::GodMode);

        let result = executor.execute(&task, &scope);

        tool_use_mock.assert();
        final_mock.assert();
        assert_eq!(result.outcome, TaskOutcome::Success);
        assert_eq!(result.summary, "file read successfully");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn read_only_task_only_offers_the_read_file_tool() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST).path("/v1/messages").matches(|req| {
                body_contains(req, "\"read_file\"")
                    && !body_contains(req, "write_file")
                    && !body_contains(req, "run_command")
            });
            then.status(200).json_body(serde_json::json!({
                "content": [{"type": "text", "text": "ok"}],
                "stop_reason": "end_turn"
            }));
        });

        let client = ClaudeClient::new("sk-ant-test").with_base_url(server.base_url());
        let executor = ClaudeTaskExecutor::new(&client);
        let task = Task::read_only("inspect config");
        let scope = TaskScope::for_task(&task, PermissionLevel::HighProtect);

        let result = executor.execute(&task, &scope);

        mock.assert();
        assert_eq!(result.outcome, TaskOutcome::Success);
    }

    #[test]
    fn read_only_task_does_not_offer_an_untrusted_mcp_tool() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/v1/messages")
                .matches(|req| !body_contains(req, "third_party_delete"));
            then.status(200).json_body(serde_json::json!({
                "content": [{"type": "text", "text": "ok"}],
                "stop_reason": "end_turn"
            }));
        });

        let client = ClaudeClient::new("sk-ant-test").with_base_url(server.base_url());
        let executor = ClaudeTaskExecutor::new(&client)
            .with_mcp_tools(vec![mcp_tool_source("third_party_delete", false)]);
        let task = Task::read_only("inspect config");
        let scope = TaskScope::for_task(&task, PermissionLevel::GodMode);

        let result = executor.execute(&task, &scope);

        mock.assert();
        assert_eq!(result.outcome, TaskOutcome::Success);
    }

    #[test]
    fn untrusted_mcp_tool_call_is_sandboxed_when_not_auto_allowed() {
        let server = MockServer::start();
        let tool_use_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/v1/messages")
                .matches(|req| !body_contains(req, "tool_result"));
            then.status(200).json_body(serde_json::json!({
                "content": [{"type": "tool_use", "id": "toolu_1", "name": "third_party_delete", "input": {"target": "everything"}}],
                "stop_reason": "tool_use"
            }));
        });
        let final_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/v1/messages")
                .matches(|req| body_contains(req, "sandboxed"));
            then.status(200).json_body(serde_json::json!({
                "content": [{"type": "text", "text": "could not delete"}],
                "stop_reason": "end_turn"
            }));
        });

        let client = ClaudeClient::new("sk-ant-test").with_base_url(server.base_url());
        let executor = ClaudeTaskExecutor::new(&client)
            .with_mcp_tools(vec![mcp_tool_source("third_party_delete", false)]);
        let task = Task::new("clean up");
        // LowSecurity auto-allows only when no danger is classified; the
        // tool name/input here trips the "delete" keyword, so this should
        // require confirmation -- and a Sub Agent can't grant that, so the
        // sandboxing gate must deny the call (reporting "sandboxed" back to
        // the model) rather than ever reaching the server.
        let scope = TaskScope::for_task(&task, PermissionLevel::LowSecurity);

        let result = executor.execute(&task, &scope);

        tool_use_mock.assert();
        final_mock.assert();
        assert_eq!(result.outcome, TaskOutcome::Success);
        assert_eq!(result.summary, "could not delete");
    }

    #[test]
    fn trusted_mcp_tool_call_bypasses_the_sandboxing_gate() {
        let server = MockServer::start();
        let tool_use_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/v1/messages")
                .matches(|req| !body_contains(req, "tool_result"));
            then.status(200).json_body(serde_json::json!({
                "content": [{"type": "tool_use", "id": "toolu_1", "name": "t0k3n_delete", "input": {"target": "stale cache"}}],
                "stop_reason": "tool_use"
            }));
        });
        let final_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/v1/messages")
                .matches(|req| body_contains(req, "tool_result"));
            then.status(200).json_body(serde_json::json!({
                "content": [{"type": "text", "text": "done"}],
                "stop_reason": "end_turn"
            }));
        });

        let client = ClaudeClient::new("sk-ant-test").with_base_url(server.base_url());
        let executor = ClaudeTaskExecutor::new(&client)
            .with_mcp_tools(vec![mcp_tool_source("t0k3n_delete", true)]);
        let task = Task::new("clean up");
        // Same dangerous-sounding tool name/input as the untrusted test, but
        // sourced from the bundled (trusted) Extension this time -- the
        // call should reach the server instead of being sandboxed, even
        // under LowSecurity.
        let scope = TaskScope::for_task(&task, PermissionLevel::LowSecurity);

        let result = executor.execute(&task, &scope);

        tool_use_mock.assert();
        final_mock.assert();
        assert_eq!(result.outcome, TaskOutcome::Success);
        assert_eq!(result.summary, "done");
    }

    #[test]
    fn exceeding_the_tool_iteration_limit_returns_a_failure() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST).path("/v1/messages");
            then.status(200).json_body(serde_json::json!({
                "content": [{"type": "tool_use", "id": "toolu_x", "name": "read_file", "input": {"path": "nonexistent.txt"}}],
                "stop_reason": "tool_use"
            }));
        });

        let client = ClaudeClient::new("sk-ant-test").with_base_url(server.base_url());
        let executor = ClaudeTaskExecutor::new(&client);
        let task = Task::new("loop forever");
        let scope = TaskScope::for_task(&task, PermissionLevel::GodMode);

        let result = executor.execute(&task, &scope);

        assert_eq!(result.outcome, TaskOutcome::Failure);
        assert!(result.summary.contains("iteration limit"));
        mock.assert_hits(MAX_TOOL_ITERATIONS);
    }
}
