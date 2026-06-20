//! Telegram Bot API adapter: long polling via `getUpdates`, replies via
//! `sendMessage`. The simplest of the three platforms to support, since it
//! needs neither a hosted webhook (unlike LINE) nor a persistent socket
//! (unlike Discord).

use super::{ChatGateway, GatewayError, IncomingMessage};
use serde::Deserialize;

pub struct TelegramGateway {
    base_url: String,
    offset: i64,
    client: reqwest::blocking::Client,
}

impl TelegramGateway {
    pub fn new(token: &str) -> Self {
        Self::with_base_url(format!("https://api.telegram.org/bot{token}"))
    }

    fn with_base_url(base_url: String) -> Self {
        Self {
            base_url,
            offset: 0,
            client: reqwest::blocking::Client::new(),
        }
    }
}

#[derive(Deserialize)]
struct GetUpdatesResponse {
    ok: bool,
    result: Vec<TelegramUpdate>,
}

#[derive(Deserialize)]
struct TelegramUpdate {
    update_id: i64,
    message: Option<TelegramMessage>,
}

#[derive(Deserialize)]
struct TelegramMessage {
    chat: TelegramChat,
    from: Option<TelegramUser>,
    text: Option<String>,
}

#[derive(Deserialize)]
struct TelegramChat {
    id: i64,
}

#[derive(Deserialize)]
struct TelegramUser {
    id: i64,
}

/// 10s long-poll: short enough that the gateway runner's loop stays
/// responsive (e.g. to a future shutdown signal) without hammering the
/// API with zero-wait polling.
const POLL_TIMEOUT_SECS: &str = "10";

impl ChatGateway for TelegramGateway {
    fn platform(&self) -> &'static str {
        "telegram"
    }

    fn poll_incoming(&mut self) -> Result<Vec<IncomingMessage>, GatewayError> {
        let response = self
            .client
            .get(format!("{}/getUpdates", self.base_url))
            .query(&[
                ("offset", (self.offset + 1).to_string()),
                ("timeout", POLL_TIMEOUT_SECS.to_string()),
            ])
            .send()
            .map_err(|e| GatewayError::Network(e.to_string()))?;
        let body: GetUpdatesResponse = response
            .json()
            .map_err(|e| GatewayError::Protocol(e.to_string()))?;
        if !body.ok {
            return Err(GatewayError::Protocol(
                "telegram getUpdates returned ok=false".to_string(),
            ));
        }

        let mut messages = Vec::new();
        for update in body.result {
            self.offset = self.offset.max(update.update_id);
            let Some(message) = update.message else {
                continue;
            };
            let (Some(from), Some(text)) = (message.from, message.text) else {
                continue;
            };
            messages.push(IncomingMessage {
                sender_id: from.id.to_string(),
                chat_id: message.chat.id.to_string(),
                text,
            });
        }
        Ok(messages)
    }

    fn send(&mut self, chat_id: &str, text: &str) -> Result<(), GatewayError> {
        self.client
            .post(format!("{}/sendMessage", self.base_url))
            .json(&serde_json::json!({"chat_id": chat_id, "text": text}))
            .send()
            .map_err(|e| GatewayError::Network(e.to_string()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::Method::{GET, POST};
    use httpmock::MockServer;

    #[test]
    fn poll_incoming_parses_text_messages_and_advances_the_offset() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/getUpdates");
            then.status(200).json_body(serde_json::json!({
                "ok": true,
                "result": [
                    {"update_id": 5, "message": {"chat": {"id": 100}, "from": {"id": 42}, "text": "hello"}}
                ]
            }));
        });

        let mut gateway = TelegramGateway::with_base_url(server.base_url());
        let messages = gateway.poll_incoming().unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].sender_id, "42");
        assert_eq!(messages[0].chat_id, "100");
        assert_eq!(messages[0].text, "hello");
        assert_eq!(gateway.offset, 5);
    }

    #[test]
    fn poll_incoming_skips_updates_without_a_text_message() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/getUpdates");
            then.status(200).json_body(serde_json::json!({
                "ok": true,
                "result": [{"update_id": 1}]
            }));
        });

        let mut gateway = TelegramGateway::with_base_url(server.base_url());
        assert!(gateway.poll_incoming().unwrap().is_empty());
        assert_eq!(gateway.offset, 1);
    }

    #[test]
    fn send_posts_the_chat_id_and_text() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST).path("/sendMessage").matches(|req| {
                req.body
                    .as_deref()
                    .map(|b| {
                        let body = String::from_utf8_lossy(b);
                        body.contains("\"chat_id\":\"100\"") && body.contains("\"text\":\"hi\"")
                    })
                    .unwrap_or(false)
            });
            then.status(200).json_body(serde_json::json!({"ok": true}));
        });

        let mut gateway = TelegramGateway::with_base_url(server.base_url());
        gateway.send("100", "hi").unwrap();
        mock.assert();
    }
}
