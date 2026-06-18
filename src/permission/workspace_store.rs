use super::{FilePermissionStore, PermissionError, PermissionLevel, PermissionStore};
use std::io;
use std::path::{Path, PathBuf};

/// Persists a permission level override scoped to a single workspace
/// directory, falling back to the global `FilePermissionStore` level when
/// no override has been set for that workspace.
pub struct WorkspacePermissionStore {
    override_path: PathBuf,
    global: FilePermissionStore,
}

impl WorkspacePermissionStore {
    pub fn new(workspace_path: &Path) -> Result<Self, PermissionError> {
        Ok(Self {
            override_path: workspace_path.join(".open-string").join("permission"),
            global: FilePermissionStore::new()?,
        })
    }
}

impl PermissionStore for WorkspacePermissionStore {
    fn load(&self) -> Result<PermissionLevel, PermissionError> {
        match std::fs::read_to_string(&self.override_path) {
            Ok(contents) => {
                let trimmed = contents.trim();
                PermissionLevel::parse(trimmed)
                    .ok_or_else(|| PermissionError::InvalidLevel(trimmed.to_string()))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => self.global.load(),
            Err(e) => Err(e.into()),
        }
    }

    fn set(&self, level: PermissionLevel) -> Result<(), PermissionError> {
        if let Some(parent) = self.override_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&self.override_path, level.as_str())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_workspace() -> PathBuf {
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = env::temp_dir().join(format!("open-string-workspace-test-{id}"));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn falls_back_to_global_level_when_no_override_set() {
        let workspace = temp_workspace();
        let store = WorkspacePermissionStore::new(&workspace).unwrap();
        assert_eq!(store.load().unwrap(), store.global.load().unwrap());
        std::fs::remove_dir_all(&workspace).ok();
    }

    #[test]
    fn set_creates_an_override_independent_of_global() {
        let workspace = temp_workspace();
        let store = WorkspacePermissionStore::new(&workspace).unwrap();

        store.set(PermissionLevel::LowSecurity).unwrap();
        assert_eq!(store.load().unwrap(), PermissionLevel::LowSecurity);
        assert!(store.override_path.exists());

        std::fs::remove_dir_all(&workspace).ok();
    }
}
