use super::{PermissionError, PermissionLevel, PermissionStore};
use std::io;
use std::path::PathBuf;

/// Persists the active permission level as a single line of plain text
/// under the OS config directory. Not a secret, so no keyring involvement
/// (unlike the credential store in `auth::api_key`).
pub struct FilePermissionStore {
    path: PathBuf,
}

impl FilePermissionStore {
    pub fn new() -> Result<Self, PermissionError> {
        let dir = dirs::config_dir()
            .ok_or(PermissionError::NoConfigDir)?
            .join("open-string");
        Ok(Self {
            path: dir.join("permission"),
        })
    }
}

impl PermissionStore for FilePermissionStore {
    fn load(&self) -> Result<PermissionLevel, PermissionError> {
        match std::fs::read_to_string(&self.path) {
            Ok(contents) => {
                let trimmed = contents.trim();
                PermissionLevel::parse(trimmed)
                    .ok_or_else(|| PermissionError::InvalidLevel(trimmed.to_string()))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(PermissionLevel::default()),
            Err(e) => Err(e.into()),
        }
    }

    fn set(&self, level: PermissionLevel) -> Result<(), PermissionError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&self.path, level.as_str())?;
        Ok(())
    }
}
