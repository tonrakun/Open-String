mod api_key;

pub use api_key::{AnthropicApiKeyProvider, validate_api_key_format};

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("credential store error: {0}")]
    Store(#[from] keyring::Error),
}

/// Abstraction over a credential storage backend, so that providers other
/// than a static Anthropic API key (e.g. a future OAuth flow) can be added
/// without changing the CLI layer.
pub trait AuthProvider {
    fn name(&self) -> &'static str;
    fn store(&self, secret: &str) -> Result<(), AuthError>;
    fn load(&self) -> Result<Option<String>, AuthError>;
    fn clear(&self) -> Result<(), AuthError>;
}
