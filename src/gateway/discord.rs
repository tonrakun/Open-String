//! Discord adapter: the real-time Gateway (websocket) for receiving
//! events, REST for sending replies.
//!
//! Heartbeating is best-effort: it's checked and sent (if due) once per
//! `poll_incoming` call, before that call's blocking read, rather than on
//! a strictly independent timer -- `tungstenite`'s blocking `read()` has
//! no read-timeout plumbing exposed at this level, and getting that exactly
//! right without a live Gateway connection to test against isn't a risk
//! worth taking for a v1 adapter. In practice any Gateway traffic
//! (including the server's own periodic frames) keeps `poll_incoming`
//! cycling often enough to heartbeat on time; `run`'s caller treats a
//! disconnect as a fatal `GatewayError` rather than silently hanging, so a
//! truly silent connection that drifts past Discord's heartbeat deadline
//! surfaces as a clear reconnect-needed error instead of a deadlock.

use super::{ChatGateway, GatewayError, IncomingMessage};
use serde_json::{Value, json};
use std::net::TcpStream;
use std::time::{Duration, Instant};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

const GATEWAY_URL: &str = "wss://gateway.discord.gg/?v=10&encoding=json";
const API_BASE: &str = "https://discord.com/api/v10";
/// GUILD_MESSAGES (1<<9) | MESSAGE_CONTENT (1<<15): the minimum needed to
/// receive a message's actual text content in a server channel.
const INTENTS: u32 = (1 << 9) | (1 << 15);

pub struct DiscordGateway {
    token: String,
    socket: WebSocket<MaybeTlsStream<TcpStream>>,
    heartbeat_interval: Duration,
    last_heartbeat: Instant,
    http: reqwest::blocking::Client,
}

impl DiscordGateway {
    pub fn connect(token: String) -> Result<Self, GatewayError> {
        let (mut socket, _response) =
            tungstenite::connect(GATEWAY_URL).map_err(|e| GatewayError::Network(e.to_string()))?;

        let hello = read_json(&mut socket)?;
        let heartbeat_interval_ms = hello
            .get("d")
            .and_then(|d| d.get("heartbeat_interval"))
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                GatewayError::Protocol("missing heartbeat_interval in HELLO".to_string())
            })?;

        send_json(
            &mut socket,
            &json!({
                "op": 2,
                "d": {
                    "token": token,
                    "intents": INTENTS,
                    "properties": {
                        "os": std::env::consts::OS,
                        "browser": "open-string",
                        "device": "open-string",
                    },
                }
            }),
        )?;

        Ok(Self {
            token,
            socket,
            heartbeat_interval: Duration::from_millis(heartbeat_interval_ms),
            last_heartbeat: Instant::now(),
            http: reqwest::blocking::Client::new(),
        })
    }
}

fn read_json(socket: &mut WebSocket<MaybeTlsStream<TcpStream>>) -> Result<Value, GatewayError> {
    let message = socket
        .read()
        .map_err(|e| GatewayError::Network(e.to_string()))?;
    let text = message
        .into_text()
        .map_err(|e| GatewayError::Protocol(e.to_string()))?;
    serde_json::from_str(&text).map_err(|e| GatewayError::Protocol(e.to_string()))
}

fn send_json(
    socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
    value: &Value,
) -> Result<(), GatewayError> {
    socket
        .send(Message::Text(value.to_string()))
        .map_err(|e| GatewayError::Network(e.to_string()))
}

/// Parses one Gateway dispatch payload into an `IncomingMessage` when it's
/// a `MESSAGE_CREATE` from a human (bot/self messages are dropped to avoid
/// reply loops), otherwise `None`. Kept pure so the parsing logic is
/// unit-testable without a socket.
pub(crate) fn parse_message_create(payload: &Value) -> Option<IncomingMessage> {
    if payload.get("t").and_then(Value::as_str) != Some("MESSAGE_CREATE") {
        return None;
    }
    let d = payload.get("d")?;
    let author = d.get("author")?;
    if author.get("bot").and_then(Value::as_bool).unwrap_or(false) {
        return None;
    }
    let sender_id = author.get("id")?.as_str()?.to_string();
    let chat_id = d.get("channel_id")?.as_str()?.to_string();
    let text = d.get("content")?.as_str()?.to_string();
    if text.trim().is_empty() {
        return None;
    }
    Some(IncomingMessage {
        sender_id,
        chat_id,
        text,
    })
}

impl ChatGateway for DiscordGateway {
    fn platform(&self) -> &'static str {
        "discord"
    }

    fn poll_incoming(&mut self) -> Result<Vec<IncomingMessage>, GatewayError> {
        if self.last_heartbeat.elapsed() >= self.heartbeat_interval {
            send_json(&mut self.socket, &json!({"op": 1, "d": Value::Null}))?;
            self.last_heartbeat = Instant::now();
        }

        let payload = read_json(&mut self.socket)?;
        match payload.get("op").and_then(Value::as_u64) {
            // Heartbeat ACK / other control opcodes: nothing to surface.
            Some(11) => Ok(Vec::new()),
            // The server is explicitly asking for an immediate heartbeat.
            Some(1) => {
                send_json(&mut self.socket, &json!({"op": 1, "d": Value::Null}))?;
                self.last_heartbeat = Instant::now();
                Ok(Vec::new())
            }
            // Dispatch: may or may not be a message worth surfacing.
            Some(0) => Ok(parse_message_create(&payload).into_iter().collect()),
            _ => Ok(Vec::new()),
        }
    }

    fn send(&mut self, chat_id: &str, text: &str) -> Result<(), GatewayError> {
        let url = format!("{API_BASE}/channels/{chat_id}/messages");
        let response = self
            .http
            .post(url)
            .header("Authorization", format!("Bot {}", self.token))
            .json(&json!({"content": text}))
            .send()
            .map_err(|e| GatewayError::Network(e.to_string()))?;
        if !response.status().is_success() {
            return Err(GatewayError::Protocol(format!(
                "Discord message send returned {}",
                response.status()
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_message_create_extracts_sender_channel_and_text() {
        let payload = json!({
            "op": 0,
            "t": "MESSAGE_CREATE",
            "d": {
                "author": {"id": "111", "bot": false},
                "channel_id": "222",
                "content": "hello there",
            }
        });
        let message = parse_message_create(&payload).unwrap();
        assert_eq!(message.sender_id, "111");
        assert_eq!(message.chat_id, "222");
        assert_eq!(message.text, "hello there");
    }

    #[test]
    fn parse_message_create_ignores_bot_authors() {
        let payload = json!({
            "op": 0,
            "t": "MESSAGE_CREATE",
            "d": {
                "author": {"id": "111", "bot": true},
                "channel_id": "222",
                "content": "I am a bot",
            }
        });
        assert!(parse_message_create(&payload).is_none());
    }

    #[test]
    fn parse_message_create_ignores_other_event_types() {
        let payload = json!({"op": 0, "t": "TYPING_START", "d": {}});
        assert!(parse_message_create(&payload).is_none());
    }

    #[test]
    fn parse_message_create_ignores_empty_content() {
        let payload = json!({
            "op": 0,
            "t": "MESSAGE_CREATE",
            "d": {
                "author": {"id": "111", "bot": false},
                "channel_id": "222",
                "content": "   ",
            }
        });
        assert!(parse_message_create(&payload).is_none());
    }
}
