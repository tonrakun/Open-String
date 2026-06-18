use super::{Task, TaskExecutor, TaskResult};
use crate::llm::{ClaudeClient, ClaudeError};

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

/// Real Sub Agent executor backed by the Claude API (replaces
/// `EchoTaskExecutor` for production use).
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

        match self.client.complete(&system, &task.description) {
            Ok(text) => TaskResult::success(text),
            Err(ClaudeError::Api { status: 401, .. }) => TaskResult::failure(
                "claude api error: authentication failed; check the stored API key (`auth login`)",
            ),
            Err(err) => TaskResult::failure(format!("claude api error: {err}")),
        }
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
}
