use super::aggregate::AggregatedReport;
use crate::llm::{ClaudeClient, ClaudeError, ContentBlock, Message};
use std::path::PathBuf;

/// Tunable knobs for the Ctx Agent's compaction policy (4.7.5/4.2.2): when
/// to trigger relative to the model's context window, how small the
/// resulting summary should be, and how many of the most recent turns to
/// keep verbatim rather than summarize. All are user-configurable with the
/// specced defaults (70% trigger, 10% target) so the threshold/granularity
/// trade-off can be tuned by benchmark rather than hardcoded.
#[derive(Debug, Clone, Copy)]
pub struct CtxAgentConfig {
    pub trigger_threshold_pct: u8,
    pub target_size_pct: u8,
    /// Number of the most recent messages excluded from summarization, so
    /// compaction never erases the immediate back-and-forth the Mediator
    /// needs for its very next decision (4.2.2: 過剰な要約による弊害を防ぐ).
    pub keep_recent_turns: usize,
}

impl Default for CtxAgentConfig {
    fn default() -> Self {
        Self {
            trigger_threshold_pct: 70,
            target_size_pct: 10,
            keep_recent_turns: 4,
        }
    }
}

/// Lightweight first-line defense against context growth (4.2.2), applied
/// on every turn rather than only once `should_compact` trips: any
/// `ToolResult` block older than the most recent `keep_recent` messages is
/// replaced with a short marker recording that the call happened, without
/// retaining its (potentially large) raw body. `ToolUse` and `Text` blocks
/// are left untouched, since only the result payload is the size risk.
/// Far cheaper than `compact`, since it never calls the model.
pub fn clear_stale_tool_results(history: &[Message], keep_recent: usize) -> Vec<Message> {
    let cutoff = history.len().saturating_sub(keep_recent);
    history
        .iter()
        .enumerate()
        .map(|(i, message)| {
            if i >= cutoff {
                return message.clone();
            }
            let content = message
                .content
                .iter()
                .map(|block| match block {
                    ContentBlock::ToolResult {
                        tool_use_id,
                        is_error,
                        ..
                    } => ContentBlock::ToolResult {
                        tool_use_id: tool_use_id.clone(),
                        content: "[tool result cleared; call is still on record]".to_string(),
                        is_error: *is_error,
                    },
                    other => other.clone(),
                })
                .collect();
            Message {
                role: message.role.clone(),
                content,
            }
        })
        .collect()
}

/// Detects a natural checkpoint (4.2.2: フェーズ境界の自動検知): a batch of
/// delegated tasks that all came back clean, with no conflicting Sub Agent
/// results and nothing denied. Such a boundary is a good moment to compact
/// even before the token-threshold trigger fires, since the Mediator's
/// in-flight state is simple to summarize faithfully right after a clean
/// batch completes.
pub fn is_phase_boundary(report: &AggregatedReport) -> bool {
    !report.items.is_empty() && report.conflicts.is_empty() && report.denied.is_empty()
}

/// Where the Ctx Agent persists the full pre-compaction history before it
/// is discarded from the Mediator's live context (4.7.5's lossless
/// fallback: "要約と保存は同一トリガーで同時に走らせ"). The eventual backing
/// store is t0k3n-mcp's `memory` tool (4.2.4), which Core has no MCP client
/// for yet; this trait lets a local implementation stand in until that
/// wiring exists, without the Mediator's call site needing to change later.
pub trait MemoryStore {
    fn save_history(&self, label: &str, history: &[Message]) -> Result<(), String>;

    /// Records a short, greppable index entry for a saved snapshot (4.2.2:
    /// 要約後も検索可能な索引の保持), so the label and summary text can be
    /// searched without re-reading the (potentially large) JSON snapshot
    /// `save_history` wrote. Defaults to a no-op so existing test doubles
    /// don't need to implement it.
    fn record_index_entry(&self, _label: &str, _summary: &str) -> Result<(), String> {
        Ok(())
    }
}

/// Persists each save as a timestamped JSON file under the OS config
/// directory, alongside `FilePermissionStore`/`FileAuditLogger`.
pub struct FileMemoryStore {
    dir: PathBuf,
}

impl FileMemoryStore {
    /// Builds a store rooted at an explicit directory. Callers resolve the
    /// directory via `session::memory_dir_for` (global or workspace-scoped,
    /// 4.2.3) rather than this type looking up the OS config dir itself.
    pub fn at(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// Reads back the most recently saved history for `label`, if any --
    /// the restore half of 4.5's "snapshot/restore機構". `save_history`
    /// writes one timestamped `{label}-{timestamp}.json` file per call, so
    /// this picks the lexicographically greatest (i.e. newest) one.
    pub fn load_latest(&self, label: &str) -> Result<Option<Vec<Message>>, String> {
        let prefix = format!("{label}-");
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.to_string()),
        };

