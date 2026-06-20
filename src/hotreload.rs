//! 5.5: hot-reload support for Extension/agent-behavior config. Watches
//! config files for filesystem changes and exposes a small, file-backed
//! log of reload attempts that both `health::run_health_check` (5.5's "セ
//! ルフヘルスチェック層の監視対象に含める") and a future TUI/GUI dashboard
//! (4.3) can read. Reloading itself -- rebuilding whatever in-memory state
//! a config file backs -- stays the caller's responsibility (`main::chat`'s
//! `rebuild_chat_runtime`); this module only detects "something changed"
//! and records the outcome of whatever the caller did about it.

use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::time::{SystemTime, UNIX_EPOCH};

/// One attempted reload, successful or not. `source` names whichever
/// config a reload was attempted for (e.g. "mcp config"), not necessarily
/// a file path, since a single reload can re-read more than one file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReloadEvent {
    pub source: String,
    pub success: bool,
    pub message: String,
    pub timestamp: u64,
}

impl ReloadEvent {
    pub fn now(source: impl Into<String>, success: bool, message: impl Into<String>) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Self {
            source: source.into(),
            success,
            message: message.into(),
            timestamp,
        }
    }
}

/// Cap on retained events: old ones are dropped so the log can't grow
/// without bound across a long-running `chat` session.
const MAX_RETAINED_EVENTS: usize = 50;

pub trait HotReloadLog {
    fn record(&self, event: ReloadEvent) -> Result<(), String>;
    /// The most recent `n` events, oldest first.
    fn recent(&self, n: usize) -> Result<Vec<ReloadEvent>, String>;
}

/// JSON-Lines-backed `HotReloadLog`, one event per line, kept under the
/// same workspace/global state directory other file-backed stores use
/// (`session::hotreload_log_path_for`).
pub struct FileHotReloadLog {
    path: PathBuf,
}

impl FileHotReloadLog {
    pub fn at(path: PathBuf) -> Self {
        Self { path }
    }

    fn read_all(&self) -> Result<Vec<ReloadEvent>, String> {
        let raw = match std::fs::read_to_string(&self.path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.to_string()),
        };
        Ok(raw
            .lines()
            .filter(|line| !line.trim().is_empty())
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect())
    }
}

impl HotReloadLog for FileHotReloadLog {
    fn record(&self, event: ReloadEvent) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut events = self.read_all()?;
        events.push(event);
        if events.len() > MAX_RETAINED_EVENTS {
            let overflow = events.len() - MAX_RETAINED_EVENTS;
            events.drain(0..overflow);
        }
        let body = events
            .iter()
            .map(|e| serde_json::to_string(e).map_err(|e| e.to_string()))
            .collect::<Result<Vec<_>, _>>()?
            .join("\n");
        std::fs::write(&self.path, format!("{body}\n")).map_err(|e| e.to_string())
    }

    fn recent(&self, n: usize) -> Result<Vec<ReloadEvent>, String> {
        let mut events = self.read_all()?;
        if events.len() > n {
            events.drain(0..events.len() - n);
        }
        Ok(events)
    }
}

/// Non-blocking filesystem-change detector for a fixed set of targets, each
/// either a single config file or an entire directory (e.g. SKILLS' flat
/// `skills/` directory, where any file inside being added/edited/removed
/// should count -- 5.5). `poll_changed` is meant to be checked once per turn
/// of a long-running loop (e.g. `chat`'s REPL): a file target that doesn't
/// exist yet, or that gets removed and recreated by an atomic save, is still
/// observed because the *parent directory* is what's actually watched; a
/// directory target is watched directly so changes to its contents are seen
/// without needing a representative file inside it to already exist.
pub struct ConfigWatcher {
    _watcher: RecommendedWatcher,
    rx: Receiver<notify::Result<Event>>,
    targets: Vec<PathBuf>,
}

impl ConfigWatcher {
    pub fn watch(paths: &[PathBuf]) -> Result<Self, String> {
        let (tx, rx) = channel();
        let mut watcher = notify::recommended_watcher(move |res| {
            let _ = tx.send(res);
        })
        .map_err(|e| e.to_string())?;

        let mut watched_dirs = std::collections::HashSet::new();
        for path in paths {
            let dir = if path.is_dir() {
                path.clone()
            } else {
                path.parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| PathBuf::from("."))
            };
            if watched_dirs.insert(dir.clone()) && dir.exists() {
                watcher
                    .watch(&dir, RecursiveMode::NonRecursive)
                    .map_err(|e| e.to_string())?;
            }
        }

