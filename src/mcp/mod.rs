mod bundled;
mod client;
mod config;
mod lifecycle;
mod protocol;

pub use bundled::{T0K3N_EXTENSION_NAME, default_server_config, is_available};
pub use client::McpClient;
pub use config::{McpServerConfig, config_path, load, save};
pub use lifecycle::{
    LifecycleCheckResult, LifecycleOutcome, check_for_updates, setup_workspace_extensions,
};
