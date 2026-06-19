use super::{global_state_dir, workspace_state_dir};
use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

/// A single chat session record (4.5): when it started, when (if ever) it
/// ended, and an optional human-readable label. The conversation content
/// itself is not stored here -- that is the Ctx Agent's `FileMemoryStore`
/// snapshot/restore path (4.2.2); this registry only tracks session
/// lifecycle metadata for listing and dashboard display (4.2.3).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Session {
    pub id: u64,
    pub label: Option<String>,
    pub started_at: u64,
    pub ended_at: Option<u64>,
}

impl Session {
    pub fn is_active(&self) -> bool {
        self.ended_at.is_none()
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct SessionFile {
    sessions: Vec<Session>,
    next_id: u64,
}

/// Create/list/end operations over the sessions recorded for one workspace
/// (or the global scope when no workspace is given), satisfying 4.5's
/// session-lifecycle requirement and providing the listing data 4.2.3 asks
/// for a dashboard to display.
pub trait SessionRegistry {
    fn start(&self, label: Option<String>) -> Result<Session, String>;
    fn end(&self, id: u64) -> Result<(), String>;
    fn list(&self) -> Result<Vec<Session>, String>;
}

pub struct FileSessionRegistry {
    file_path: PathBuf,
}

impl FileSessionRegistry {
    /// Resolves the session file for a workspace (`<workspace>/.open-string
    /// /sessions.json`) or, when `workspace` is `None`, the global scope
    /// (`<config_dir>/open-string/sessions.json`) -- the same split used by
    /// `memory_dir_for`/`progress_path_for` so a workspace's sessions stay
    /// isolated from every other workspace's.
    pub fn for_workspace(workspace: Option<&Path>) -> Result<Self, String> {
        let file_path = match workspace {
            Some(path) => workspace_state_dir(path).join("sessions.json"),
            None => global_state_dir()?.join("sessions.json"),
        };
        Ok(Self { file_path })
    }

    fn read(&self) -> Result<SessionFile, String> {
        match std::fs::read_to_string(&self.file_path) {
            Ok(contents) => serde_json::from_str(&contents)
                .map_err(|e| format!("corrupt session registry: {e}")),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(SessionFile::default()),
            Err(e) => Err(e.to_string()),
        }
    }

    fn write(&self, file: &SessionFile) -> Result<(), String> {
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

impl SessionRegistry for FileSessionRegistry {
    fn start(&self, label: Option<String>) -> Result<Session, String> {
        let mut file = self.read()?;
        let id = file.next_id;
        file.next_id += 1;
        let session = Session {
            id,
            label,
            started_at: Self::now(),
            ended_at: None,
        };
        file.sessions.push(session.clone());
        self.write(&file)?;
        Ok(session)
    }

    fn end(&self, id: u64) -> Result<(), String> {
        let mut file = self.read()?;
        let session = file
            .sessions
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or_else(|| format!("no session with id {id}"))?;
        if session.ended_at.is_none() {
            session.ended_at = Some(Self::now());
        }
        self.write(&file)
    }

    fn list(&self) -> Result<Vec<Session>, String> {
        Ok(self.read()?.sessions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_registry() -> (FileSessionRegistry, PathBuf) {
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = env::temp_dir().join(format!("open-string-session-registry-test-{id}"));
        std::fs::create_dir_all(&dir).unwrap();
        let registry = FileSessionRegistry {
            file_path: dir.join("sessions.json"),
        };
        (registry, dir)
    }

    #[test]
    fn start_assigns_increasing_ids_and_marks_sessions_active() {
        let (registry, dir) = temp_registry();
        let first = registry.start(Some("first".to_string())).unwrap();
        let second = registry.start(None).unwrap();

        assert_eq!(first.id, 0);
        assert_eq!(second.id, 1);
        assert!(first.is_active());
        assert!(second.is_active());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn end_marks_a_session_inactive_without_touching_others() {
        let (registry, dir) = temp_registry();
        let first = registry.start(None).unwrap();
        let second = registry.start(None).unwrap();

        registry.end(first.id).unwrap();
        let sessions = registry.list().unwrap();
        assert!(!sessions[0].is_active());
        assert!(sessions[1].is_active());
        assert_eq!(sessions[1].id, second.id);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn end_is_idempotent_and_errors_on_unknown_id() {
        let (registry, dir) = temp_registry();
        let session = registry.start(None).unwrap();
        registry.end(session.id).unwrap();
        let ended_at_first = registry.list().unwrap()[0].ended_at;

        registry.end(session.id).unwrap();
        assert_eq!(registry.list().unwrap()[0].ended_at, ended_at_first);

        assert!(registry.end(999).is_err());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn sessions_for_different_workspaces_are_isolated() {
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = env::temp_dir().join(format!("open-string-session-isolation-test-{id}"));
        let ws_a = dir.join("a");
        let ws_b = dir.join("b");
        std::fs::create_dir_all(&ws_a).unwrap();
        std::fs::create_dir_all(&ws_b).unwrap();

        let registry_a = FileSessionRegistry::for_workspace(Some(&ws_a)).unwrap();
        let registry_b = FileSessionRegistry::for_workspace(Some(&ws_b)).unwrap();
        registry_a.start(Some("in a".to_string())).unwrap();

        assert_eq!(registry_a.list().unwrap().len(), 1);
        assert_eq!(registry_b.list().unwrap().len(), 0);

        std::fs::remove_dir_all(&dir).ok();
    }
}
