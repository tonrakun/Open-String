use super::system_prompt::{ExtensionInfo, SystemPromptBuilder};
use super::tools;
use super::{Task, TaskExecutor, TaskResult, TaskScope};
use crate::llm::{ClaudeClient, ClaudeError, ContentBlock, Message, ToolDefinition};

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
}

impl<'a> ClaudeTaskExecutor<'a> {
    pub fn new(client: &'a ClaudeClient) -> Self {
        Self {
            client,
            extensions: Vec::new(),
        }
    }

    /// Registers the Extensions connected for this run so their usage
    /// instructions are injected into the system prompt (4.2.1). Extensions
    /// not passed here contribute no prompt fragment.
    pub fn with_extensions(mut self, extensions: Vec<ExtensionInfo>) -> Self {
        self.extensions = extensions;
        self
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

        let available_tools: Vec<ToolDefinition> = scope
            .allowed_tools
            .iter()
            .filter_map(|name| tool_for_name(name))
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
                .map(|(id, name, input)| match tools::execute(&name, &input) {
                    Ok(content) => ContentBlock::ToolResult {
                        tool_use_id: id,
                        content,
                        is_error: false,
                    },
                    Err(content) => ContentBlock::ToolResult {
                        tool_use_id: id,
                        content,
                        is_error: true,
                    },
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
