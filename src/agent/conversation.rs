use super::Task;
use crate::llm::{ClaudeClient, ClaudeError, ContentBlock, Message, ToolDefinition};

/// The Mediator's sole interactive system prompt (4.7.1): it talks to the
/// user directly, but never executes work itself. The `delegate_tasks` tool
/// is the only path from free-form user input to a `Task` -- there is no
/// other way for this module to produce one.
const MEDIATOR_CHAT_SYSTEM_PROMPT: &str = "You are the Mediator in the Open String system: \
the only component that converses with the user in natural language. You never execute file \
operations, run shell commands, fetch URLs, or otherwise perform work yourself -- that \
capability belongs solely to disposable Sub Agents you delegate to. When the user's request \
requires actual execution (reading or writing files, running commands, fetching web content, \
or any other concrete action), call the `delegate_tasks` tool with one or more self-contained \
task descriptions for Sub Agents to carry out; mark a task's `read_only` field true only if it \
performs no write, delete, send, or other irreversible action. When the user explicitly asks \
to connect, add, or use a new MCP server Extension by name (e.g. \"I want to use the X MCP \
server\"), call `propose_extension` instead -- this only stages the request for the user's \
explicit confirmation and never connects anything itself; never call it unless the user named \
a specific server they want. For anything that needs no execution -- greetings, questions \
about your own capabilities, clarification requests, or anything answerable from the \
conversation alone -- respond directly in natural language without calling either tool.";

/// What came out of interpreting one user message: either the Mediator
/// answers directly, it has decomposed the request into `Task`s for the
/// caller to dispatch through `Mediator::dispatch_many`, or it is proposing
/// a new MCP server Extension for the caller to confirm and add (5.4).
pub enum MediatorTurn {
    Direct(String),
    Delegated(Vec<Task>),
    ProposeExtension(ProposedExtension),
}

/// An Extension connection the Mediator wants to add, awaiting the user's
/// explicit confirmation (5.4's "確認を得た上で設定変更を実行するフロー").
pub struct ProposedExtension {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub reason: String,
}

pub fn delegate_tasks_tool() -> ToolDefinition {
    ToolDefinition {
        name: "delegate_tasks".to_string(),
        description: "Delegate one or more concrete units of work to disposable Sub Agents. \
            Call this only when the user's request requires actual execution; never to merely \
            describe or narrate what you are about to do."
            .to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "tasks": {
                    "type": "array",
                    "minItems": 1,
                    "items": {
                        "type": "object",
                        "properties": {
                            "description": {
                                "type": "string",
                                "description": "Self-contained instruction for a single Sub Agent, e.g. \"read the contents of config.toml\"."
                            },
                            "read_only": {
                                "type": "boolean",
                                "description": "true only if this task performs no write, delete, send, or other irreversible action."
                            }
                        },
                        "required": ["description", "read_only"]
                    }
                }
            },
            "required": ["tasks"]
        }),
    }
}

pub fn propose_extension_tool() -> ToolDefinition {
    ToolDefinition {
        name: "propose_extension".to_string(),
        description: "Stage a request to connect a new MCP server Extension for the user's \
            explicit confirmation. Call this only when the user named a specific server they \
            want to use; it never connects anything by itself."
            .to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Short identifier for the server, used as its .mcp.json key."
                },
                "command": {
                    "type": "string",
                    "description": "Executable to launch the server with."
                },
                "args": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Arguments passed to the command, if any."
                },
                "reason": {
                    "type": "string",
                    "description": "One sentence explaining why this server is being proposed."
                }
            },
            "required": ["name", "command", "reason"]
        }),
    }
}

/// Interprets the latest user message against the conversation so far,
/// deciding whether it can be answered directly or must be decomposed into
/// `Task`s. This is the Mediator's own model call -- distinct from a Sub
/// Agent's tool-use loop (4.7.2) -- and is the only place user input turns
/// into `Task`s, satisfying 4.7.1's "唯一の対話主体" requirement.
pub fn plan(
    client: &ClaudeClient,
    history: &[Message],
    user_input: &str,
) -> Result<MediatorTurn, ClaudeError> {
    let mut messages = history.to_vec();
    messages.push(Message::user_text(user_input));

    let response = client.send(
        MEDIATOR_CHAT_SYSTEM_PROMPT,
        &messages,
        &[delegate_tasks_tool(), propose_extension_tool()],
    )?;

    if response.stop_reason != "tool_use" {
        let text: String = response
            .blocks
            .into_iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        return Ok(MediatorTurn::Direct(text));
    }

    let tool_use = response
        .blocks
        .into_iter()
        .find_map(|block| match block {
            ContentBlock::ToolUse { name, input, .. }
                if name == "delegate_tasks" || name == "propose_extension" =>
            {
                Some((name, input))
            }
            _ => None,
        })
        .ok_or_else(|| {
            ClaudeError::UnexpectedResponse(
                "model signalled tool_use but did not call a known tool".to_string(),
            )
        })?;

    if tool_use.0 == "propose_extension" {
        let parsed: ProposedExtensionInput = serde_json::from_value(tool_use.1).map_err(|e| {
            ClaudeError::UnexpectedResponse(format!("malformed propose_extension input: {e}"))
        })?;
        return Ok(MediatorTurn::ProposeExtension(ProposedExtension {
            name: parsed.name,
            command: parsed.command,
            args: parsed.args,
            reason: parsed.reason,
        }));
    }

    let parsed: DelegatedTasks = serde_json::from_value(tool_use.1).map_err(|e| {
        ClaudeError::UnexpectedResponse(format!("malformed delegate_tasks input: {e}"))
    })?;

    let tasks = parsed
        .tasks
        .into_iter()
        .map(|t| {
            if t.read_only {
                Task::read_only(t.description)
            } else {
                Task::new(t.description)
            }
        })
        .collect();

    Ok(MediatorTurn::Delegated(tasks))
}

