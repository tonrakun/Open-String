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

    /// Sends a single-turn request (system prompt + one user message) and
    /// returns the concatenated text of the response.
    pub fn complete(&self, system: &str, user_message: &str) -> Result<String, ClaudeError> {
        let request = MessageRequest {
            model: &self.model,
            max_tokens: self.max_tokens,
            system,
            messages: vec![MessageParam {
                role: "user",
                content: user_message,
            }],
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

        let parsed: MessageResponse = serde_json::from_str(&body)
            .map_err(|e| ClaudeError::UnexpectedResponse(e.to_string()))?;

        let text: String = parsed
            .content
            .into_iter()
            .filter(|block| block.block_type == "text")
            .filter_map(|block| block.text)
            .collect();

        if text.is_empty() {
            return Err(ClaudeError::UnexpectedResponse(
                "response contained no text content".to_string(),
            ));
        }

        Ok(text)
    }
}

#[derive(Debug, Serialize)]
struct MessageRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    system: &'a str,
    messages: Vec<MessageParam<'a>>,
}

#[derive(Debug, Serialize)]
struct MessageParam<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Debug, Deserialize)]
struct MessageResponse {
    content: Vec<ContentBlock>,
}

#[derive(Debug, Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    block_type: String,
    text: Option<String>,
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
    fn complete_returns_concatenated_text_blocks_on_success() {
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
                ]
            }));
        });

        let client = ClaudeClient::new("sk-ant-test").with_base_url(server.base_url());
        let result = client.complete("be terse", "say hello world").unwrap();

        mock.assert();
        assert_eq!(result, "hello world");
    }

    #[test]
    fn complete_surfaces_api_error_message_on_non_2xx() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/v1/messages");
            then.status(401).json_body(serde_json::json!({
                "type": "error",
                "error": {"type": "authentication_error", "message": "invalid x-api-key"}
            }));
        });

        let client = ClaudeClient::new("sk-ant-bad").with_base_url(server.base_url());
        let err = client.complete("system", "hi").unwrap_err();

        match err {
            ClaudeError::Api { status, message } => {
                assert_eq!(status, 401);
                assert_eq!(message, "invalid x-api-key");
            }
            other => panic!("expected ClaudeError::Api, got {other:?}"),
        }
    }

    #[test]
    fn complete_errors_on_response_with_no_text_content() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/v1/messages");
            then.status(200)
                .json_body(serde_json::json!({"content": []}));
        });

        let client = ClaudeClient::new("sk-ant-test").with_base_url(server.base_url());
        let err = client.complete("system", "hi").unwrap_err();

        assert!(matches!(err, ClaudeError::UnexpectedResponse(_)));
    }
}
