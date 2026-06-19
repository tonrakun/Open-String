use serde::{Deserialize, Serialize};

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const DEFAULT_MODEL: &str = "claude-sonnet-4-6";
const DEFAULT_MAX_TOKENS: u32 = 4096;
const ANTHROPIC_VERSION: &str = "2023-06-01";

#[derive(Debug, thiserror::Error)]
pub enum ClaudeError {
    #[error("request to Claude API failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("Claude API returned an error ({status}): {message}")]
    Api { status: u16, message: String },
    #[error("unexpected response from Claude API: {0}")]
    UnexpectedResponse(String),
}

/// Synchronous client for the Anthropic Messages API. Blocking on purpose:
/// the rest of the codebase (Mediator/Sub Agent dispatch, `std::thread::scope`
/// for parallel runs) is synchronous, so this avoids pulling an async runtime
/// into the whole crate for one HTTP call.
pub struct ClaudeClient {
    api_key: String,
    base_url: String,
    model: String,
    max_tokens: u32,
    http: reqwest::blocking::Client,
}

impl ClaudeClient {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: DEFAULT_BASE_URL.to_string(),
            model: DEFAULT_MODEL.to_string(),
            max_tokens: DEFAULT_MAX_TOKENS,
            http: reqwest::blocking::Client::new(),
        }
    }

    #[cfg(test)]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// Sends a multi-turn request that may include tool definitions, and
    /// returns the raw response blocks (text and/or tool-use requests) plus
    /// the stop reason, so the caller can drive a tool-execution loop
    /// (Sub Agent tool use, 4.7.2).
    pub fn send(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> Result<ClaudeResponse, ClaudeError> {
        let request = MessageRequest {
            model: &self.model,
            max_tokens: self.max_tokens,
            system,
            messages,
            tools: if tools.is_empty() { None } else { Some(tools) },
        };

        let response = self
            .http
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&request)
            .send()?;

        let status = response.status();
        let body = response.text()?;

        if !status.is_success() {
            let message = serde_json::from_str::<ErrorResponse>(&body)
                .map(|e| e.error.message)
                .unwrap_or(body);
            return Err(ClaudeError::Api {
                status: status.as_u16(),
                message,
            });
        }

        let parsed: RawMessageResponse = serde_json::from_str(&body)
            .map_err(|e| ClaudeError::UnexpectedResponse(e.to_string()))?;

        Ok(ClaudeResponse {
            blocks: parsed.content,
            stop_reason: parsed.stop_reason.unwrap_or_default(),
        })
    }
}

/// A tool a Sub Agent may call (4.7.2). `input_schema` is a JSON Schema
/// object describing the tool's arguments, per the Anthropic Messages API.
#[derive(Debug, Clone, Serialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// One turn of a conversation. Built up across a tool-use loop: the
/// assistant's `tool_use` blocks and the caller's `tool_result` blocks both
/// round-trip through here so the model can see its own prior tool calls.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: Vec<ContentBlock>,
}

impl Message {
    pub fn user_text(text: impl Into<String>) -> Self {
        Self {
            role: "user".to_string(),
            content: vec![ContentBlock::Text { text: text.into() }],
        }
    }

    pub fn assistant_text(text: impl Into<String>) -> Self {
        Self {
            role: "assistant".to_string(),
            content: vec![ContentBlock::Text { text: text.into() }],
        }
    }

    pub fn assistant_blocks(content: Vec<ContentBlock>) -> Self {
        Self {
            role: "assistant".to_string(),
            content,
        }
    }

    pub fn user_blocks(content: Vec<ContentBlock>) -> Self {
        Self {
            role: "user".to_string(),
            content,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        is_error: bool,
    },
}

/// The result of one `send` call: the response's content blocks plus its
/// stop reason (e.g. `"tool_use"` when the model wants to call a tool,
/// `"end_turn"` when it has produced a final answer).
#[derive(Debug)]
pub struct ClaudeResponse {
    pub blocks: Vec<ContentBlock>,
    pub stop_reason: String,
}

#[derive(Debug, Serialize)]
struct MessageRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    system: &'a str,
    messages: &'a [Message],
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<&'a [ToolDefinition]>,
}

#[derive(Debug, Deserialize)]
struct RawMessageResponse {
    content: Vec<ContentBlock>,
    stop_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ErrorResponse {
    error: ErrorDetail,
}

#[derive(Debug, Deserialize)]
struct ErrorDetail {
    message: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::Method::POST;
    use httpmock::MockServer;

    #[test]
    fn send_returns_text_blocks_and_stop_reason_on_success() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/v1/messages")
                .header("x-api-key", "sk-ant-test")
                .header("anthropic-version", ANTHROPIC_VERSION);
            then.status(200).json_body(serde_json::json!({
                "content": [
                    {"type": "text", "text": "hello "},
                    {"type": "text", "text": "world"}
                ],
                "stop_reason": "end_turn"
            }));
        });

        let client = ClaudeClient::new("sk-ant-test").with_base_url(server.base_url());
        let messages = vec![Message::user_text("say hello world")];
        let response = client.send("be terse", &messages, &[]).unwrap();

        mock.assert();
        assert_eq!(response.stop_reason, "end_turn");
        let text: String = response
            .blocks
            .into_iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text),
                _ => None,
            })
            .collect();
        assert_eq!(text, "hello world");
    }

    #[test]
    fn send_parses_tool_use_blocks() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/v1/messages");
            then.status(200).json_body(serde_json::json!({
                "content": [
                    {"type": "tool_use", "id": "toolu_1", "name": "read_file", "input": {"path": "a.txt"}}
                ],
                "stop_reason": "tool_use"
            }));
        });

        let client = ClaudeClient::new("sk-ant-test").with_base_url(server.base_url());
        let messages = vec![Message::user_text("read a.txt")];
        let response = client.send("system", &messages, &[]).unwrap();

        assert_eq!(response.stop_reason, "tool_use");
        match &response.blocks[0] {
            ContentBlock::ToolUse { id, name, input } => {
                assert_eq!(id, "toolu_1");
                assert_eq!(name, "read_file");
                assert_eq!(input["path"], "a.txt");
            }
            other => panic!("expected ContentBlock::ToolUse, got {other:?}"),
        }
    }

    #[test]
    fn send_surfaces_api_error_message_on_non_2xx() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/v1/messages");
            then.status(401).json_body(serde_json::json!({
                "type": "error",
                "error": {"type": "authentication_error", "message": "invalid x-api-key"}
            }));
        });

        let client = ClaudeClient::new("sk-ant-bad").with_base_url(server.base_url());
        let messages = vec![Message::user_text("hi")];
        let err = client.send("system", &messages, &[]).unwrap_err();

        match err {
            ClaudeError::Api { status, message } => {
                assert_eq!(status, 401);
                assert_eq!(message, "invalid x-api-key");
            }
            other => panic!("expected ClaudeError::Api, got {other:?}"),
        }
    }
}