        let mut newest: Option<(String, PathBuf)> = None;
        for entry in entries {
            let entry = entry.map_err(|e| e.to_string())?;
            let file_name = entry.file_name().to_string_lossy().to_string();
            if !file_name.starts_with(&prefix) || !file_name.ends_with(".json") {
                continue;
            }
            if newest.as_ref().is_none_or(|(name, _)| file_name > *name) {
                newest = Some((file_name, entry.path()));
            }
        }

        match newest {
            Some((_, path)) => {
                let contents = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
                serde_json::from_str(&contents)
                    .map(Some)
                    .map_err(|e| e.to_string())
            }
            None => Ok(None),
        }
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

    fn record_index_entry(&self, label: &str, summary: &str) -> Result<(), String> {
        use std::io::Write as _;

        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let entry = serde_json::json!({"timestamp": timestamp, "label": label, "summary": summary});
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join("index.jsonl"))
            .map_err(|e| e.to_string())?;
        writeln!(file, "{entry}").map_err(|e| e.to_string())
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

/// Rough token estimate for a conversation history (4.3's "トークン消費状況"
/// dashboard data needs the same estimate `should_compact` already uses, so
/// the two stay consistent rather than drifting apart).
pub fn estimate_history_tokens(history: &[Message]) -> usize {
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

/// Runs the Ctx Agent's compaction (4.7.5): summarizes the older portion of
/// `history` down to roughly `target_size_pct` of the context window while,
/// concurrently, persisting the full pre-compaction history to `memory` so
/// nothing is lost (the save and the summarization share one trigger and
/// run side by side, neither waiting on the other). The most recent
/// `config.keep_recent_turns` messages are excluded from summarization and
/// returned verbatim after the summary (4.2.2: 直近N件のやり取りは生のまま
/// 保持する), so the immediate back-and-forth the Mediator needs for its
/// next decision survives compaction untouched. The summary's tail always
/// carries the fixed guidance pointing back at that saved memory.
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
    let cutoff = history.len().saturating_sub(config.keep_recent_turns);
    let (older, recent) = history.split_at(cutoff);

    if older.is_empty() {
        // Everything in `history` falls inside the keep-recent window: there
        // is nothing left to summarize, so skip the model call entirely
        // rather than asking it to summarize an empty transcript. The
        // lossless backup still runs since it's cheap and not on the hot
        // path of this decision.
        if let Err(e) = memory.save_history("mediator-pre-compaction", history) {
            eprintln!("warning: Ctx Agent could not persist pre-compaction history to memory: {e}");
        }
        return Ok(history.to_vec());
    }

    let target_tokens = context_window_tokens * config.target_size_pct as usize / 100;
    let system_prompt = format!(
        "{CTX_AGENT_SYSTEM_PROMPT}\n\nTarget length: roughly {target_tokens} tokens or fewer."
    );
    let transcript = render_transcript(older);

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
    if let Err(e) = memory.record_index_entry("mediator-pre-compaction", &summary) {
        eprintln!("warning: Ctx Agent could not record a searchable index entry: {e}");
    }

    let mut compacted = vec![Message::assistant_text(format!(
        "{summary}{CTX_AGENT_GUIDANCE_SUFFIX}"
    ))];
    compacted.extend(recent.iter().cloned());
    Ok(compacted)
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
        let config = CtxAgentConfig {
            keep_recent_turns: 0,
            ..CtxAgentConfig::default()
        };

        let result = compact(&client, &history, &memory, 1_000_000, &config)
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
        let config = CtxAgentConfig {
            keep_recent_turns: 0,
            ..CtxAgentConfig::default()
        };

        let result = compact(&client, &history, &memory, 1_000_000, &config);

        assert!(result.is_err());
        let saved = memory.saved.lock().unwrap();
        assert_eq!(
            saved.len(),
            1,
            "memory save runs even if summarization fails"
        );
    }