        Ok(Self {
            _watcher: watcher,
            rx,
            targets: paths.to_vec(),
        })
    }

    /// Drains every pending filesystem event and reports whether any of
    /// them touched one of the watched paths. A watcher-internal error is
    /// conservatively treated as a change too, since the safe response to
    /// "something went wrong observing the filesystem" is to re-check the
    /// config rather than silently assume nothing happened.
    pub fn poll_changed(&self) -> bool {
        let mut changed = false;
        while let Ok(result) = self.rx.try_recv() {
            match result {
                Ok(event) => {
                    if event
                        .paths
                        .iter()
                        .any(|p| self.targets.iter().any(|t| paths_match(p, t)))
                    {
                        changed = true;
                    }
                }
                Err(_) => changed = true,
            }
        }
        changed
    }
}

/// Matches a filesystem event path `a` against a watched target `b`. A
/// directory target matches anything underneath it (a SKILLS file added or
/// edited inside `skills/`); a file target requires an exact match, falling
/// back to canonicalized comparison so a relative target still matches an
/// event path the OS reports in a different (e.g. absolute) form.
fn paths_match(a: &Path, b: &Path) -> bool {
    if b.is_dir() {
        if a.starts_with(b) {
            return true;
        }
        return matches!((a.canonicalize(), b.canonicalize()), (Ok(a), Ok(b)) if a.starts_with(&b));
    }
    if a == b {
        return true;
    }
    matches!((a.canonicalize(), b.canonicalize()), (Ok(a), Ok(b)) if a == b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_dir() -> PathBuf {
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("open-string-hotreload-test-{id}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn recent_is_empty_before_anything_is_recorded() {
        let dir = temp_dir();
        let log = FileHotReloadLog::at(dir.join("hotreload.json"));
        assert!(log.recent(10).unwrap().is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn record_then_recent_round_trips_and_orders_oldest_first() {
        let dir = temp_dir();
        let log = FileHotReloadLog::at(dir.join("hotreload.json"));
        log.record(ReloadEvent::now("mcp config", true, "reloaded"))
            .unwrap();
        log.record(ReloadEvent::now("mcp config", false, "parse error"))
            .unwrap();

        let events = log.recent(10).unwrap();
        assert_eq!(events.len(), 2);
        assert!(events[0].success);
        assert!(!events[1].success);
        assert_eq!(events[1].message, "parse error");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn recent_caps_to_the_requested_count() {
        let dir = temp_dir();
        let log = FileHotReloadLog::at(dir.join("hotreload.json"));
        for i in 0..5 {
            log.record(ReloadEvent::now("mcp config", true, format!("reload {i}")))
                .unwrap();
        }
        let events = log.recent(2).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].message, "reload 4");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn old_events_are_dropped_once_the_retention_cap_is_exceeded() {
        let dir = temp_dir();
        let log = FileHotReloadLog::at(dir.join("hotreload.json"));
        for i in 0..(MAX_RETAINED_EVENTS + 5) {
            log.record(ReloadEvent::now("mcp config", true, format!("reload {i}")))
                .unwrap();
        }
        let events = log.recent(MAX_RETAINED_EVENTS + 5).unwrap();
        assert_eq!(events.len(), MAX_RETAINED_EVENTS);
        assert_eq!(events[0].message, "reload 5");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn poll_changed_detects_a_write_to_a_watched_file() {
        let dir = temp_dir();
        let target = dir.join("watched.json");
        std::fs::write(&target, "{}").unwrap();

        let watcher = ConfigWatcher::watch(std::slice::from_ref(&target)).unwrap();
        assert!(!watcher.poll_changed());

        std::fs::write(&target, "{\"changed\": true}").unwrap();

        let mut detected = false;
        for _ in 0..40 {
            if watcher.poll_changed() {
                detected = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(detected, "expected the watcher to observe the write");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn poll_changed_detects_a_new_file_inside_a_watched_directory() {
        let dir = temp_dir();
        let skills_dir = dir.join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();

        let watcher = ConfigWatcher::watch(std::slice::from_ref(&skills_dir)).unwrap();
        assert!(!watcher.poll_changed());

        std::fs::write(
            skills_dir.join("new-skill.md"),
            "---\nname: new-skill\ndescription: test\n---\nbody\n",
        )
        .unwrap();

        let mut detected = false;
        for _ in 0..40 {
            if watcher.poll_changed() {
                detected = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(
            detected,
            "expected the watcher to observe the new file inside the directory"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
