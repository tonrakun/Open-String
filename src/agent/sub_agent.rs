use super::{Task, TaskExecutor, TaskResult, TaskScope};

/// A disposable, single-use execution handle. Can only be constructed by
/// `Mediator::dispatch`/`dispatch_many` (see `mediator.rs`), so every Sub
/// Agent goes through the Mediator's permission pre-check (4.1, 4.7.1) —
/// there is no other path that produces one.
pub struct SubAgent<'a> {
    executor: &'a dyn TaskExecutor,
}

impl<'a> SubAgent<'a> {
    pub(super) fn new(executor: &'a dyn TaskExecutor) -> Self {
        Self { executor }
    }

    /// Runs exactly one task and consumes the Sub Agent, matching the
    /// "1 task = 1 generation, disposable, no carried-over state" rule
    /// (4.7.2): there is no way to call `run` a second time on the same
    /// instance. `scope` is the Mediator-computed permission/tool scope
    /// for this task (4.7.1).
    pub fn run(self, task: &Task, scope: &TaskScope) -> TaskResult {
        self.executor.execute(task, scope)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permission::PermissionLevel;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingExecutor {
        calls: AtomicUsize,
    }

    impl TaskExecutor for CountingExecutor {
        fn execute(&self, task: &Task, _scope: &TaskScope) -> TaskResult {
            self.calls.fetch_add(1, Ordering::SeqCst);
            TaskResult::success(task.description.clone())
        }
    }

    #[test]
    fn run_executes_exactly_once() {
        let executor = CountingExecutor {
            calls: AtomicUsize::new(0),
        };
        let sub_agent = SubAgent::new(&executor);
        let task = Task::new("do the thing");
        let scope = TaskScope::for_task(&task, PermissionLevel::GodMode);
        let result = sub_agent.run(&task, &scope);
        assert_eq!(result.summary, "do the thing");
        assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
    }
}
