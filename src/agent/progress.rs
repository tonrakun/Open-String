//! External progress memo (4.2.2: 外部状態への退避). Summarization is lossy,
//! so completed tasks and unresolved items are additionally written to a
//! structured memo outside the Mediator's live context. The memo survives
//! a context reset/compaction and is read back at the start of the next
//! session so that work already done doesn't need to be re-derived from a
//! (possibly imperfect) prose summary.

use std::path::PathBuf;

pub trait ProgressMemoStore {
    /// Records a task that finished successfully.
    fn record_completed(&self, description: &str) -> Result<(), String>;
    /// Records something left unresolved (a denied task, a conflicting
    /// result, anything the next session still needs to deal with).
    fn record_unresolved(&self, note: &str) -> Result<(), String>;
    /// Reads the memo back, in whatever structured form it was written.
    /// Returns an empty string when nothing has been recorded yet.
    fn load(&self) -> Result<String, String>;
}

/// Persists the memo as a Markdown checklist under the OS config
/// directory, alongside `FileMemoryStore`/`FilePermissionStore`.
pub struct FileProgressMemoStore {
    path: PathBuf,
}

impl FileProgressMemoStore {
    pub fn new() -> Result<Self, String> {
        let path = dirs::config_dir()
            .ok_or_else(|| "could not determine OS config directory".to_string())?
            .join("open-string")
            .join("progress.md");
        Ok(Self::at(path))
    }

    /// Builds a store rooted at an explicit file, bypassing the OS
    /// config-dir lookup. Used by tests so they don't write into the real
    /// user config directory.
    pub fn at(path: PathBuf) -> Self {
        Self { path }
    }

    fn append_line(&self, line: &str) -> Result<(), String> {
        use std::io::Write as _;

        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|e| e.to_string())?;
        writeln!(file, "{line}").map_err(|e| e.to_string())
    }
}

impl ProgressMemoStore for FileProgressMemoStore {
    fn record_completed(&self, description: &str) -> Result<(), String> {
        self.append_line(&format!("- [x] {description}"))
    }

    fn record_unresolved(&self, note: &str) -> Result<(), String> {
        self.append_line(&format!("- [ ] {note}"))
    }

    fn load(&self) -> Result<String, String> {
        match std::fs::read_to_string(&self.path) {
            Ok(content) => Ok(content),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
            Err(e) => Err(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store(name: &str) -> FileProgressMemoStore {
        let path = std::env::temp_dir().join(format!("open_string_progress_test_{name}.md"));
        let _ = std::fs::remove_file(&path);
        FileProgressMemoStore::at(path)
    }

    #[test]
    fn load_returns_empty_string_when_nothing_was_recorded() {
        let store = temp_store("empty");
        assert_eq!(store.load().unwrap(), "");
    }

    #[test]
    fn completed_and_unresolved_entries_round_trip_through_load() {
        let store = temp_store("round_trip");

        store.record_completed("implement the widget").unwrap();
        store
            .record_unresolved("denied: delete the database")
            .unwrap();

        let memo = store.load().unwrap();
        assert!(memo.contains("- [x] implement the widget"));
        assert!(memo.contains("- [ ] denied: delete the database"));

        let _ = std::fs::remove_file(&store.path);
    }
}
