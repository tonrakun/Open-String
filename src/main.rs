mod agent;
mod auth;
mod llm;
mod permission;
mod prompt;

use agent::{
    ClaudeTaskExecutor, CliConfirmationPrompt, CtxAgentConfig, DispatchError, FileMemoryStore,
    Mediator, MediatorConfig, MediatorTurn, SystemPromptBuilder, Task, TaskOutcome, compact,
    natural_language_response, render_report, should_compact,
};
use auth::{AnthropicApiKeyProvider, AuthProvider, validate_api_key_format};
use clap::{Parser, Subcommand};
use llm::{ClaudeClient, Message};
use permission::{
    AuditDecision, AuditEntry, AuditLogger, FileAuditLogger, FilePermissionStore, PermissionLevel,
    PermissionStore, WorkspacePermissionStore,
};
use std::io::Write;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "open-string", version, about = "Open String Core CLI")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Manage Anthropic API key authentication
    Auth {
        #[command(subcommand)]
        action: AuthAction,
    },
    /// Manage the active operation permission level
    Permission {
        #[command(subcommand)]
        action: PermissionAction,
    },
    /// Run a task through the Mediator/Sub Agent pipeline (4.7)
    Agent {
        #[command(subcommand)]
        action: AgentAction,
    },
    /// Start an interactive natural-language session with the Mediator
    /// (4.7.1): free-form requests are interpreted turn by turn, decomposed
    /// into Sub Agent tasks when execution is needed, and dispatched.
    Chat {
        /// Run the permission pre-check against this workspace's override
        /// instead of the global default
        #[arg(long)]
        workspace: Option<PathBuf>,
        /// Maximum number of Sub Agents to run at once for a single turn
        #[arg(long)]
        max_parallel: Option<usize>,
        /// Ctx Agent trigger threshold, as a percentage of the model's
        /// context window at which conversation history gets compacted (4.7.5)
        #[arg(long)]
        ctx_trigger_threshold_pct: Option<u8>,
        /// Ctx Agent compaction target, as a percentage of the model's
        /// context window the summarized history should shrink to (4.7.5)
        #[arg(long)]
        ctx_target_size_pct: Option<u8>,
    },
}

/// Claude Sonnet 4.6's context window, used to evaluate the Ctx Agent's
/// percentage-based trigger and target thresholds (4.7.5). Core has no
/// Models API call wired in to look this up at runtime (4.2.4), so it is
/// hardcoded alongside `llm::client::DEFAULT_MODEL`.
const MEDIATOR_CONTEXT_WINDOW_TOKENS: usize = 1_000_000;

#[derive(Subcommand)]
enum AuthAction {
    /// Store an Anthropic API key in the OS secure credential store
    Login {
        /// API key value. If omitted, you will be prompted (input hidden).
        #[arg(long)]
        api_key: Option<String>,
    },
    /// Show whether an API key is currently stored
    Status,
    /// Remove the stored API key
    Logout,
}

