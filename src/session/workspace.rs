use super::global_state_dir;
use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

/// A registered workspace: a directory Open String has been told to track
/// state for (permission overrides, conversation memory, progress notes,
/// sessions), independent of every other registered workspace (4.2.3).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Workspace {
    pub path: PathBuf,
    pub name: String,
    pub created_at: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct WorkspaceFile {
    workspaces: Vec<Workspace>,
    current: Option<PathBuf>,
}

/// CRUD + "current workspace" pointer over the set of workspaces Open
/// String knows about (4.5: create/delete/switch). Backed by a single JSON
/// file in the global config directory; each workspace's own state still
/// lives under its own `.open-string/` directory, so removing a workspace
/// from this registry does not by itself delete its state.
pub trait WorkspaceRegistry {
    fn create(&self, path: &Path, name: Option<String>) -> Result<Workspace, String>;
    fn remove(&self, path: &Path) -> Result<(), String>;
    fn list(&self) -> Result<Vec<Workspace>, String>;
    fn switch(&self, path: &Path) -> Result<Workspace, String>;
    fn current(&self) -> Result<Option<Workspace>, String>;
}

pub struct FileWorkspaceRegistry {
    file_path: PathBuf,
}

impl FileWorkspaceRegistry {
    pub fn new() -> Result<Self, String> {
        let file_path = global_state_dir()?.join("workspaces.json");
        Ok(Self { file_path })
    }

    fn read(&self) -> Result<WorkspaceFile, String> {
        match std::fs::read_to_string(&self.file_path) {
            Ok(contents) => serde_json::from_str(&contents)
                .map_err(|e| format!("corrupt workspace registry: {e}")),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(WorkspaceFile::default()),
            Err(e) => Err(e.to_string()),
        }
    }

    fn write(&self, file: &WorkspaceFile) -> Result<(), String> {
        if let Some(parent) = self.file_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(file).map_err(|e| e.to_string())?;
        std::fs::write(&self.file_path, json).map_err(|e| e.to_string())
    }

    fn now() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }
}

impl WorkspaceRegistry for FileWorkspaceRegistry {
    fn create(&self, path: &Path, name: Option<String>) -> Result<Workspace, String> {
        std::fs::create_dir_all(path)
            .map_err(|e| format!("failed to create workspace dir: {e}"))?;
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());

        let mut file = self.read()?;
        if let Some(existing) = file
            .workspaces
            .iter()
            .find(|w| w.path == canonical)
            .cloned()
        {
            return Ok(existing);
        }

        let workspace = Workspace {
            path: canonical,
            name: name.unwrap_or_else(|| {
                path.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.display().to_string())
            }),
            created_at: Self::now(),
        };
        file.workspaces.push(workspace.clone());
        self.write(&file)?;
        Ok(workspace)
    }

    fn remove(&self, path: &Path) -> Result<(), String> {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let mut file = self.read()?;
        file.workspaces.retain(|w| w.path != canonical);
        if file.current.as_deref() == Some(canonical.as_path()) {
            file.current = None;
        }
        self.write(&file)
    }

    fn list(&self) -> Result<Vec<Workspace>, String> {
        Ok(self.read()?.workspaces)
    }

    fn switch(&self, path: &Path) -> Result<Workspace, String> {
        let workspace = self.create(path, None)?;
        let mut file = self.read()?;
        file.current = Some(workspace.path.clone());
        self.write(&file)?;
        Ok(workspace)
    }

    fn current(&self) -> Result<Option<Workspace>, String> {
        let file = self.read()?;
        Ok(match file.current {
            Some(current_path) => file.workspaces.into_iter().find(|w| w.path == current_path),
            None => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_registry() -> (FileWorkspaceRegistry, PathBuf) {
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = env::temp_dir().join(format!("open-string-workspace-registry-test-{id}"));
        std::fs::create_dir_all(&dir).unwrap();
        let registry = FileWorkspaceRegistry {
            file_path: dir.join("workspaces.json"),
        };
        (registry, dir)
    }

    fn temp_workspace_path(dir: &Path, name: &str) -> PathBuf {
        dir.join(name)
    }

    #[test]
    fn create_registers_a_new_workspace_and_is_idempotent() {
        let (registry, dir) = temp_registry();
        let ws_path = temp_workspace_path(&dir, "ws-a");

        let created = registry
            .create(&ws_path, Some("My Workspace".to_string()))
            .unwrap();
        assert_eq!(created.name, "My Workspace");
        assert!(ws_path.exists());

        let created_again = registry
            .create(&ws_path, Some("Different Name".to_string()))
            .unwrap();
        assert_eq!(created_again, created);
        assert_eq!(registry.list().unwrap().len(), 1);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn remove_unregisters_a_workspace_and_clears_current_if_it_was_active() {
        let (registry, dir) = temp_registry();
        let ws_path = temp_workspace_path(&dir, "ws-b");
        registry.create(&ws_path, None).unwrap();
        registry.switch(&ws_path).unwrap();
        assert!(registry.current().unwrap().is_some());

        registry.remove(&ws_path).unwrap();
        assert!(registry.list().unwrap().is_empty());
        assert!(registry.current().unwrap().is_none());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn switch_sets_the_current_workspace_pointer() {
        let (registry, dir) = temp_registry();
        let ws_a = temp_workspace_path(&dir, "ws-c");
        let ws_b = temp_workspace_path(&dir, "ws-d");
        registry.create(&ws_a, None).unwrap();
        registry.create(&ws_b, None).unwrap();

        registry.switch(&ws_b).unwrap();
        assert_eq!(
            registry.current().unwrap().unwrap().path,
            registry.create(&ws_b, None).unwrap().path
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn current_is_none_when_nothing_was_ever_switched_to() {
        let (registry, dir) = temp_registry();
        assert!(registry.current().unwrap().is_none());
        std::fs::remove_dir_all(&dir).ok();
    }
}
