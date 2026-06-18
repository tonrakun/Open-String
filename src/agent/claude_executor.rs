use super::tools;
use super::{Task, TaskExecutor, TaskResult};
use crate::llm::{ClaudeClient, ClaudeError, ContentBlock, Message, ToolDefinition};

/// Enforces 4.7.2's narration ban: a Sub Agent must report only the work
/// outcome, never describe what it is about to do or is doing.
const SUB_AGENT_SYSTEM_PROMPT: &str = "You are a disposable Sub Agent in the Open String \
system. You execute exactly one task and then terminate; you carry no state between \
invocations. Never narrate, explain, or describe what you are about to do or are doing \
(for example, never say things like \"I will search the web\" or \"Reading the file now\"). \
Respond only with the final result: the work outcome, any produced artifact paths, state \
changes, or error information. Compress your response to whatever is minimally sufficient \
for the Mediator to make its next decision.";

const READ_ONLY_SUFFIX: &str = "\n\nThis task is read-only: do not perform any write, \
delete, send, or otherwise irreversible action.";

/// Upper bound on tool-call round trips for a single task, so a confused or
/// looping model can't keep a disposable Sub Agent running forever (4.7.2).
const MAX_TOOL_ITERATIONS: usize = 8;

/// Real Sub Agent executor backed by the Claude API (replaces
/// `EchoTaskExecutor` for production use). Drives a tool-use loop so the
/// Sub Agent can actually perform file operations and run commands, not
/// just produce text (4.7.2).
pub struct ClaudeTaskExecutor<'a> {
    client: &'a ClaudeClient,
}

impl<'a> ClaudeTaskExecutor<'a> {
    pub fn new(client: &'a ClaudeClient) -> Self {
        Self { client }
    }
}

impl TaskExecutor for ClaudeTaskExecutor<'_> {
    fn execute(&self, task: &Task) -> TaskResult {
        if task.description.trim().is_empty() {
            return TaskResult::failure("task description is empty");
        }

        let system = if task.read_only {
            format!("{SUB_AGENT_SYSTEM_PROMPT}{READ_ONLY_SUFFIX}")
        } else {
            SUB_AGENT_SYSTEM_PROMPT.to_string()
        };

        // A read-only task is restricted to the read_file tool at the
        // source: write_file/run_command are simply never offered, so the
        // model has no path to an irreversible action regardless of what it
        // is asked to do (4.7.2's "tool access itself is scope-limited").
        let available_tools: Vec<ToolDefinition> = if task.read_only {
            vec![tools::read_file_tool()]
        } else {
            vec![
                tools::read_file_tool(),
                tools::write_file_tool(),
                tools::run_command_tool(),
            ]
        };

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::TaskOutcome;
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

        let result = executor.execute(&Task::new("  "));

        assert_eq!(result.outcome, TaskOutcome::Failure);
        mock.assert_hits(0);
    }

    #[test]
    fn read_only_task_sends_the_read_only_system_suffix() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/v1/messages")
                .json_body_partial(serde_json::json!({"system": format!("{SUB_AGENT_SYSTEM_PROMPT}{READ_ONLY_SUFFIX}")}).to_string());
            then.status(200).json_body(serde_json::json!({
                "content": [{"type": "text", "text": "done"}]
            }));
        });

        let client = ClaudeClient::new("sk-ant-test").with_base_url(server.base_url());
        let executor = ClaudeTaskExecutor::new(&client);

        let result = executor.execute(&Task::read_only("inspect config"));

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

        let result = executor.execute(&Task::new("do something"));

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

        let result = executor.execute(&Task::new("read the file"));

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

        let result = executor.execute(&Task::read_only("inspect config"));

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

        let result = executor.execute(&Task::new("loop forever"));

        assert_eq!(result.outcome, TaskOutcome::Failure);
        assert!(result.summary.contains("iteration limit"));
        mock.assert_hits(MAX_TOOL_ITERATIONS);
    }
}
