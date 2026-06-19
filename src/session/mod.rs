mod registry;
mod workspace;

pub use registry::{FileSessionRegistry, SessionRegistry};
pub use workspace::{FileWorkspaceRegistry, Workspace, WorkspaceRegistry};

use std::path::{Path, PathBuf};

/// Per-workspace state directory: every workspace-scoped store (permission
/// override, conversation memory, progress memo, session registry) lives
/// under `<workspace>/.open-string/`, mirroring `WorkspacePermissionStore`'s
/// existing layout so a workspace's state stays self-contained and
/// independent of every other workspace (4.2.3's isolation requirement).
fn workspace_state_dir(workspace: &Path) -> PathBuf {
    workspace.join(".open-string")
}

fn global_state_dir() -> Result<PathBuf, String> {
    dirs::config_dir()
        .ok_or_else(|| "could not determine OS config directory".to_string())
        .map(|dir| dir.join("open-string"))
}

/// Resolves the conversation-memory directory a `FileMemoryStore` should
/// use: workspace-scoped when a workspace is given, otherwise the existing
/// global default. Keeping this as a free function (rather than baking the
/// fallback into `FileMemoryStore::new`) lets the Mediator's memory and
/// progress notes stay isolated per workspace without changing the global
/// behavior any other caller already relies on.
pub fn memory_dir_for(workspace: Option<&Path>) -> Result<PathBuf, String> {
    match workspace {
        Some(path) => Ok(workspace_state_dir(path).join("memory")),
        None => global_state_dir().map(|dir| dir.join("memory")),
    }
}

/// Resolves the progress-memo file path a `FileProgressMemoStore` should
/// use, with the same workspace-scoped/global split as `memory_dir_for`.
pub fn progress_path_for(workspace: Option<&Path>) -> Result<PathBuf, String> {
    match workspace {
        Some(path) => Ok(workspace_state_dir(path).join("progress.md")),
        None => global_state_dir().map(|dir| dir.join("progress.md")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_and_progress_paths_are_scoped_under_the_workspace_dir() {
        let workspace = PathBuf::from("/tmp/some-workspace");
        assert_eq!(
            memory_dir_for(Some(&workspace)).unwrap(),
            workspace.join(".open-string").join("memory")
        );
        assert_eq!(
            progress_path_for(Some(&workspace)).unwrap(),
            workspace.join(".open-string").join("progress.md")
        );
    }

    #[test]
    fn memory_and_progress_paths_differ_between_workspaces() {
        let a = memory_dir_for(Some(Path::new("/tmp/workspace-a"))).unwrap();
        let b = memory_dir_for(Some(Path::new("/tmp/workspace-b"))).unwrap();
        assert_ne!(a, b);
    }
}
