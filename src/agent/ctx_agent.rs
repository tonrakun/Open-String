use crate::llm::{ClaudeClient, ClaudeError, ContentBlock, Message};
use std::path::PathBuf;

/// Tunable knobs for the Ctx Agent's compaction policy (4.7.5): when to
/// trigger relative to the model's context window, and how small the
/// resulting summary should be. Both are user-configurable with the specced
/// defaults (70% trigger, 10% target).
#[derive(Debug, Clone, Copy)]
pub struct CtxAgentConfig {
    pub trigger_threshold_pct: u8,
    pub target_size_pct: u8,
}

impl Default for CtxAgentConfig {
    fn default() -> Self {
        Self {
            trigger_threshold_pct: 70,
            target_size_pct: 10,
        }
    }
}

/// Where the Ctx Agent persists the full pre-compaction history before it
/// is discarded from the Mediator's live context (4.7.5's lossless
/// fallback: "要約と保存は同一トリガーで同時に走らせ"). The eventual backing
/// store is t0k3n-mcp's `memory` tool (4.2.4), which Core has no MCP client
/// for yet; this trait lets a local implementation stand in until that
/// wiring exists, without the Mediator's call site needing to change later.
pub trait MemoryStore {
    fn save_history(&self, label: &str, history: &[Message]) -> Result<(), String>;
}

/// Persists each save as a timestamped JSON file under the OS config
/// directory, alongside `FilePermissionStore`/`FileAuditLogger`.
pub struct FileMemoryStore {
    dir: PathBuf,
}

impl FileMemoryStore {
    pub fn new() -> Result<Self, String> {
        let dir = dirs::config_dir()
            .ok_or_else(|| "could not determine OS config directory".to_string())?
            .join("open-string")
            .join("memory");
        Ok(Self { dir })
    }
}

impl MemoryStore for FileMemoryStore {
    fn save_history(&self, label: &str, history: &[Message]) -> Result<(), String> {
        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let path = self.dir.join(format!("{label}-{timestamp}.json"));
        let json = serde_json::to_string_pretty(history).map_err(|e| e.to_string())?;
        std::fs::write(&path, json).map_err(|e| e.to_string())
    }
}

/// Rough token estimate using the standard ~4-characters-per-token rule of
/// thumb. Core has no tokenizer or `count_tokens` call wired in (4.2.4
/// leaves precise token accounting to t0k3n-mcp / a future API call); this
/// is only precise enough to decide whether the trigger threshold has been
/// crossed, not to bill against.
fn message_char_len(message: &Message) -> usize {
    message
        .content
        .iter()
        .map(|block| match block {
            ContentBlock::Text { text } => text.len(),
            ContentBlock::ToolUse { input, .. } => input.to_string().len(),
            ContentBlock::ToolResult { content, .. } => content.len(),
        })
        .sum()
}

fn estimate_history_tokens(history: &[Message]) -> usize {
    history.iter().map(message_char_len).sum::<usize>() / 4
}

/// 4.7.5's trigger condition: fires once the Mediator's estimated context
/// usage reaches `config.trigger_threshold_pct` of the model's context
/// window. The caller is responsible for checking this only between turns
/// (after the current response has been fully handled), not mid-response.
pub fn should_compact(
    history: &[Message],
    context_window_tokens: usize,
    config: &CtxAgentConfig,
) -> bool {
    if context_window_tokens == 0 {
        return false;
    }
    let used = estimate_history_tokens(history);
    used.saturating_mul(100) >= context_window_tokens * config.trigger_threshold_pct as usize
}

const CTX_AGENT_SYSTEM_PROMPT: &str = "You are the Ctx Agent: a one-shot, disposable agent invoked solely to \
compress the Mediator's conversation history so the Mediator can keep running without exceeding its context \
window (4.7.5). Summarize the conversation below into a dense, faithful account of what the user asked for, \
what was delegated to Sub Agents, what succeeded or failed, and any open threads -- omit pleasantries and \
restate only what the Mediator needs to keep acting correctly. Do not invent information that is not in the \
transcript.";

const CTX_AGENT_GUIDANCE_SUFFIX: &str = "\n\n(If you need history older than this summary, or the user asks \
about something not covered here, use t0k3n's memory tool to retrieve the full pre-compaction conversation.)";