    #[test]
    fn compact_keeps_the_most_recent_turns_verbatim() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/v1/messages");
            then.status(200).json_body(serde_json::json!({
                "content": [{"type": "text", "text": "summary of the older turns"}],
                "stop_reason": "end_turn"
            }));
        });

        let client = ClaudeClient::new("sk-ant-test").with_base_url(server.base_url());
        let history = vec![
            text_message("user", "first request"),
            text_message("assistant", "first reply"),
            text_message("user", "second request"),
            text_message("assistant", "second reply"),
        ];
        let memory = RecordingMemoryStore::new();
        let config = CtxAgentConfig {
            keep_recent_turns: 1,
            ..CtxAgentConfig::default()
        };

        let result =
            compact(&client, &history, &memory, 1_000_000, &config).expect("should succeed");

        assert_eq!(result.len(), 2, "summary plus the one kept-recent message");
        match &result[1].content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "second reply"),
            other => panic!("expected the verbatim recent message, got {other:?}"),
        }
    }

    #[test]
    fn record_index_entry_appends_a_searchable_line() {
        let dir = std::env::temp_dir().join("open_string_ctx_agent_index_test");
        let _ = std::fs::remove_dir_all(&dir);
        let store = FileMemoryStore::at(dir.clone());

        store
            .record_index_entry("mediator-pre-compaction", "user asked X; it succeeded.")
            .expect("recording an index entry should succeed");

        let index = std::fs::read_to_string(dir.join("index.jsonl")).unwrap();
        assert!(index.contains("mediator-pre-compaction"));
        assert!(index.contains("user asked X; it succeeded."));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_latest_returns_the_newest_snapshot_for_a_label() {
        let dir = std::env::temp_dir().join("open_string_ctx_agent_load_latest_test");
        let _ = std::fs::remove_dir_all(&dir);
        let store = FileMemoryStore::at(dir.clone());

        store
            .save_history("session-1", &[text_message("user", "first")])
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        store
            .save_history("session-1", &[text_message("user", "second")])
            .unwrap();

        let restored = store.load_latest("session-1").unwrap().unwrap();
        match &restored[0].content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "second"),
            other => panic!("expected the newest snapshot, got {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_latest_returns_none_when_no_snapshot_exists() {
        let dir = std::env::temp_dir().join("open_string_ctx_agent_load_latest_missing_test");
        let _ = std::fs::remove_dir_all(&dir);
        let store = FileMemoryStore::at(dir.clone());

        assert!(store.load_latest("session-unknown").unwrap().is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clear_stale_tool_results_replaces_old_results_but_keeps_recent_ones() {
        let old = Message::user_blocks(vec![ContentBlock::ToolResult {
            tool_use_id: "toolu_1".to_string(),
            content: "a very large raw tool output".to_string(),
            is_error: false,
        }]);
        let recent = Message::user_blocks(vec![ContentBlock::ToolResult {
            tool_use_id: "toolu_2".to_string(),
            content: "still needed for the next decision".to_string(),
            is_error: false,
        }]);
        let history = vec![old, recent];

        let cleared = clear_stale_tool_results(&history, 1);

        match &cleared[0].content[0] {
            ContentBlock::ToolResult { content, .. } => {
                assert_eq!(content, "[tool result cleared; call is still on record]");
            }
            other => panic!("expected a cleared tool result, got {other:?}"),
        }
        match &cleared[1].content[0] {
            ContentBlock::ToolResult { content, .. } => {
                assert_eq!(content, "still needed for the next decision");
            }
            other => panic!("expected the recent tool result untouched, got {other:?}"),
        }
    }

    #[test]
    fn clear_stale_tool_results_leaves_text_and_tool_use_blocks_untouched() {
        let history = vec![text_message("user", "hello")];
        let cleared = clear_stale_tool_results(&history, 0);
        match &cleared[0].content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "hello"),
            other => panic!("expected the text block untouched, got {other:?}"),
        }
    }

    fn aggregated_report(
        items: usize,
        conflicts: usize,
        denied: usize,
    ) -> crate::agent::aggregate::AggregatedReport {
        use crate::agent::TaskOutcome;
        use crate::agent::aggregate::{AggregatedItem, Conflict, DeniedTask};

        AggregatedReport {
            items: (0..items)
                .map(|i| AggregatedItem {
                    description: format!("task {i}"),
                    outcome: TaskOutcome::Success,
                    summary: "done".to_string(),
                    duplicate_count: 1,
                })
                .collect(),
            conflicts: (0..conflicts)
                .map(|i| Conflict {
                    description: format!("conflict {i}"),
                    resolved_outcome: TaskOutcome::Failure,
                    results: vec![],
                })
                .collect(),
            denied: (0..denied)
                .map(|i| DeniedTask {
                    description: format!("denied {i}"),
                    reason: "not confirmed".to_string(),
                })
                .collect(),
        }
    }

    #[test]
    fn is_phase_boundary_true_for_a_clean_successful_batch() {
        assert!(is_phase_boundary(&aggregated_report(2, 0, 0)));
    }

    #[test]
    fn is_phase_boundary_false_when_empty_or_conflicted_or_denied() {
        assert!(!is_phase_boundary(&aggregated_report(0, 0, 0)));
        assert!(!is_phase_boundary(&aggregated_report(1, 1, 0)));
        assert!(!is_phase_boundary(&aggregated_report(1, 0, 1)));
    }
}
