//! 4.4's チャット連携ゲートウェイ: a protocol-agnostic bridge between a
//! chat platform and the Mediator, plus the safety policy every adapter
//! must go through regardless of platform.
//!
//! Design policy (OpenClawのゲートウェイ実装を参照した設計方針, derived
//! from the specific failure modes this requirements document calls out
//! for OpenClaw rather than from its source, which isn't available here):
//! - **Default-closed allow-list** (`GatewayConfig::allowed_senders`):
//!   OpenClaw's reported "open設定での第三者操作" problem was a gateway
//!   that let anyone in a reachable chat issue commands. An adapter here
//!   never dispatches a message from a sender not explicitly allow-listed
//!   -- there is no "open to everyone" mode.
//! - **Permission clamp, not pass-through** (`effective_level`): a chat
//!   message is the least-trusted input surface Open String has (anyone
//!   allow-listed in a group chat, not just the operator at the
//!   keyboard), so chat-originated requests run at `max(level, max_level)`
//!   regardless of Core's own configured level, and a confirmation
//!   request from that path is always declined rather than escalated
//!   blindly (`DeclineConfirmationPrompt`) -- there is no one present to
//!   answer it.
//! - **No unattended self-reconfiguration**: a `propose_extension` turn
//!   reaching the gateway is refused outright rather than evaluated
//!   against the permission gate at all (see `handle_message`), since
//!   self-editing Core's own MCP config from a remote chat surface is the
//!   single most consequential "third-party operation" a stranger in an
//!   allow-listed group chat could attempt.
//! - **No plaintext secrets**: bot tokens go through `GatewayTokenStore`
//!   (the OS keyring, same as the Anthropic API key), never a config file
//!   (6.3's "設定ファイルの平文秘匿情報禁止").
//! - **Compressed replies** (`compress_for_chat`): keeps responses within
//!   platform message-size limits and reduces token spend on long
//!   Mediator output before it's ever sent.

pub mod discord;
pub mod line;
pub mod telegram;

use crate::agent::{self, ClaudeTaskExecutor, ConfirmationPrompt, Mediator, MediatorTurn};
use crate::llm::ClaudeClient;
use crate::permission::{PermissionError, PermissionLevel, PermissionStore};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct IncomingMessage {
    pub sender_id: String,
    pub chat_id: String,
    pub text: String,
}

#[derive(Debug, thiserror::Error)]
pub enum GatewayError {
    #[error("network error: {0}")]
    Network(String),
    #[error("protocol error: {0}")]
    Protocol(String),
}

/// Protocol-agnostic abstraction layer every chat platform adapter
/// implements (4.4's "プロトコル非依存の抽象化層"), so `run` and the
/// safety policy around it are written once and shared by all three
/// platforms.
pub trait ChatGateway {
    fn platform(&self) -> &'static str;
    /// Blocks briefly waiting for new messages; an empty result is not an
    /// error, just "nothing new yet".
    fn poll_incoming(&mut self) -> Result<Vec<IncomingMessage>, GatewayError>;
    fn send(&mut self, chat_id: &str, text: &str) -> Result<(), GatewayError>;
}

pub struct GatewayConfig {
    pub allowed_senders: Vec<String>,
    pub max_level: PermissionLevel,
    pub max_reply_chars: usize,
    /// Maps a platform `chat_id` (Discord channel, Telegram chat, LINE
    /// group/user) to the workspace its messages should run against,
    /// letting one adapter process serve several workspaces at once. A
    /// `chat_id` with no entry here falls back to the `workspace` passed
    /// to `run` (4.4's multi-workspace routing extension).
    pub routes: HashMap<String, PathBuf>,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            allowed_senders: Vec::new(),
            max_level: PermissionLevel::HighProtect,
            max_reply_chars: 1800,
            routes: HashMap::new(),
        }
    }
}

