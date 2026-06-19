use super::aggregate::compute as compute_aggregate;
use super::{AggregatedReport, ConfirmationPrompt, SubAgent, Task, TaskExecutor, TaskResult};
use crate::permission::{
    AuditDecision, AuditEntry, AuditLogger, PermissionDecision, PermissionLevel, PermissionStore,
    classify_danger,
};

#[derive(Debug, thiserror::Error)]
pub enum DispatchError {
    #[error("operation was not confirmed by the user")]
    Denied,
    #[error("failed to read permission level: {0}")]
    PermissionLoadFailed(String),
}

/// Tunable knobs for how the Mediator runs Sub Agents in parallel (4.7.4).
#[derive(Debug, Clone, Copy)]
pub struct MediatorConfig {
    pub max_parallel_sub_agents: usize,
}

impl Default for MediatorConfig {
    fn default() -> Self {
        Self {
            max_parallel_sub_agents: 4,
        }
    }
}

/// The resident, user-facing side of the architecture (4.7.1). Holds the
/// permission store, confirmation prompt, and audit logger; `dispatch`/
/// `dispatch_many` are the only way to obtain and run a `SubAgent`, so the
/// permission pre-check can never be bypassed (4.1's "Mediator経由に一本化"
/// requirement).
pub struct Mediator<'a> {
    permission_store: &'a dyn PermissionStore,
    confirmation: &'a dyn ConfirmationPrompt,
    audit_logger: &'a dyn AuditLogger,
    config: MediatorConfig,
}

impl<'a> Mediator<'a> {
    pub fn new(
        permission_store: &'a dyn PermissionStore,
        confirmation: &'a dyn ConfirmationPrompt,
        audit_logger: &'a dyn AuditLogger,
    ) -> Self {
        Self {
            permission_store,
            confirmation,
            audit_logger,
            config: MediatorConfig::default(),
        }
    }

    pub fn with_config(mut self, config: MediatorConfig) -> Self {
        self.config = config;
        self
    }

    /// Runs a single task: permission pre-check, then (only if allowed) a
    /// freshly generated, disposable Sub Agent.
    pub fn dispatch(
        &self,
        task: &Task,
        executor: &dyn TaskExecutor,
    ) -> Result<TaskResult, DispatchError> {
        self.authorize(task)?;
        let sub_agent = SubAgent::new(executor);
        Ok(sub_agent.run(task))
    }

    /// Runs multiple tasks as parallel Sub Agents (4.7.4), capped at
    /// `max_parallel_sub_agents` running at once. Authorization happens
    /// sequentially first (interactive confirmation can't be parallelized),
    /// then permitted tasks run in batches. A denied or failed task does
    /// not abort the rest of the batch: every task's outcome is reported
    /// independently so the Mediator can act on whatever succeeded.
    pub fn dispatch_many(
        &self,
        tasks: &[Task],
        executor: &(dyn TaskExecutor + Sync),
    ) -> Vec<Result<TaskResult, DispatchError>> {
        let mut results: Vec<Option<Result<TaskResult, DispatchError>>> =
            (0..tasks.len()).map(|_| None).collect();
        let mut authorized_indices = Vec::new();

        for (i, task) in tasks.iter().enumerate() {
            match self.authorize(task) {
                Ok(()) => authorized_indices.push(i),
                Err(e) => results[i] = Some(Err(e)),
            }
        }

        let max_parallel = self.config.max_parallel_sub_agents.max(1);
        for chunk in authorized_indices.chunks(max_parallel) {
            std::thread::scope(|scope| {
                let handles: Vec<_> = chunk
                    .iter()
                    .map(|&i| {
                        let task = &tasks[i];
                        scope.spawn(move || {
                            let sub_agent = SubAgent::new(executor);
                            (i, sub_agent.run(task))
                        })
                    })
                    .collect();
                for handle in handles {
                    let (i, result) = handle.join().expect("sub agent thread panicked");
                    results[i] = Some(Ok(result));
                }
            });
        }

        results
            .into_iter()
            .map(|r| r.expect("every task index is filled by either authorize or the scoped run"))
            .collect()
    }