#[derive(Subcommand)]
enum PermissionAction {
    /// Show the active permission level
    Status {
        /// Show the level for this workspace directory instead of the
        /// global default (falls back to global if no override is set)
        #[arg(long)]
        workspace: Option<PathBuf>,
    },
    /// Set the active permission level
    Set {
        level: PermissionLevel,
        /// Required to enable god mode (explicit opt-in)
        #[arg(long)]
        confirm: bool,
        /// Set the level for this workspace directory instead of the
        /// global default
        #[arg(long)]
        workspace: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum AgentAction {
    /// Run a single task through the Mediator's permission pre-check and a
    /// disposable Sub Agent backed by the Claude API. Requires an API key
    /// configured via `auth login`.
    RunTask {
        description: String,
        /// Mark the task as read-only (auto-allowed under high-protect)
        #[arg(long)]
        read_only: bool,
        /// Run the permission pre-check against this workspace's override
        /// instead of the global default
        #[arg(long)]
        workspace: Option<PathBuf>,
    },
    /// Run several tasks as parallel Sub Agents (4.7.4)
    RunTasks {
        /// One or more task descriptions, each dispatched as its own task
        #[arg(required = true)]
        descriptions: Vec<String>,
        /// Mark every task as read-only (auto-allowed under high-protect)
        #[arg(long)]
        read_only: bool,
        /// Run the permission pre-check against this workspace's override
        /// instead of the global default
        #[arg(long)]
        workspace: Option<PathBuf>,
        /// Maximum number of Sub Agents to run at once
        #[arg(long)]
        max_parallel: Option<usize>,
    },
    /// Print the (id, version) of every system-prompt fragment that would
    /// be assembled for the current permission level (4.2.1: プロンプトの
    /// 圧縮済みテンプレートのバージョン管理)
    PromptVersions {
        /// Report the fragment set used for read-only tasks
        #[arg(long)]
        read_only: bool,
        /// Evaluate against this workspace's permission override instead
        /// of the global default
        #[arg(long)]
        workspace: Option<PathBuf>,
    },
}

fn main() {
    let cli = Cli::parse();
    let provider = AnthropicApiKeyProvider::new();
    let permission_store = match FilePermissionStore::new() {
        Ok(store) => store,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    };
    let audit_logger = match FileAuditLogger::new() {
        Ok(logger) => logger,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    };

    if let Err(message) = reconfirm_god_mode_if_active(&permission_store, &audit_logger) {
        eprintln!("error: {message}");
        std::process::exit(1);
    }

    let result = match cli.command {
        Command::Auth { action } => match action {
            AuthAction::Login { api_key } => login(&provider, api_key),
            AuthAction::Status => status(&provider),
            AuthAction::Logout => logout(&provider),
        },
        Command::Permission { action } => match action {
            PermissionAction::Status { workspace } => permission_store_for(workspace.as_deref())
                .and_then(|store| permission_status(store.as_ref())),
            PermissionAction::Set {
                level,
                confirm,
                workspace,
            } => permission_store_for(workspace.as_deref())
                .and_then(|store| permission_set(store.as_ref(), &audit_logger, level, confirm)),
        },
        Command::Agent { action } => match action {
            AgentAction::RunTask {
                description,
                read_only,
                workspace,
            } => permission_store_for(workspace.as_deref()).and_then(|store| {
                run_task(
                    store.as_ref(),
                    &audit_logger,
                    &provider,
                    description,
                    read_only,
                    workspace.as_deref(),
                )
            }),
            AgentAction::RunTasks {
                descriptions,
                read_only,
                workspace,
                max_parallel,
            } => permission_store_for(workspace.as_deref()).and_then(|store| {
                run_tasks(
                    store.as_ref(),
                    &audit_logger,
                    &provider,
                    descriptions,
                    read_only,
                    max_parallel,
                    workspace.as_deref(),
                )
            }),
            AgentAction::PromptVersions {
                read_only,
                workspace,
            } => permission_store_for(workspace.as_deref())
                .and_then(|store| prompt_versions(store.as_ref(), read_only)),
        },
        Command::Chat {
            workspace,
            max_parallel,
            ctx_trigger_threshold_pct,
            ctx_target_size_pct,
        } => permission_store_for(workspace.as_deref()).and_then(|store| {
            chat(
                store.as_ref(),
                &audit_logger,
                &provider,
                max_parallel,
                ctx_trigger_threshold_pct,
                ctx_target_size_pct,
                workspace.as_deref(),
            )
        }),
    };

    if let Err(message) = result {
        eprintln!("error: {message}");
        std::process::exit(1);
    }
}

fn login(provider: &dyn AuthProvider, api_key: Option<String>) -> Result<(), String> {
    let api_key = match api_key {
        Some(key) => key,
        None => prompt_hidden("Anthropic API key: ").map_err(|e| e.to_string())?,
    };
    let api_key = api_key.trim();

    if !validate_api_key_format(api_key) {
        eprintln!("warning: key does not start with the expected \"sk-ant-\" prefix");
    }

    provider
        .store(api_key)
        .map_err(|e| format!("failed to store API key: {e}"))?;
    println!("Anthropic API key stored ({}).", provider.name());
    Ok(())
}

fn status(provider: &dyn AuthProvider) -> Result<(), String> {
    let stored = provider
        .load()
        .map_err(|e| format!("failed to read API key: {e}"))?;
    match stored {
        Some(_) => println!("Anthropic API key is configured ({}).", provider.name()),
        None => println!("No Anthropic API key is configured. Run `auth login` to set one."),
    }
    Ok(())
}

fn logout(provider: &dyn AuthProvider) -> Result<(), String> {
    provider
        .clear()
        .map_err(|e| format!("failed to remove API key: {e}"))?;
    println!("Anthropic API key removed.");
    Ok(())
}

fn prompt_hidden(prompt: &str) -> std::io::Result<String> {
    print!("{prompt}");
    std::io::stdout().flush()?;
    rpassword::read_password()
}

/// Builds the permission store a `permission status`/`permission set`
/// invocation should act on: a workspace-scoped override when `--workspace`
/// is given (falling back to the global level when no override is set
/// yet), otherwise the global store.
fn permission_store_for(
    workspace: Option<&std::path::Path>,
) -> Result<Box<dyn PermissionStore>, String> {
    match workspace {
        Some(path) => WorkspacePermissionStore::new(path)
            .map(|store| Box::new(store) as Box<dyn PermissionStore>)
            .map_err(|e| format!("failed to open workspace permission store: {e}")),
        None => FilePermissionStore::new()
            .map(|store| Box::new(store) as Box<dyn PermissionStore>)
            .map_err(|e| format!("failed to open permission store: {e}")),
    }
}

fn permission_status(store: &dyn PermissionStore) -> Result<(), String> {
    let level = store
        .load()
        .map_err(|e| format!("failed to read permission level: {e}"))?;
    println!("Active permission level: {level}");
    if level == PermissionLevel::GodMode {
        println!("note: god mode requires reconfirmation on every launch.");
    }
    Ok(())
}

/// Implements 4.2.1's template-version management: prints the fragments a
/// system prompt would draw from for the current permission level, so a
/// fragment change can be diffed/tracked across releases without having to
/// re-render a full prompt by hand.
fn prompt_versions(store: &dyn PermissionStore, read_only: bool) -> Result<(), String> {
    let level = store
        .load()
        .map_err(|e| format!("failed to read permission level: {e}"))?;
    let builder = SystemPromptBuilder::new(level, read_only);
    for (id, version) in builder.template_versions() {
        println!("{id} v{version}");
    }
    Ok(())
}

fn permission_set(
    store: &dyn PermissionStore,
    audit_logger: &dyn AuditLogger,
    level: PermissionLevel,
    confirm: bool,
) -> Result<(), String> {
    let operation = format!("permission set {level}");

    if level == PermissionLevel::GodMode && !confirm {
        log_audit(audit_logger, level, &operation, AuditDecision::Denied);
        return Err("god mode must be enabled explicitly; re-run with `--confirm`".to_string());
    }

    store
        .set(level)
        .map_err(|e| format!("failed to set permission level: {e}"))?;
    log_audit(audit_logger, level, &operation, AuditDecision::Allowed);
    println!("Permission level set to {level}.");
    Ok(())
}

/// God mode is disabled by default and, once enabled, must be reconfirmed
/// on every launch rather than staying silently active from a past run.
/// The reconfirmation outcome is always recorded (6.3: god mode requires
/// forced log recording, with no setting to opt out of it).
fn reconfirm_god_mode_if_active(
    store: &dyn PermissionStore,
    audit_logger: &dyn AuditLogger,
) -> Result<(), String> {
    let level = store
        .load()
        .map_err(|e| format!("failed to read permission level: {e}"))?;
    if level != PermissionLevel::GodMode {
        return Ok(());
    }

    eprintln!(
        "warning: god mode is the active permission level (all operations allowed, no confirmation)."
    );
    let confirmed = prompt::yes_no("Re-confirm god mode for this session? [y/N]: ")
        .map_err(|e| e.to_string())?;
    let decision = if confirmed {
        AuditDecision::ConfirmedByUser
    } else {
        AuditDecision::DeclinedByUser
    };
    log_audit(
        audit_logger,
        level,
        "god mode session reconfirmation",
        decision,
    );
    if !confirmed {
        eprintln!("god mode not reconfirmed; treating this session as high-protect.");
    }
    Ok(())
}

/// Builds a Claude API client from the API key stored via `auth login`.
fn claude_client_from_stored_key(provider: &dyn AuthProvider) -> Result<ClaudeClient, String> {
    match provider.load() {
        Ok(Some(api_key)) => Ok(ClaudeClient::new(api_key)),
        Ok(None) => Err("no Anthropic API key configured; run `auth login` first".to_string()),
        Err(e) => Err(format!("failed to read stored API key: {e}")),
    }
}

fn run_task(
    store: &dyn PermissionStore,
    audit_logger: &dyn AuditLogger,
    provider: &dyn AuthProvider,
    description: String,
    read_only: bool,
    workspace: Option<&std::path::Path>,
) -> Result<(), String> {
    let client = claude_client_from_stored_key(provider)?;
    let confirmation = CliConfirmationPrompt;
    let mediator = Mediator::new(store, &confirmation, audit_logger);
    let task = if read_only {
        Task::read_only(description)
    } else {
        Task::new(description)
    };
    let executor = ClaudeTaskExecutor::new(&client)
        .with_extensions(agent::load_connected_extensions(workspace));

    match mediator.dispatch(&task, &executor) {
        Ok(result) => {
            println!("{}: {}", outcome_label(result.outcome), result.summary);
            Ok(())
        }
        Err(DispatchError::Denied) => {
            Err("task was not confirmed by the user; nothing was executed".to_string())
        }
        Err(DispatchError::PermissionLoadFailed(message)) => {
            Err(format!("failed to evaluate permission level: {message}"))
        }
    }
}

fn run_tasks(
    store: &dyn PermissionStore,
    audit_logger: &dyn AuditLogger,
    provider: &dyn AuthProvider,
    descriptions: Vec<String>,
    read_only: bool,
    max_parallel: Option<usize>,
    workspace: Option<&std::path::Path>,
) -> Result<(), String> {
    let client = claude_client_from_stored_key(provider)?;
    let confirmation = CliConfirmationPrompt;
    let mut mediator = Mediator::new(store, &confirmation, audit_logger);
    if let Some(max_parallel_sub_agents) = max_parallel {
        mediator = mediator.with_config(MediatorConfig {
            max_parallel_sub_agents,
        });
    }
    let tasks: Vec<Task> = descriptions
        .into_iter()
        .map(|description| {
            if read_only {
                Task::read_only(description)
            } else {
                Task::new(description)
            }
        })
        .collect();
    let executor = ClaudeTaskExecutor::new(&client)
        .with_extensions(agent::load_connected_extensions(workspace));

    // The Mediator aggregates results across the batch (4.7.4): agreeing
    // Sub Agents collapse into one line, disagreeing ones surface as a
    // conflict with its majority-vote resolution instead of silently
    // picking one result.
    let report = mediator.dispatch_many_aggregated(&tasks, &executor);

    // The Mediator is the sole natural-language interlocutor (4.7.1): it
    // phrases the aggregated report itself rather than handing the user
    // raw structured data. If phrasing fails (e.g. API error), fall back to
    // printing the structured report so the result isn't lost.
    match natural_language_response(&client, &report) {
        Ok(response) => println!("{response}"),
        Err(_) => print_structured_report(&report),
    }
    Ok(())
}

/// Interactive Mediator session (4.7.1): each line of input is interpreted
/// by `agent::plan`, which decides whether it needs Sub Agent dispatch or
/// can be answered directly. Tool-call mechanics never enter the kept
/// history (4.2.2's "原則含めない" requirement) -- only the user's text and
/// the Mediator's natural-language reply are retained, so this session's
/// turns stay readable to the model on every subsequent call.
///
/// After every completed turn (never mid-response, per 4.7.5's "現在進行中
/// のターンが完了した時点で") the Ctx Agent's trigger check runs against the
/// accumulated history; once it crosses the threshold, `compact` summarizes
/// and hands the Mediator a replacement history before the next prompt.
fn chat(
    store: &dyn PermissionStore,
    audit_logger: &dyn AuditLogger,
    provider: &dyn AuthProvider,
    max_parallel: Option<usize>,
    ctx_trigger_threshold_pct: Option<u8>,
    ctx_target_size_pct: Option<u8>,
    workspace: Option<&std::path::Path>,
) -> Result<(), String> {
    let client = claude_client_from_stored_key(provider)?;
    let confirmation = CliConfirmationPrompt;
    let mut mediator = Mediator::new(store, &confirmation, audit_logger);
    if let Some(max_parallel_sub_agents) = max_parallel {
        mediator = mediator.with_config(MediatorConfig {
            max_parallel_sub_agents,
        });
    }
    let executor = ClaudeTaskExecutor::new(&client)
        .with_extensions(agent::load_connected_extensions(workspace));
    let mut history: Vec<Message> = Vec::new();
    let mut ctx_config = CtxAgentConfig::default();
    if let Some(pct) = ctx_trigger_threshold_pct {
        ctx_config.trigger_threshold_pct = pct;
    }
    if let Some(pct) = ctx_target_size_pct {
        ctx_config.target_size_pct = pct;
    }
    let memory = FileMemoryStore::new()?;

    println!("Open String chat. Type a request, or \"exit\"/\"quit\" to leave.");
    loop {
        print!("> ");
        std::io::stdout().flush().map_err(|e| e.to_string())?;

        let mut line = String::new();
        let bytes_read = std::io::stdin()
            .read_line(&mut line)
            .map_err(|e| e.to_string())?;
        if bytes_read == 0 {
            break;
        }
        let input = line.trim();
        if input.is_empty() {
            continue;
        }
        if input.eq_ignore_ascii_case("exit") || input.eq_ignore_ascii_case("quit") {
            break;
        }

        match agent::plan(&client, &history, input) {
            Ok(MediatorTurn::Direct(text)) => {
                println!("{text}");
                history.push(Message::user_text(input));
                history.push(Message::assistant_text(text));
            }
            Ok(MediatorTurn::Delegated(tasks)) => {
                let report = mediator.dispatch_many_aggregated(&tasks, &executor);
                let reply = match natural_language_response(&client, &report) {
                    Ok(reply) => reply,
                    Err(_) => render_report(&report),
                };
                println!("{reply}");
                history.push(Message::user_text(input));
                history.push(Message::assistant_text(reply));
            }
            Err(e) => {
                eprintln!("error: failed to interpret request: {e}");
            }
        }

        if should_compact(&history, MEDIATOR_CONTEXT_WINDOW_TOKENS, &ctx_config) {
            match compact(
                &client,
                &history,
                &memory,
                MEDIATOR_CONTEXT_WINDOW_TOKENS,
                &ctx_config,
            ) {
                Ok(compacted) => {
                    eprintln!(
                        "note: conversation history compacted by the Ctx Agent (full history saved to memory)."
                    );
                    history = compacted;
                }
                Err(e) => {
                    eprintln!(
                        "warning: Ctx Agent compaction failed, continuing with uncompacted history: {e}"
                    );
                }
            }
        }
    }
    Ok(())
}

fn outcome_label(outcome: TaskOutcome) -> &'static str {
    match outcome {
        TaskOutcome::Success => "success",
        TaskOutcome::Failure => "failure",
    }
}

fn print_structured_report(report: &agent::AggregatedReport) {
    for item in &report.items {
        let agreement = if item.duplicate_count > 1 {
            format!(" [{} sub agents agreed]", item.duplicate_count)
        } else {
            String::new()
        };
        println!(
            "{}: {} ({}){agreement}",
            outcome_label(item.outcome),
            item.summary,
            item.description
        );
    }
    for conflict in &report.conflicts {
        println!(
            "conflict: sub agents disagreed on \"{}\"; resolved as {}",
            conflict.description,
            outcome_label(conflict.resolved_outcome)
        );
        for (outcome, summary) in &conflict.results {
            println!("  - {}: {summary}", outcome_label(*outcome));
        }
    }
    for denied in &report.denied {
        println!("denied: {} ({})", denied.reason, denied.description);
    }
}

fn log_audit(
    audit_logger: &dyn AuditLogger,
    level: PermissionLevel,
    operation: &str,
    decision: AuditDecision,
) {
    let entry = AuditEntry {
        level,
        operation,
        decision,
    };
    if let Err(e) = audit_logger.record(&entry) {
        eprintln!("warning: failed to record audit log entry: {e}");
    }
}
