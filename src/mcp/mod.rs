mod client;
mod config;
mod lifecycle;
mod protocol;

pub use client::McpClient;
pub use config::{McpServerConfig, config_path, load, save};
pub use lifecycle::{
    LifecycleCheckResult, LifecycleOutcome, check_for_updates, setup_workspace_extensions,
};