    /// Collapses `dispatch_many`'s per-task results into a single report
    /// (4.7.4): Sub Agents that ran the same task description and agreed
    /// are deduplicated into one `AggregatedItem`; ones that disagreed are
    /// surfaced as a `Conflict` with a majority-vote resolution instead of
    /// having one result silently picked over the other.
    pub fn aggregate(
        &self,
        tasks: &[Task],
        results: &[Result<TaskResult, DispatchError>],
    ) -> AggregatedReport {
        compute_aggregate(tasks, results)
    }

    /// Convenience wrapper: runs `dispatch_many` then aggregates the result
    /// in one call.
    pub fn dispatch_many_aggregated(
        &self,
        tasks: &[Task],
        executor: &(dyn TaskExecutor + Sync),
    ) -> AggregatedReport {
        let results = self.dispatch_many(tasks, executor);
        self.aggregate(tasks, &results)
    }

    fn authorize(&self, task: &Task) -> Result<(), DispatchError> {
        let level = self
            .permission_store
            .load()
            .map_err(|e| DispatchError::PermissionLoadFailed(e.to_string()))?;
        let danger = classify_danger(&task.description);

        match level.decide(&danger, task.read_only) {
            PermissionDecision::AutoAllow => {
                self.log(level, task, AuditDecision::Allowed);
                Ok(())
            }
            PermissionDecision::RequireConfirmation => {
                if self.confirmation.confirm(&task.description) {
                    self.log(level, task, AuditDecision::ConfirmedByUser);
                    Ok(())
                } else {
                    self.log(level, task, AuditDecision::DeclinedByUser);
                    Err(DispatchError::Denied)
                }
            }
        }
    }