/// Resolves which workspace a message's `chat_id` should run against: its
/// routed override if one is configured, otherwise the adapter's default
/// `workspace`. Pulled out as a pure function so the routing decision is
/// unit-testable without standing up a real `WorkspaceRuntime` (which
/// needs a stored API key and a permission store).
fn resolve_target_workspace(
    routes: &HashMap<String, PathBuf>,
    chat_id: &str,
    default: Option<&Path>,
) -> Option<PathBuf> {
    routes
        .get(chat_id)
        .cloned()
        .or_else(|| default.map(Path::to_path_buf))
}

/// 6.3's "チャットゲートウェイの公開設定デフォルトを「許可リスト制」とす
/// る": an empty allow-list (the default) lets nobody through, not
/// everybody.
pub fn is_sender_allowed(config: &GatewayConfig, sender_id: &str) -> bool {
    config.allowed_senders.iter().any(|id| id == sender_id)
}

/// Never lets a chat-originated request run more permissively than
/// `config.max_level`, regardless of what Core's own configured level is.
pub fn effective_level(configured: PermissionLevel, config: &GatewayConfig) -> PermissionLevel {
    if configured.permissiveness_rank() > config.max_level.permissiveness_rank() {
        config.max_level
    } else {
        configured
    }
}

/// Truncates a reply to `max_chars`, marking that it was cut rather than
/// silently dropping content -- both a token-spend control and a guard
/// against exceeding a platform's own message-length limit.
pub fn compress_for_chat(text: &str, max_chars: usize) -> String {
    let char_count = text.chars().count();
    if char_count <= max_chars {
        return text.to_string();
    }
    let mut truncated: String = text.chars().take(max_chars).collect();
    truncated.push_str("\n... (truncated)");
    truncated
}

/// Stores a chat platform's bot token in the OS secure credential store,
/// mirroring `AnthropicApiKeyProvider` (6.3's "認証情報の暗号化保存")
/// rather than a plaintext config file (6.3's plaintext-secret ban).
pub struct GatewayTokenStore {
    platform: &'static str,
}

impl GatewayTokenStore {
    pub fn for_platform(platform: &'static str) -> Self {
        Self { platform }
    }

    fn entry(&self) -> Result<keyring::Entry, String> {
        keyring::Entry::new("open-string-gateway", self.platform).map_err(|e| e.to_string())
    }

    pub fn store(&self, token: &str) -> Result<(), String> {
        self.entry()?.set_password(token).map_err(|e| e.to_string())
    }

