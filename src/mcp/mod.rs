mod bundled;
mod catalog;
mod client;
mod config;
mod lifecycle;
mod protocol;

pub use bundled::{
    T0K3N_EXTENSION_NAME, default_server_config, is_available, is_trusted_extension_name,
};
pub use catalog::{LOCAL_CATALOG_FILE, builtin_catalog, find as find_catalog_entry, local_catalog};
pub use client::McpClient;
pub use config::{McpServerConfig, config_path, load, save};
pub use lifecycle::{
    LifecycleCheckResult, LifecycleOutcome, check_for_updates, setup_workspace_extensions,
};