    fn log(&self, level: PermissionLevel, task: &Task, decision: AuditDecision) {
        let entry = AuditEntry {
            level,
            operation: &task.description,
            decision,
        };
        if let Err(e) = self.audit_logger.record(&entry) {
            eprintln!("warning: failed to record audit log entry: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::TaskOutcome;
    use crate::permission::PermissionError;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FixedPermissionStore(PermissionLevel);
    impl PermissionStore for FixedPermissionStore {
        fn load(&self) -> Result<PermissionLevel, PermissionError> {
            Ok(self.0)
        }
        fn set(&self, _level: PermissionLevel) -> Result<(), PermissionError> {
            Ok(())
        }
    }

    struct NoOpAuditLogger;
    impl AuditLogger for NoOpAuditLogger {
        fn record(&self, _entry: &AuditEntry) -> Result<(), PermissionError> {
            Ok(())
        }
    }

    struct FixedConfirmation(bool, AtomicUsize);
    impl ConfirmationPrompt for FixedConfirmation {
        fn confirm(&self, _summary: &str) -> bool {
            self.1.fetch_add(1, Ordering::SeqCst);
            self.0
        }
    }

    struct CountingExecutor {
        calls: AtomicUsize,
        max_concurrent_seen: AtomicUsize,
        in_flight: AtomicUsize,
    }

    impl CountingExecutor {
        fn new() -> Self {
            Self {
                calls: AtomicUsize::new(0),
                max_concurrent_seen: AtomicUsize::new(0),
                in_flight: AtomicUsize::new(0),
            }
        }
    }

    impl TaskExecutor for CountingExecutor {
        fn execute(&self, task: &Task) -> TaskResult {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let current = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_concurrent_seen
                .fetch_max(current, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(20));
            self.in_flight.fetch_sub(1, Ordering::SeqCst);

            if task.description.contains("fail") {
                TaskResult::failure("boom")
            } else {
                TaskResult::success("ok")
            }
        }
    }

    #[test]
    fn auto_allow_runs_without_confirmation() {
        let store = FixedPermissionStore(PermissionLevel::GodMode);
        let confirmation = FixedConfirmation(false, AtomicUsize::new(0));
        let audit = NoOpAuditLogger;
        let mediator = Mediator::new(&store, &confirmation, &audit);
        let executor = CountingExecutor::new();

        let result = mediator
            .dispatch(&Task::new("delete everything"), &executor)
            .unwrap();

        assert_eq!(result.summary, "ok");
        assert_eq!(confirmation.1.load(Ordering::SeqCst), 0);
        assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn require_confirmation_runs_executor_only_when_confirmed() {
        let store = FixedPermissionStore(PermissionLevel::HighProtect);
        let confirmation = FixedConfirmation(true, AtomicUsize::new(0));
        let audit = NoOpAuditLogger;
        let mediator = Mediator::new(&store, &confirmation, &audit);
        let executor = CountingExecutor::new();

        let result = mediator.dispatch(&Task::new("write a file"), &executor);

        assert!(result.is_ok());
        assert_eq!(confirmation.1.load(Ordering::SeqCst), 1);
        assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn declined_confirmation_denies_dispatch_without_running_executor() {
        let store = FixedPermissionStore(PermissionLevel::HighProtect);
        let confirmation = FixedConfirmation(false, AtomicUsize::new(0));
        let audit = NoOpAuditLogger;
        let mediator = Mediator::new(&store, &confirmation, &audit);
        let executor = CountingExecutor::new();

        let result = mediator.dispatch(&Task::new("write a file"), &executor);

        assert!(matches!(result, Err(DispatchError::Denied)));
        assert_eq!(executor.calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn dispatch_many_continues_past_denied_and_failed_tasks() {
        let store = FixedPermissionStore(PermissionLevel::HighProtect);
        let confirmation = FixedConfirmation(false, AtomicUsize::new(0));
        let audit = NoOpAuditLogger;
        let mediator = Mediator::new(&store, &confirmation, &audit);
        let executor = CountingExecutor::new();

        let tasks = vec![
            Task::read_only("read config"),
            Task::new("write a file"),
            Task::read_only("fail on purpose"),
        ];

        let results = mediator.dispatch_many(&tasks, &executor);

        assert_eq!(results.len(), 3);
        assert!(matches!(results[0], Ok(ref r) if r.summary == "ok"));
        assert!(matches!(results[1], Err(DispatchError::Denied)));
        assert!(matches!(results[2], Ok(ref r) if r.outcome == TaskOutcome::Failure));
    }

    #[test]
    fn dispatch_many_respects_max_parallel_sub_agents() {
        let store = FixedPermissionStore(PermissionLevel::GodMode);
        let confirmation = FixedConfirmation(false, AtomicUsize::new(0));
        let audit = NoOpAuditLogger;
        let mediator = Mediator::new(&store, &confirmation, &audit).with_config(MediatorConfig {
            max_parallel_sub_agents: 2,
        });
        let executor = CountingExecutor::new();

        let tasks: Vec<Task> = (0..6).map(|i| Task::new(format!("task {i}"))).collect();
        let results = mediator.dispatch_many(&tasks, &executor);

        assert_eq!(results.len(), 6);
        assert!(results.iter().all(|r| r.is_ok()));
        assert!(executor.max_concurrent_seen.load(Ordering::SeqCst) <= 2);
        assert_eq!(executor.calls.load(Ordering::SeqCst), 6);
    }

    #[test]
    fn dispatch_many_aggregated_deduplicates_agreeing_results() {
        let store = FixedPermissionStore(PermissionLevel::GodMode);
        let confirmation = FixedConfirmation(false, AtomicUsize::new(0));
        let audit = NoOpAuditLogger;
        let mediator = Mediator::new(&store, &confirmation, &audit);
        let executor = CountingExecutor::new();

        let tasks = vec![
            Task::read_only("check disk space"),
            Task::read_only("check disk space"),
            Task::read_only("check disk space"),
        ];

        let report = mediator.dispatch_many_aggregated(&tasks, &executor);

        assert_eq!(report.items.len(), 1);
        assert_eq!(report.items[0].duplicate_count, 3);
        assert!(report.conflicts.is_empty());
    }
}
