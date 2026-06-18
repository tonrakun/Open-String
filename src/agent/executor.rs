use super::{Task, TaskResult};

/// What a Sub Agent actually runs to produce a result (`ClaudeTaskExecutor`
/// drives the Claude API; tests use canned executors).
pub trait TaskExecutor {
    fn execute(&self, task: &Task) -> TaskResult;
}
