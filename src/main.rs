mod auth;

use auth::{AnthropicApiKeyProvider, AuthProvider, validate_api_key_format};
use clap::{Parser, Subcommand};
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

fn main() {
    let cli = Cli::parse();
    let provider = AnthropicApiKeyProvider::new();

    let result = match cli.command {
        Command::Auth { action } => match action {
            AuthAction::Login { api_key } => login(&provider, api_key),
            AuthAction::Status => status(&provider),
            AuthAction::Logout => logout(&provider),
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