fn render_transcript(history: &[Message]) -> String {
    history
        .iter()
        .map(|message| {
            let text = message
                .content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text } => Some(text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            format!("{}: {text}", message.role)
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn request_summary(
    client: &ClaudeClient,
    system_prompt: &str,
    transcript: &str,
) -> Result<String, ClaudeError> {
    let messages = vec![Message::user_text(transcript)];
    let response = client.send(system_prompt, &messages, &[])?;
    Ok(response
        .blocks
        .into_iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

/// Runs the Ctx Agent's compaction (4.7.5): summarizes `history` down to
/// roughly `target_size_pct` of the context window while, concurrently,
/// persisting the full pre-compaction history to `memory` so nothing is
/// lost (the save and the summarization share one trigger and run side by
/// side, neither waiting on the other). The summary's tail always carries
/// the fixed guidance pointing back at that saved memory.
///
/// On summarization failure, returns the error without having touched the
/// caller's history -- the failsafe required by 4.7.5: the Mediator keeps
/// its pre-compaction state and can retry on a later turn instead of being
/// force-terminated. A failed memory save is logged but does not fail the
/// whole operation, since the summarization branch may still have
/// succeeded independently.
pub fn compact(
    client: &ClaudeClient,
    history: &[Message],
    memory: &(dyn MemoryStore + Sync),
    context_window_tokens: usize,
    config: &CtxAgentConfig,
) -> Result<Vec<Message>, ClaudeError> {
    let target_tokens = context_window_tokens * config.target_size_pct as usize / 100;
    let system_prompt = format!(
        "{CTX_AGENT_SYSTEM_PROMPT}\n\nTarget length: roughly {target_tokens} tokens or fewer."
    );
    let transcript = render_transcript(history);

    let mut summary_result: Option<Result<String, ClaudeError>> = None;
    let mut save_result: Option<Result<(), String>> = None;
    std::thread::scope(|scope| {
        let save_handle = scope.spawn(|| memory.save_history("mediator-pre-compaction", history));
        summary_result = Some(request_summary(client, &system_prompt, &transcript));
        save_result = Some(
            save_handle
                .join()
                .unwrap_or_else(|_| Err("memory save thread panicked".to_string())),
        );
    });

    if let Err(e) = save_result.expect("save thread always runs within the scope") {
        eprintln!("warning: Ctx Agent could not persist pre-compaction history to memory: {e}");
    }

    let summary = summary_result.expect("summary request always runs within the scope")?;
    Ok(vec![Message::assistant_text(format!(
        "{summary}{CTX_AGENT_GUIDANCE_SUFFIX}"
    ))])
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::Method::POST;
    use httpmock::MockServer;
    use std::sync::Mutex;

    fn text_message(role: &str, text: &str) -> Message {
        if role == "user" {
            Message::user_text(text)
        } else {
            Message::assistant_text(text)
        }
    }

    #[test]
    fn should_compact_is_false_below_threshold() {
        let history = vec![text_message("user", "hi")];
        let config = CtxAgentConfig::default();
        assert!(!should_compact(&history, 1_000_000, &config));
    }

    #[test]
    fn should_compact_is_true_at_or_above_threshold() {
        let long_text = "a".repeat(4_000);
        let history = vec![text_message("user", &long_text)];
        let config = CtxAgentConfig {
            trigger_threshold_pct: 70,
            ..CtxAgentConfig::default()
        };
        // ~1000 estimated tokens against a 1000-token window is 100% >= 70%.
        assert!(should_compact(&history, 1_000, &config));
    }

    struct RecordingMemoryStore {
        saved: Mutex<Vec<(String, usize)>>,
    }

    impl RecordingMemoryStore {
        fn new() -> Self {
            Self {
                saved: Mutex::new(Vec::new()),
            }
        }
    }

    impl MemoryStore for RecordingMemoryStore {
        fn save_history(&self, label: &str, history: &[Message]) -> Result<(), String> {
            self.saved
                .lock()
                .unwrap()
                .push((label.to_string(), history.len()));
            Ok(())
        }
    }

    #[test]
    fn compact_summarizes_and_persists_full_history_to_memory() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/v1/messages");
            then.status(200).json_body(serde_json::json!({
                "content": [{"type": "text", "text": "user asked X; Sub Agent did Y; succeeded."}],
                "stop_reason": "end_turn"
            }));
        });

        let client = ClaudeClient::new("sk-ant-test").with_base_url(server.base_url());
        let history = vec![
            text_message("user", "please do X"),
            text_message("assistant", "done"),
        ];
        let memory = RecordingMemoryStore::new();

        let result = compact(&client, &history, &memory, 1_000_000, &CtxAgentConfig::default())
            .expect("compaction should succeed");

        assert_eq!(result.len(), 1);
        match &result[0].content[0] {
            ContentBlock::Text { text } => {
                assert!(text.starts_with("user asked X; Sub Agent did Y; succeeded."));
                assert!(text.contains("t0k3n's memory tool"));
            }
            other => panic!("expected text block, got {other:?}"),
        }

        let saved = memory.saved.lock().unwrap();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0], ("mediator-pre-compaction".to_string(), 2));
    }

    #[test]
    fn compact_still_saves_to_memory_when_summarization_fails() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/v1/messages");
            then.status(500).json_body(serde_json::json!({
                "type": "error",
                "error": {"type": "api_error", "message": "internal error"}
            }));
        });

        let client = ClaudeClient::new("sk-ant-test").with_base_url(server.base_url());
        let history = vec![text_message("user", "please do X")];
        let memory = RecordingMemoryStore::new();

        let result = compact(&client, &history, &memory, 1_000_000, &CtxAgentConfig::default());

        assert!(result.is_err());
        let saved = memory.saved.lock().unwrap();
        assert_eq!(saved.len(), 1, "memory save runs even if summarization fails");
    }
}
