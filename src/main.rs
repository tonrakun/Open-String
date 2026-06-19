mod agent;
mod auth;
mod llm;
mod mcp;
mod permission;
mod prompt;
mod session;
mod skills;

use agent::{
    ClaudeTaskExecutor, CliConfirmationPrompt, CtxAgentConfig, DispatchError, FileMemoryStore,
    FileProgressMemoStore, Mediator, MediatorConfig, MediatorTurn, MemoryStore, ProgressMemoStore,
    SystemPromptBuilder, Task, TaskOutcome, clear_stale_tool_results, compact, is_phase_boundary,
    natural_language_response, render_report, should_compact,
};
use auth::{AnthropicApiKeyProvider, AuthProvider, validate_api_key_format};
use clap::{Parser, Subcommand};
use llm::{ClaudeClient, Message};
use permission::{
    AuditDecision, AuditEntry, AuditLogger, FileAuditLogger, FilePermissionStore, PermissionLevel,
    PermissionStore, WorkspacePermissionStore,
};
use session::{
    FileSessionRegistry, FileWorkspaceRegistry, SessionRegistry, Workspace, WorkspaceRegistry,
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
    /// Create, list, remove, and switch between workspaces (4.5)
    Workspace {
        #[command(subcommand)]
        action: WorkspaceAction,
    },
    /// List and end chat sessions recorded for a workspace (4.5)
    Session {
        #[command(subcommand)]
        action: SessionAction,
    },
    /// Manage MCP server Extensions (`.mcp.json`) and SKILLS (5.1)
    Extension {
        #[command(subcommand)]
        action: ExtensionAction,
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
        /// Resume a previous session's conversation history instead of
        /// starting fresh, restoring the latest snapshot saved for it (4.5)
        #[arg(long)]
        resume: Option<u64>,
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
enum WorkspaceAction {
    /// Register a directory as a workspace (creates it if missing)
    Create {
        path: PathBuf,
        /// Human-readable name; defaults to the directory's file name
        #[arg(long)]
        name: Option<String>,
    },
    /// List every registered workspace, marking the current one
    List,
    /// Unregister a workspace (does not delete its `.open-string/` state)
    Remove { path: PathBuf },
    /// Make a workspace the current one, used as the default `--workspace`
    /// for commands that omit the flag
    Switch { path: PathBuf },
    /// Show the current workspace, if any
    Status,
}

#[derive(Subcommand)]
enum SessionAction {
    /// List sessions recorded for a workspace (or the global scope)
    List {
        #[arg(long)]
        workspace: Option<PathBuf>,
    },
    /// Mark a session as ended
    End {
        id: u64,
        #[arg(long)]
        workspace: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum ExtensionAction {
    /// List configured MCP servers and loaded SKILLS for a workspace
    List {
        #[arg(long)]
        workspace: Option<PathBuf>,
    },
    /// Add (or replace) an MCP server entry in `.mcp.json`
    Add {
        name: String,
        command: String,
        /// Minimum permission level required before Core will connect
        #[arg(long)]
        required_permission_level: Option<PermissionLevel>,
        /// Name of this server's history-snapshot tool, if it should be
        /// used for the Mediator's state management (4.7.1)
        #[arg(long)]
        memory_save_tool: Option<String>,
        /// Name of this server's searchable-index tool, if it should be
        /// used for the Mediator's state management (4.7.1)
        #[arg(long)]
        memory_index_tool: Option<String>,
        #[arg(long)]
        workspace: Option<PathBuf>,
        /// Arguments passed to `command`, e.g. `-- -y t0k3n-mcp`
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// Remove an MCP server entry
    Remove {
        name: String,
        #[arg(long)]
        workspace: Option<PathBuf>,
    },
    /// Re-enable a disabled MCP server entry
    Enable {
        name: String,
        #[arg(long)]
        workspace: Option<PathBuf>,
    },
    /// Disable an MCP server entry without removing its configuration
    Disable {
        name: String,
        #[arg(long)]
        workspace: Option<PathBuf>,
    },
    /// Connect to a configured server and list the tools it advertises,
    /// verifying both connectivity and permission-level compatibility
    Check {
        name: String,
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
            PermissionAction::Status { workspace } => {
                let workspace = resolve_workspace(workspace);
                permission_store_for(workspace.as_deref())
                    .and_then(|store| permission_status(store.as_ref()))
            }
            PermissionAction::Set {
                level,
                confirm,
                workspace,
            } => {
                let workspace = resolve_workspace(workspace);
                permission_store_for(workspace.as_deref())
                    .and_then(|store| permission_set(store.as_ref(), &audit_logger, level, confirm))
            }
        },
        Command::Agent { action } => match action {
            AgentAction::RunTask {
                description,
                read_only,
                workspace,
            } => {
                let workspace = resolve_workspace(workspace);
                permission_store_for(workspace.as_deref()).and_then(|store| {
                    run_task(
                        store.as_ref(),
                        &audit_logger,
                        &provider,
                        description,
                        read_only,
                        workspace.as_deref(),
                    )
                })
            }
            AgentAction::RunTasks {
                descriptions,
                read_only,
                workspace,
                max_parallel,
            } => {
                let workspace = resolve_workspace(workspace);
                permission_store_for(workspace.as_deref()).and_then(|store| {
                    run_tasks(
                        store.as_ref(),
                        &audit_logger,
                        &provider,
                        descriptions,
                        read_only,
                        max_parallel,
                        workspace.as_deref(),
                    )
                })
            }
            AgentAction::PromptVersions {
                read_only,
                workspace,
            } => {
                let workspace = resolve_workspace(workspace);
                permission_store_for(workspace.as_deref())
                    .and_then(|store| prompt_versions(store.as_ref(), read_only))
            }
        },
        Command::Workspace { action } => match action {
            WorkspaceAction::Create { path, name } => workspace_create(&path, name),
            WorkspaceAction::List => workspace_list(),
            WorkspaceAction::Remove { path } => workspace_remove(&path),
            WorkspaceAction::Switch { path } => workspace_switch(&path),
            WorkspaceAction::Status => workspace_status(),
        },
        Command::Session { action } => match action {
            SessionAction::List { workspace } => {
                session_list(resolve_workspace(workspace).as_deref())
            }
            SessionAction::End { id, workspace } => {
                session_end(resolve_workspace(workspace).as_deref(), id)
            }
        },
        Command::Extension { action } => match action {
            ExtensionAction::List { workspace } => {
                extension_list(resolve_workspace(workspace).as_deref())
            }
            ExtensionAction::Add {
                name,
                command,
                required_permission_level,
                memory_save_tool,
                memory_index_tool,
                workspace,
                args,
            } => extension_add(
                resolve_workspace(workspace).as_deref(),
                name,
                command,
                args,
                required_permission_level,
                memory_save_tool,
                memory_index_tool,
            ),
            ExtensionAction::Remove { name, workspace } => {
                extension_remove(resolve_workspace(workspace).as_deref(), &name)
            }
            ExtensionAction::Enable { name, workspace } => {
                extension_set_enabled(resolve_workspace(workspace).as_deref(), &name, true)
            }
            ExtensionAction::Disable { name, workspace } => {
                extension_set_enabled(resolve_workspace(workspace).as_deref(), &name, false)
            }
            ExtensionAction::Check { name, workspace } => {
                let workspace = resolve_workspace(workspace);
                permission_store_for(workspace.as_deref())
                    .and_then(|store| extension_check(store.as_ref(), workspace.as_deref(), &name))
            }
        },
        Command::Chat {
            workspace,
            max_parallel,
            ctx_trigger_threshold_pct,
            ctx_target_size_pct,
            resume,
        } => {
            let workspace = resolve_workspace(workspace);
            permission_store_for(workspace.as_deref()).and_then(|store| {
                chat(
                    store.as_ref(),
                    &audit_logger,
                    &provider,
                    workspace.as_deref(),
                    ChatOptions {
                        max_parallel,
                        ctx_trigger_threshold_pct,
                        ctx_target_size_pct,
                        resume,
                    },
                )
            })
        }
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

/// Falls back to the registry's current workspace when `--workspace` was
/// omitted, so `workspace switch` actually changes default behavior for
/// every command that accepts the flag (4.5).
fn resolve_workspace(explicit: Option<PathBuf>) -> Option<PathBuf> {
    if explicit.is_some() {
        return explicit;
    }
    FileWorkspaceRegistry::new()
        .and_then(|registry| registry.current())
        .ok()
        .flatten()
        .map(|workspace| workspace.path)
}

fn print_workspace(workspace: &Workspace, is_current: bool) {
    let marker = if is_current { "* " } else { "  " };
    println!("{marker}{} ({})", workspace.name, workspace.path.display());
}

fn workspace_create(path: &std::path::Path, name: Option<String>) -> Result<(), String> {
    let registry = FileWorkspaceRegistry::new()?;
    let workspace = registry.create(path, name)?;
    println!(
        "Workspace \"{}\" registered at {}.",
        workspace.name,
        workspace.path.display()
    );
    Ok(())
}

fn workspace_list() -> Result<(), String> {
    let registry = FileWorkspaceRegistry::new()?;
    let workspaces = registry.list()?;
    let current = registry.current()?;
    if workspaces.is_empty() {
        println!("No workspaces registered. Run `workspace create <path>` to add one.");
        return Ok(());
    }
    for workspace in &workspaces {
        let is_current = current.as_ref().map(|c| &c.path) == Some(&workspace.path);
        print_workspace(workspace, is_current);
    }
    Ok(())
}

fn workspace_remove(path: &std::path::Path) -> Result<(), String> {
    let registry = FileWorkspaceRegistry::new()?;
    registry.remove(path)?;
    println!("Workspace at {} unregistered.", path.display());
    Ok(())
}

fn workspace_switch(path: &std::path::Path) -> Result<(), String> {
    let registry = FileWorkspaceRegistry::new()?;
    let workspace = registry.switch(path)?;
    println!(
        "Switched to workspace \"{}\" ({}).",
        workspace.name,
        workspace.path.display()
    );
    Ok(())
}

fn workspace_status() -> Result<(), String> {
    let registry = FileWorkspaceRegistry::new()?;
    match registry.current()? {
        Some(workspace) => print_workspace(&workspace, true),
        None => println!("No current workspace. Run `workspace switch <path>` to set one."),
    }
    Ok(())
}

fn session_list(workspace: Option<&std::path::Path>) -> Result<(), String> {
    let registry = FileSessionRegistry::for_workspace(workspace)?;
    let sessions = registry.list()?;
    if sessions.is_empty() {
        println!("No sessions recorded.");
        return Ok(());
    }
    for s in &sessions {
        let state = if s.is_active() { "active" } else { "ended" };
        let label = s.label.as_deref().unwrap_or("(no label)");
        println!(
            "[{}] #{} {label} started_at={} ({state})",
            state, s.id, s.started_at
        );
    }
    Ok(())
}

fn session_end(workspace: Option<&std::path::Path>, id: u64) -> Result<(), String> {
    let registry = FileSessionRegistry::for_workspace(workspace)?;
    registry.end(id)?;
    println!("Session #{id} ended.");
    Ok(())
}

fn extension_list(workspace: Option<&std::path::Path>) -> Result<(), String> {
    let config = mcp::load(workspace)?;
    if config.mcp_servers.is_empty() {
        println!("No MCP servers configured. Run `extension add <name> <command>` to add one.");
    } else {
        for (name, entry) in &config.mcp_servers {
            let state = if entry.disabled {
                "disabled"
            } else {
                "enabled"
            };
            let requirement = entry
                .required_permission_level
                .map(|level| format!(", requires {level}"))
                .unwrap_or_default();
            println!(
                "{name} [{state}]: {} {}{requirement}",
                entry.command,
                entry.args.join(" ")
            );
        }
    }

    let loaded_skills = skills::load_skills(workspace);
    if loaded_skills.is_empty() {
        println!("No SKILLS loaded.");
    } else {
        for skill in &loaded_skills {
            println!("skill {}: {}", skill.name, skill.description);
        }
    }
    Ok(())
}

fn extension_add(
    workspace: Option<&std::path::Path>,
    name: String,
    command: String,
    args: Vec<String>,
    required_permission_level: Option<PermissionLevel>,
    memory_save_tool: Option<String>,
    memory_index_tool: Option<String>,
) -> Result<(), String> {
    let mut config = mcp::load(workspace)?;
    config.mcp_servers.insert(
        name.clone(),
        mcp::McpServerConfig {
            command,
            args,
            required_permission_level,
            memory_save_tool,
            memory_index_tool,
            ..Default::default()
        },
    );
    mcp::save(workspace, &config)?;
    println!(
        "Extension \"{name}\" added to {}.",
        mcp::config_path(workspace).display()
    );
    Ok(())
}

fn extension_remove(workspace: Option<&std::path::Path>, name: &str) -> Result<(), String> {
    let mut config = mcp::load(workspace)?;
    if config.mcp_servers.remove(name).is_none() {
        return Err(format!("no extension named \"{name}\" is configured"));
    }
    mcp::save(workspace, &config)?;
    println!("Extension \"{name}\" removed.");
    Ok(())
}

fn extension_set_enabled(
    workspace: Option<&std::path::Path>,
    name: &str,
    enabled: bool,
) -> Result<(), String> {
    let mut config = mcp::load(workspace)?;
    let entry = config
        .mcp_servers
        .get_mut(name)
        .ok_or_else(|| format!("no extension named \"{name}\" is configured"))?;
    entry.disabled = !enabled;
    mcp::save(workspace, &config)?;
    println!(
        "Extension \"{name}\" {}.",
        if enabled { "enabled" } else { "disabled" }
    );
    Ok(())
}

/// Connects to a configured server and lists its tools, checking 5.1's two
/// gates first: the entry must be enabled, and Core's active permission
/// level must satisfy the entry's `requiredPermissionLevel` (5.4 will reuse
/// this same check before a Mediator-driven dynamic introduction connects).
fn extension_check(
    store: &dyn PermissionStore,
    workspace: Option<&std::path::Path>,
    name: &str,
) -> Result<(), String> {
    let config = mcp::load(workspace)?;
    let entry = config
        .mcp_servers
        .get(name)
        .ok_or_else(|| format!("no extension named \"{name}\" is configured"))?;

    if entry.disabled {
        return Err(format!(
            "extension \"{name}\" is disabled; run `extension enable {name}` first"
        ));
    }

    let level = store
        .load()
        .map_err(|e| format!("failed to read permission level: {e}"))?;
    if !entry.is_compatible_with(level) {
        let required = entry
            .required_permission_level
            .expect("is_compatible_with only fails when a requirement is set");
        return Err(format!(
            "extension \"{name}\" requires permission level {required} or higher; current level is {level}"
        ));
    }

    let mut client = mcp::McpClient::connect(&entry.command, &entry.args)
        .map_err(|e| format!("failed to connect to \"{name}\": {e}"))?;
    let tools = client
        .list_tools()
        .map_err(|e| format!("connected to \"{name}\" but failed to list its tools: {e}"))?;

    println!(
        "\"{name}\" is reachable and advertises {} tool(s):",
        tools.len()
    );
    for tool in &tools {
        println!("  {}: {}", tool.name, tool.description);
    }
    Ok(())
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
    let level = store
        .load()
        .map_err(|e| format!("failed to read permission level: {e}"))?;
    let executor = ClaudeTaskExecutor::new(&client)
        .with_extensions(agent::load_connected_extensions(workspace))
        .with_mcp_tools(agent::connect_workspace_tools(workspace, level));

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
    let level = store
        .load()
        .map_err(|e| format!("failed to read permission level: {e}"))?;
    let executor = ClaudeTaskExecutor::new(&client)
        .with_extensions(agent::load_connected_extensions(workspace))
        .with_mcp_tools(agent::connect_workspace_tools(workspace, level));

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
/// Bundles `chat`'s tunable knobs so the function itself stays under
/// clippy's argument-count limit as more 4.5/4.7.5 options are added.
struct ChatOptions {
    max_parallel: Option<usize>,
    ctx_trigger_threshold_pct: Option<u8>,
    ctx_target_size_pct: Option<u8>,
    resume: Option<u64>,
}

fn chat(
    store: &dyn PermissionStore,
    audit_logger: &dyn AuditLogger,
    provider: &dyn AuthProvider,
    workspace: Option<&std::path::Path>,
    options: ChatOptions,
) -> Result<(), String> {
    let client = claude_client_from_stored_key(provider)?;
    let confirmation = CliConfirmationPrompt;
    let mut mediator = Mediator::new(store, &confirmation, audit_logger);
    if let Some(max_parallel_sub_agents) = options.max_parallel {
        mediator = mediator.with_config(MediatorConfig {
            max_parallel_sub_agents,
        });
    }
    let permission_level = store
        .load()
        .map_err(|e| format!("failed to read permission level: {e}"))?;
    let executor = ClaudeTaskExecutor::new(&client)
        .with_extensions(agent::load_connected_extensions(workspace))
        .with_mcp_tools(agent::connect_workspace_tools(workspace, permission_level));
    let mut history: Vec<Message> = Vec::new();
    let mut ctx_config = CtxAgentConfig::default();
    if let Some(pct) = options.ctx_trigger_threshold_pct {
        ctx_config.trigger_threshold_pct = pct;
    }
    if let Some(pct) = options.ctx_target_size_pct {
        ctx_config.target_size_pct = pct;
    }
    let resume = options.resume;
    // Workspace-scoped state (4.2.3): conversation memory and progress
    // notes live under the workspace's own `.open-string/` directory when
    // one is given, so two workspaces never see each other's history.
    //
    // 4.7.1: prefer an Extension configured for state management (e.g.
    // t0k3n-mcp's memory tools) over the local `FileMemoryStore`, falling
    // back to it when no such Extension is connected or reachable.
    let memory_dir = session::memory_dir_for(workspace)?;
    // 4.5's snapshot/restore for `--resume` always goes through the local
    // store directly (predictable, no network/process dependency); the
    // Ctx Agent's pre-compaction backup (4.2.2) additionally prefers a
    // connected state-management Extension when one is configured.
    let local_memory = FileMemoryStore::at(memory_dir.clone());
    let memory: Box<dyn MemoryStore + Sync> =
        match agent::connect_for_state_management(workspace, permission_level) {
            Some(extension_memory) => extension_memory,
            None => Box::new(FileMemoryStore::at(memory_dir)),
        };
    let progress = FileProgressMemoStore::at(session::progress_path_for(workspace)?);
    let sessions = FileSessionRegistry::for_workspace(workspace)?;
    let current_session = sessions.start(None)?;

    // The snapshot label spans every run of a given session: resuming
    // session #N keeps appending to that session's own snapshot lineage
    // rather than starting a fresh one under this run's new session id
    // (4.5's snapshot/restore requirement).
    let snapshot_label = format!("session-{}", resume.unwrap_or(current_session.id));

    // 4.5: restore a prior session's conversation in full when asked,
    // instead of starting from an empty transcript.
    if let Some(resume_id) = resume {
        match local_memory.load_latest(&format!("session-{resume_id}")) {
            Ok(Some(restored)) => {
                println!(
                    "Resumed session #{resume_id} ({} messages restored).",
                    restored.len()
                );
                history = restored;
            }
            Ok(None) => {
                eprintln!(
                    "warning: no saved snapshot found for session #{resume_id}; starting fresh."
                );
            }
            Err(e) => {
                eprintln!("warning: failed to restore session #{resume_id}: {e}; starting fresh.");
            }
        }
    }

    // 4.2.2's external-memo escape hatch: completed/unresolved work from a
    // prior session was written to the progress memo even after that
    // session's history was compacted or lost. Read it back now so this
    // session doesn't have to re-derive it from scratch.
    if let Ok(notes) = progress.load()
        && !notes.trim().is_empty()
    {
        history.push(Message::assistant_text(format!(
            "(progress notes carried over from a previous session)\n{notes}"
        )));
    }

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

        let mut phase_boundary = false;
        match agent::plan(&client, &history, input) {
            Ok(MediatorTurn::Direct(text)) => {
                println!("{text}");
                history.push(Message::user_text(input));
                history.push(Message::assistant_text(text));
            }
            Ok(MediatorTurn::Delegated(tasks)) => {
                let report = mediator.dispatch_many_aggregated(&tasks, &executor);
                phase_boundary = is_phase_boundary(&report);

                // 4.2.2's external-memo escape hatch: record completed work
                // and anything left unresolved outside the conversation
                // history itself, so a later compaction's prose summary
                // isn't the only place that information lives.
                for item in &report.items {
                    if item.outcome == TaskOutcome::Success {
                        let _ = progress.record_completed(&item.description);
                    }
                }
                for conflict in &report.conflicts {
                    let _ = progress.record_unresolved(&format!(
                        "conflicting result: {}",
                        conflict.description
                    ));
                }
                for denial in &report.denied {
                    let _ = progress.record_unresolved(&format!("denied: {}", denial.description));
                }

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

        // Lightweight first-line defense (4.2.2), run on every turn rather
        // than only once `should_compact` trips: cheaper than a full
        // Ctx Agent pass, so it absorbs growth from any tool-call traffic
        // that ends up in `history` without waiting for the threshold.
        history = clear_stale_tool_results(&history, ctx_config.keep_recent_turns);

        // A clean phase boundary (a batch with no conflicts or denials) is
        // also a trigger on its own once there is a meaningful "older"
        // portion to summarize, so the Mediator doesn't have to wait for
        // the token threshold to checkpoint a natural stopping point.
        let phase_boundary_ready =
            phase_boundary && history.len() > ctx_config.keep_recent_turns * 2;

        if should_compact(&history, MEDIATOR_CONTEXT_WINDOW_TOKENS, &ctx_config)
            || phase_boundary_ready
        {
            match compact(
                &client,
                &history,
                memory.as_ref(),
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

        // Snapshot after every turn (4.5's snapshot/restore機構) so a crash
        // or unclean exit loses at most one turn's worth of history rather
        // than the whole session.
        if let Err(e) = local_memory.save_history(&snapshot_label, &history) {
            eprintln!("warning: failed to snapshot session history: {e}");
        }
    }
    if let Err(e) = sessions.end(current_session.id) {
        eprintln!("warning: failed to record session end: {e}");
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
