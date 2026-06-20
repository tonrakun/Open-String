//! LINE Messaging API adapter: receives a webhook over a small local HTTP
//! server (signature-verified against the channel secret) and replies via
//! the push API (addressable by a persistent user/group id, unlike the
//! reply API's single-use, time-limited `replyToken`, which doesn't fit
//! this module's `send(chat_id, text)` shape).

use super::{ChatGateway, GatewayError, IncomingMessage};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use hmac::{Hmac, Mac};
use serde::Deserialize;
use sha2::Sha256;
use std::time::Duration;

const PUSH_URL: &str = "https://api.line.me/v2/bot/message/push";

pub struct LineGateway {
    channel_access_token: String,
    channel_secret: String,
    server: tiny_http::Server,
    http: reqwest::blocking::Client,
}

impl LineGateway {
    pub fn bind(
        bind_addr: &str,
        channel_access_token: String,
        channel_secret: String,
    ) -> Result<Self, GatewayError> {
        let server =
            tiny_http::Server::http(bind_addr).map_err(|e| GatewayError::Network(e.to_string()))?;
        Ok(Self {
            channel_access_token,
            channel_secret,
            server,
            http: reqwest::blocking::Client::new(),
        })
    }
}

#[derive(Deserialize)]
struct WebhookBody {
    #[serde(default)]
    events: Vec<WebhookEvent>,
}

#[derive(Deserialize)]
struct WebhookEvent {
    #[serde(rename = "type")]
    kind: String,
    source: WebhookSource,
    message: Option<WebhookMessage>,
}

#[derive(Deserialize)]
struct WebhookSource {
    #[serde(rename = "userId")]
    user_id: Option<String>,
    #[serde(rename = "groupId")]
    group_id: Option<String>,
}

#[derive(Deserialize)]
struct WebhookMessage {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
}

/// Verifies `X-Line-Signature`: base64(HMAC-SHA256(channel_secret, body)),
/// constant-time compared via `hmac::Mac::verify_slice`. A webhook with no
/// or invalid signature is never parsed as a real event -- this is the
/// only thing standing between this endpoint and an arbitrary internet
/// caller spoofing messages once it's exposed (4.4/6.3's threat model for
/// any chat gateway that, unlike Discord/Telegram, must accept inbound
/// connections from the platform rather than only making outbound ones).
fn verify_signature(channel_secret: &str, body: &[u8], signature_header: &str) -> bool {
    let Ok(signature) = BASE64.decode(signature_header) else {
        return false;
    };
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(channel_secret.as_bytes()) else {
        return false;
    };
    mac.update(body);
    mac.verify_slice(&signature).is_ok()
}

fn parse_webhook(body: &[u8]) -> Vec<IncomingMessage> {
    let Ok(parsed) = serde_json::from_slice::<WebhookBody>(body) else {
        return Vec::new();
    };
    parsed
        .events
        .into_iter()
        .filter(|event| event.kind == "message")
        .filter_map(|event| {
            let message = event.message?;
            if message.kind != "text" {
                return None;
            }
            let text = message.text?;
            let chat_id = event.source.group_id.or(event.source.user_id.clone())?;
            let sender_id = event.source.user_id.unwrap_or_else(|| chat_id.clone());
            Some(IncomingMessage {
                sender_id,
                chat_id,
                text,
            })
        })
        .collect()
}

impl ChatGateway for LineGateway {
    fn platform(&self) -> &'static str {
        "line"
    }

    fn poll_incoming(&mut self) -> Result<Vec<IncomingMessage>, GatewayError> {
        let Some(mut request) = self
            .server
            .recv_timeout(Duration::from_millis(500))
            .map_err(|e| GatewayError::Network(e.to_string()))?
        else {
            return Ok(Vec::new());
        };

        let mut body = Vec::new();
        std::io::Read::read_to_end(request.as_reader(), &mut body)
            .map_err(|e| GatewayError::Network(e.to_string()))?;

        let signature = request
            .headers()
            .iter()
            .find(|h| {
                h.field
                    .as_str()
                    .as_str()
                    .eq_ignore_ascii_case("X-Line-Signature")
            })
            .map(|h| h.value.as_str().to_string());

        let valid =
            signature.is_some_and(|sig| verify_signature(&self.channel_secret, &body, &sig));
        let response =
            tiny_http::Response::from_string("ok").with_status_code(if valid { 200 } else { 401 });
        let _ = request.respond(response);

        if !valid {
            return Ok(Vec::new());
        }
        Ok(parse_webhook(&body))
    }

    fn send(&mut self, chat_id: &str, text: &str) -> Result<(), GatewayError> {
        let response = self
            .http
            .post(PUSH_URL)
            .bearer_auth(&self.channel_access_token)
            .json(&serde_json::json!({
                "to": chat_id,
                "messages": [{"type": "text", "text": text}],
            }))
            .send()
            .map_err(|e| GatewayError::Network(e.to_string()))?;
        if !response.status().is_success() {
            return Err(GatewayError::Protocol(format!(
                "LINE push API returned {}",
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
    fn verify_signature_accepts_a_correctly_signed_body() {
        let secret = "test-secret";
        let body = br#"{"events":[]}"#;
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        let signature = BASE64.encode(mac.finalize().into_bytes());

        assert!(verify_signature(secret, body, &signature));
    }

    #[test]
    fn verify_signature_rejects_a_tampered_body() {
        let secret = "test-secret";
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(b"original body");
        let signature = BASE64.encode(mac.finalize().into_bytes());

        assert!(!verify_signature(secret, b"tampered body", &signature));
    }

    #[test]
    fn verify_signature_rejects_a_malformed_header() {
        assert!(!verify_signature("secret", b"body", "not-base64!!"));
    }

    #[test]
    fn parse_webhook_extracts_text_messages_from_a_user() {
        let body = br#"{
            "events": [
                {"type": "message", "source": {"userId": "U123"}, "message": {"type": "text", "text": "hi"}}
            ]
        }"#;
        let messages = parse_webhook(body);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].sender_id, "U123");
        assert_eq!(messages[0].chat_id, "U123");
        assert_eq!(messages[0].text, "hi");
    }

    #[test]
    fn parse_webhook_uses_group_id_as_chat_id_for_group_messages() {
        let body = br#"{
            "events": [
                {"type": "message", "source": {"userId": "U123", "groupId": "G456"}, "message": {"type": "text", "text": "hi"}}
            ]
        }"#;
        let messages = parse_webhook(body);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].sender_id, "U123");
        assert_eq!(messages[0].chat_id, "G456");
    }

    #[test]
    fn parse_webhook_ignores_non_text_and_non_message_events() {
        let body = br#"{
            "events": [
                {"type": "follow", "source": {"userId": "U1"}},
                {"type": "message", "source": {"userId": "U2"}, "message": {"type": "sticker"}}
            ]
        }"#;
        assert!(parse_webhook(body).is_empty());
    }
}
