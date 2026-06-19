use super::{Task, TaskResult, TaskScope};

/// What a Sub Agent actually runs to produce a result (`ClaudeTaskExecutor`
/// drives the Claude API; tests use canned executors). `scope` is computed
/// by the Mediator (4.7.1) and tells the executor which tools it may offer
/// the model — the executor renders that scope, it does not decide it.
pub trait TaskExecutor {
    fn execute(&self, task: &Task, scope: &TaskScope) -> TaskResult;
}
