mod agent;
mod auth;
mod permission;
mod prompt;

use agent::{
    CliConfirmationPrompt, DispatchError, EchoTaskExecutor, Mediator, MediatorConfig, Task,
    TaskOutcome,
};
use auth::{AnthropicApiKeyProvider, AuthProvider, validate_api_key_format};
use clap::{Parser, Subcommand};
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
}

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
    /// disposable Sub Agent. Until a real LLM-backed executor lands, this
    /// echoes the task description back as the result, so it's a way to
    /// exercise the permission/confirmation/audit-log pipeline end to end.
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
            } => permission_store_for(workspace.as_deref())
                .and_then(|store| run_task(store.as_ref(), &audit_logger, description, read_only)),
            AgentAction::RunTasks {
                descriptions,
                read_only,
                workspace,
                max_parallel,
            } => permission_store_for(workspace.as_deref()).and_then(|store| {
                run_tasks(
                    store.as_ref(),
                    &audit_logger,
                    descriptions,
                    read_only,
                    max_parallel,
                )
            }),
        },
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

fn run_task(
    store: &dyn PermissionStore,
    audit_logger: &dyn AuditLogger,
    description: String,
    read_only: bool,
) -> Result<(), String> {
    let confirmation = CliConfirmationPrompt;
    let mediator = Mediator::new(store, &confirmation, audit_logger);
    let task = if read_only {
        Task::read_only(description)
    } else {
        Task::new(description)
    };
    let executor = EchoTaskExecutor;

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
    descriptions: Vec<String>,
    read_only: bool,
    max_parallel: Option<usize>,
) -> Result<(), String> {
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
    let executor = EchoTaskExecutor;

    for (task, result) in tasks.iter().zip(mediator.dispatch_many(&tasks, &executor)) {
        match result {
            Ok(result) => println!(
                "{}: {} ({})",
                outcome_label(result.outcome),
                result.summary,
                task.description
            ),
            Err(e) => println!("denied: {e} ({})", task.description),
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