#[derive(serde::Deserialize)]
struct DelegatedTasks {
    tasks: Vec<DelegatedTask>,
}

#[derive(serde::Deserialize)]
struct DelegatedTask {
    description: String,
    #[serde(default)]
    read_only: bool,
}

#[derive(serde::Deserialize)]
struct ProposedExtensionInput {
    name: String,
    command: String,
    #[serde(default)]
    args: Vec<String>,
    reason: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::Method::POST;
    use httpmock::MockServer;

    #[test]
    fn direct_reply_when_model_does_not_call_the_tool() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/v1/messages");
            then.status(200).json_body(serde_json::json!({
                "content": [{"type": "text", "text": "Hi! I'm the Open String Mediator."}],
                "stop_reason": "end_turn"
            }));
        });

        let client = ClaudeClient::new("sk-ant-test").with_base_url(server.base_url());
        let turn = plan(&client, &[], "hello").unwrap();

        match turn {
            MediatorTurn::Direct(text) => assert_eq!(text, "Hi! I'm the Open String Mediator."),
            MediatorTurn::Delegated(_) => panic!("expected a direct reply"),
            MediatorTurn::ProposeExtension(_) => panic!("expected a direct reply"),
        }
    }

    #[test]
    fn delegated_tasks_are_parsed_with_their_read_only_flags() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/v1/messages");
            then.status(200).json_body(serde_json::json!({
                "content": [{
                    "type": "tool_use",
                    "id": "toolu_1",
                    "name": "delegate_tasks",
                    "input": {
                        "tasks": [
                            {"description": "read config.toml", "read_only": true},
                            {"description": "delete the temp directory", "read_only": false}
                        ]
                    }
                }],
                "stop_reason": "tool_use"
            }));
        });

        let client = ClaudeClient::new("sk-ant-test").with_base_url(server.base_url());
        let turn = plan(&client, &[], "clean up and check the config").unwrap();

        match turn {
            MediatorTurn::Delegated(tasks) => {
                assert_eq!(tasks.len(), 2);
                assert_eq!(tasks[0].description, "read config.toml");
                assert!(tasks[0].read_only);
                assert_eq!(tasks[1].description, "delete the temp directory");
                assert!(!tasks[1].read_only);
            }
            MediatorTurn::Direct(_) => panic!("expected delegated tasks"),
            MediatorTurn::ProposeExtension(_) => panic!("expected delegated tasks"),
        }
    }

    #[test]
    fn malformed_tool_input_is_a_claude_error() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/v1/messages");
            then.status(200).json_body(serde_json::json!({
                "content": [{
                    "type": "tool_use",
                    "id": "toolu_1",
                    "name": "delegate_tasks",
                    "input": {"not_tasks": []}
                }],
                "stop_reason": "tool_use"
            }));
        });

        let client = ClaudeClient::new("sk-ant-test").with_base_url(server.base_url());
        let result = plan(&client, &[], "do something");

        assert!(matches!(result, Err(ClaudeError::UnexpectedResponse(_))));
    }

    #[test]
    fn propose_extension_tool_use_is_parsed_into_a_proposal() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/v1/messages");
            then.status(200).json_body(serde_json::json!({
                "content": [{
                    "type": "tool_use",
                    "id": "toolu_1",
                    "name": "propose_extension",
                    "input": {
                        "name": "weather",
                        "command": "weather-mcp",
                        "args": ["--stdio"],
                        "reason": "user asked to check the forecast"
                    }
                }],
                "stop_reason": "tool_use"
            }));
        });

        let client = ClaudeClient::new("sk-ant-test").with_base_url(server.base_url());
        let turn = plan(&client, &[], "I want to use the weather MCP server").unwrap();

        match turn {
            MediatorTurn::ProposeExtension(proposal) => {
                assert_eq!(proposal.name, "weather");
                assert_eq!(proposal.command, "weather-mcp");
                assert_eq!(proposal.args, vec!["--stdio".to_string()]);
                assert_eq!(proposal.reason, "user asked to check the forecast");
            }
            _ => panic!("expected a proposed extension"),
        }
    }
}
