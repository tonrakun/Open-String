use super::{AuthError, AuthProvider};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;

const SERVICE: &str = "open-string";
const USERNAME: &str = "anthropic_api_key";
const EXPECTED_PREFIX: &str = "sk-ant-";

/// Stores the Anthropic API key in the OS-native secure credential store
/// (Windows Credential Manager / macOS Keychain / Linux Secret Service).
///
/// Per-workspace overrides (4.5's "ワークスペースごとの認証プロバイダの個別
/// 管理") are kept in the same secure store under a workspace-derived
/// keyring username rather than a plaintext file, to honor 6.3's "平文での
/// 秘匿情報保存禁止". `load`/`clear` fall back to the global entry when no
/// workspace-specific one exists, mirroring `WorkspacePermissionStore`.
pub struct AnthropicApiKeyProvider {
    workspace_username: Option<String>,
}

impl AnthropicApiKeyProvider {
    pub fn new() -> Self {
        Self {
            workspace_username: None,
        }
    }

    /// Scopes the provider to a workspace. `None` behaves like `new()`.
    pub fn for_workspace(workspace: Option<&Path>) -> Self {
        Self {
            workspace_username: workspace.map(workspace_username),
        }
    }

    fn entry(&self) -> Result<keyring::Entry, AuthError> {
        let username = self.workspace_username.as_deref().unwrap_or(USERNAME);
        Ok(keyring::Entry::new(SERVICE, username)?)
    }

    fn global_entry(&self) -> Result<keyring::Entry, AuthError> {
        Ok(keyring::Entry::new(SERVICE, USERNAME)?)
    }
}

fn workspace_username(workspace: &Path) -> String {
    let canonical = std::fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
    let mut hasher = DefaultHasher::new();
    canonical.hash(&mut hasher);
    format!("{USERNAME}::workspace::{:x}", hasher.finish())
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
            Err(keyring::Error::NoEntry) if self.workspace_username.is_some() => {
                match self.global_entry()?.get_password() {
                    Ok(secret) => Ok(Some(secret)),
                    Err(keyring::Error::NoEntry) => Ok(None),
                    Err(err) => Err(err.into()),
                }
            }
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

    #[test]
    fn for_workspace_with_none_matches_new() {
        assert_eq!(
            AnthropicApiKeyProvider::for_workspace(None).workspace_username,
            AnthropicApiKeyProvider::new().workspace_username,
        );
    }

    #[test]
    fn workspace_username_is_deterministic_and_distinct_per_path() {
        let a = workspace_username(Path::new("workspace-a"));
        let a_again = workspace_username(Path::new("workspace-a"));
        let b = workspace_username(Path::new("workspace-b"));
        assert_eq!(a, a_again);
        assert_ne!(a, b);
        assert!(a.starts_with(&format!("{USERNAME}::workspace::")));
    }
}
