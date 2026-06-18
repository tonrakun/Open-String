mod auth;
mod permission;

use auth::{AnthropicApiKeyProvider, AuthProvider, validate_api_key_format};
use clap::{Parser, Subcommand};
use permission::{
    AuditDecision, AuditEntry, AuditLogger, FileAuditLogger, FilePermissionStore, PermissionLevel,
    PermissionStore,
};
use std::io::Write;

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
    Status,
    /// Set the active permission level
    Set {
        level: PermissionLevel,
        /// Required to enable god mode (explicit opt-in)
        #[arg(long)]
        confirm: bool,
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
            PermissionAction::Status => permission_status(&permission_store),
            PermissionAction::Set { level, confirm } => {
                permission_set(&permission_store, &audit_logger, level, confirm)
            }
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
    let confirmed = prompt_yes_no("Re-confirm god mode for this session? [y/N]: ")
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

fn prompt_yes_no(prompt: &str) -> std::io::Result<bool> {
    print!("{prompt}");
    std::io::stdout().flush()?;
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    let answer = input.trim().to_lowercase();
    Ok(answer == "y" || answer == "yes")
}
