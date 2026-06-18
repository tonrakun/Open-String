use super::{AuthError, AuthProvider};

const SERVICE: &str = "open-string";
const USERNAME: &str = "anthropic_api_key";
const EXPECTED_PREFIX: &str = "sk-ant-";

/// Stores the Anthropic API key in the OS-native secure credential store
/// (Windows Credential Manager / macOS Keychain / Linux Secret Service).
pub struct AnthropicApiKeyProvider;

impl AnthropicApiKeyProvider {
    pub fn new() -> Self {
        Self
    }

    fn entry(&self) -> Result<keyring::Entry, AuthError> {
        Ok(keyring::Entry::new(SERVICE, USERNAME)?)
    }
}

impl Default for AnthropicApiKeyProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl AuthProvider for AnthropicApiKeyProvider {
    fn name(&self) -> &'static str {
        "anthropic-api-key"
    }

    fn store(&self, secret: &str) -> Result<(), AuthError> {
        self.entry()?.set_password(secret)?;
        Ok(())
    }

    fn load(&self) -> Result<Option<String>, AuthError> {
        match self.entry()?.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    fn clear(&self) -> Result<(), AuthError> {
        match self.entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(err.into()),
        }
    }
}

/// Checks the Anthropic API key prefix. Returns `false` for an unexpected
/// format, but callers should only warn, not hard-block, since the key
/// format may change in the future.
pub fn validate_api_key_format(key: &str) -> bool {
    key.starts_with(EXPECTED_PREFIX) && key.len() > EXPECTED_PREFIX.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_expected_prefix() {
        assert!(validate_api_key_format("sk-ant-abcdef123456"));
    }

    #[test]
    fn rejects_missing_prefix() {
        assert!(!validate_api_key_format("abcdef123456"));
    }

    #[test]
    fn rejects_bare_prefix_with_no_key_material() {
        assert!(!validate_api_key_format("sk-ant-"));
    }

    #[test]
    fn rejects_empty_string() {
        assert!(!validate_api_key_format(""));
    }
}