    pub fn load(&self) -> Result<Option<String>, String> {
        match self.entry()?.get_password() {
            Ok(token) => Ok(Some(token)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }
}

/// A confirmation request reaching this point has nowhere to go: nobody
/// is attending an unattended gateway process to answer it, so the only
/// safe answer is no (the request falls back to whatever a denial
/// produces, same as a human declining at the CLI).
struct DeclineConfirmationPrompt;

impl ConfirmationPrompt for DeclineConfirmationPrompt {
    fn confirm(&self, _summary: &str) -> bool {
        false
    }
}

/// Always reports `effective_level` regardless of what Core's real store
/// holds, and ignores `set` (a chat-originated request never changes
/// Core's persisted permission level).
struct ClampedPermissionStore(PermissionLevel);

impl PermissionStore for ClampedPermissionStore {
    fn load(&self) -> Result<PermissionLevel, PermissionError> {
        Ok(self.0)
    }

    fn set(&self, _level: PermissionLevel) -> Result<(), PermissionError> {
        Ok(())
    }
}

/// The per-workspace pieces a gateway run needs to handle a message:
/// the Claude client, the (already gateway-clamped) permission level, and
/// the executor wired up with that workspace's Extensions/SKILLS. Built
/// once per distinct target workspace and cached in `run`'s loop, since
/// `routes` can point different `chat_id`s at different workspaces within
/// the same adapter process.
struct WorkspaceRuntime {
    client: ClaudeClient,
    level: PermissionLevel,
    extensions: Vec<agent::ExtensionInfo>,
    skills: Vec<crate::skills::Skill>,
    mcp_tools: Vec<agent::McpToolSource>,
}

impl WorkspaceRuntime {
    fn build(workspace: Option<&Path>, config: &GatewayConfig) -> Result<Self, String> {
        let provider = crate::auth::AnthropicApiKeyProvider::for_workspace(workspace);
        let client = crate::claude_client_from_stored_key(&provider)?;
        let configured_level = crate::permission_store_for(workspace)?
            .load()
            .map_err(|e| format!("failed to read permission level: {e}"))?;
        let level = effective_level(configured_level, config);
        let extensions = agent::load_connected_extensions(workspace);
        let skills = crate::skills::load_skills(workspace);
        let mcp_tools = agent::connect_workspace_tools(workspace, level);
        Ok(Self {
            client,
            level,
            extensions,
            skills,
            mcp_tools,
        })
    }

    /// Builds an executor borrowing this runtime's cached client/Extension
    /// connections. Cheap to call per message: `mcp_tools`' actual
    /// connections are `Arc<Mutex<McpClient>>` handles cloned by reference
    /// count, not reconnected.
    fn executor(&self) -> ClaudeTaskExecutor<'_> {
        ClaudeTaskExecutor::new(&self.client)
            .with_extensions(self.extensions.clone())
            .with_skills(self.skills.clone())
            .with_mcp_tools(self.mcp_tools.clone())
    }
}

/// Runs `gateway` until it returns a fatal error: polls for incoming
/// messages, drops anything from a sender not on `config.allowed_senders`,
/// and otherwise runs the message through the same Mediator pipeline
/// `chat` uses (minus persistent history -- each chat-gateway message is
/// handled as its own self-contained turn) before compressing and sending
/// the reply back. `config.routes` can send different `chat_id`s to
/// different workspaces; each target workspace's `WorkspaceRuntime` is
/// built lazily on first use and cached for the rest of the run.
pub fn run<G: ChatGateway>(
    mut gateway: G,
    workspace: Option<&Path>,
    config: GatewayConfig,
) -> Result<(), String> {
    let audit_logger = crate::permission::FileAuditLogger::new()
        .map_err(|e| format!("failed to open audit log: {e}"))?;
    let confirmation = DeclineConfirmationPrompt;
    let mut runtimes: HashMap<Option<PathBuf>, WorkspaceRuntime> = HashMap::new();

    println!(
        "{} gateway running ({} allow-listed sender(s), {} route(s), max level: {})",
        gateway.platform(),
        config.allowed_senders.len(),
        config.routes.len(),
        config.max_level
    );
    loop {
        let messages = gateway.poll_incoming().map_err(|e| e.to_string())?;
        for msg in messages {
            if !is_sender_allowed(&config, &msg.sender_id) {
                eprintln!(
                    "{}: ignoring message from non-allow-listed sender {}",
                    gateway.platform(),
                    msg.sender_id
                );
                continue;
            }
            let target = resolve_target_workspace(&config.routes, &msg.chat_id, workspace);
            let runtime = match runtimes.entry(target.clone()) {
                std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                std::collections::hash_map::Entry::Vacant(e) => {
                    match WorkspaceRuntime::build(target.as_deref(), &config) {
                        Ok(runtime) => e.insert(runtime),
                        Err(err) => {
                            eprintln!(
                                "{}: failed to prepare workspace runtime for chat {}: {err}",
                                gateway.platform(),
                                msg.chat_id
                            );
                            continue;
                        }
                    }
                }
            };
            let clamped_store = ClampedPermissionStore(runtime.level);
            let mut mediator = Mediator::new(&clamped_store, &confirmation, &audit_logger);
            let executor = runtime.executor();
            let reply = handle_message(&runtime.client, &mut mediator, &executor, &msg.text);
            let reply = compress_for_chat(&reply, config.max_reply_chars);
            if let Err(e) = gateway.send(&msg.chat_id, &reply) {
                eprintln!("{}: failed to send reply: {e}", gateway.platform());
            }
        }
    }
}

fn handle_message(
    client: &ClaudeClient,
    mediator: &mut Mediator,
    executor: &ClaudeTaskExecutor,
    text: &str,
) -> String {
    match agent::plan(client, &[], text) {
        Ok(MediatorTurn::Direct(reply)) => reply,
        Ok(MediatorTurn::Delegated(tasks)) => {
            let report = mediator.dispatch_many_aggregated(&tasks, executor);
            agent::natural_language_response(client, &report)
                .unwrap_or_else(|_| agent::render_report(&report))
        }
        Ok(MediatorTurn::ProposeExtension(proposal)) => format!(
            "Connecting new Extensions isn't available from chat for safety; ask the operator \
             to run this from the CLI instead (wanted: \"{}\").",
            proposal.name
        ),
        Err(e) => format!("error: failed to interpret request: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allow_list_defaults_to_denying_everyone() {
        let config = GatewayConfig::default();
        assert!(!is_sender_allowed(&config, "anyone"));
    }

    #[test]
    fn allow_list_admits_only_listed_senders() {
        let config = GatewayConfig {
            allowed_senders: vec!["alice".to_string()],
            ..GatewayConfig::default()
        };
        assert!(is_sender_allowed(&config, "alice"));
        assert!(!is_sender_allowed(&config, "bob"));
    }

    #[test]
    fn effective_level_clamps_down_to_the_configured_max() {
        let config = GatewayConfig {
            max_level: PermissionLevel::HighProtect,
            ..GatewayConfig::default()
        };
        assert_eq!(
            effective_level(PermissionLevel::GodMode, &config),
            PermissionLevel::HighProtect
        );
    }

    #[test]
    fn effective_level_never_raises_above_the_configured_level() {
        let config = GatewayConfig {
            max_level: PermissionLevel::GodMode,
            ..GatewayConfig::default()
        };
        assert_eq!(
            effective_level(PermissionLevel::HighProtect, &config),
            PermissionLevel::HighProtect
        );
    }

    #[test]
    fn resolve_target_workspace_uses_the_route_when_one_matches() {
        let mut routes = HashMap::new();
        routes.insert("channel-a".to_string(), PathBuf::from("/workspaces/a"));
        let default = Path::new("/workspaces/default");
        assert_eq!(
            resolve_target_workspace(&routes, "channel-a", Some(default)),
            Some(PathBuf::from("/workspaces/a"))
        );
    }

    #[test]
    fn resolve_target_workspace_falls_back_to_the_default_when_unrouted() {
        let mut routes = HashMap::new();
        routes.insert("channel-a".to_string(), PathBuf::from("/workspaces/a"));
        let default = Path::new("/workspaces/default");
        assert_eq!(
            resolve_target_workspace(&routes, "channel-b", Some(default)),
            Some(PathBuf::from("/workspaces/default"))
        );
    }

    #[test]
    fn resolve_target_workspace_is_none_when_unrouted_and_no_default() {
        let routes = HashMap::new();
        assert_eq!(resolve_target_workspace(&routes, "channel-a", None), None);
    }

    #[test]
    fn compress_for_chat_passes_short_text_through_unchanged() {
        assert_eq!(compress_for_chat("hello", 100), "hello");
    }

    #[test]
    fn compress_for_chat_truncates_and_marks_long_text() {
        let long = "x".repeat(50);
        let result = compress_for_chat(&long, 10);
        assert!(result.starts_with(&"x".repeat(10)));
        assert!(result.contains("truncated"));
    }

    #[test]
    fn decline_confirmation_prompt_always_declines() {
        assert!(!DeclineConfirmationPrompt.confirm("anything"));
    }
}
