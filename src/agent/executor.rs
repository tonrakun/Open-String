use super::{Task, TaskResult};

/// What a Sub Agent actually runs to produce a result. Real implementations
/// will drive an LLM tool-use loop against the permitted tool set; until
/// that lands, any type satisfying this contract can stand in (tests use
/// canned executors, the CLI uses `EchoTaskExecutor` below).
pub trait TaskExecutor {
    fn execute(&self, task: &Task) -> TaskResult;
}

/// Reports back exactly what it was asked to do. Placeholder default until
/// a real LLM-backed executor lands (4.2/5.x); keeps the Mediator -> Sub
/// Agent pipeline exercisable end to end before then.
pub struct EchoTaskExecutor;

impl TaskExecutor for EchoTaskExecutor {
    fn execute(&self, task: &Task) -> TaskResult {
        if task.description.trim().is_empty() {
            TaskResult::failure("task description is empty")
        } else {
            TaskResult::success(format!("executed: {}", task.description))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::TaskOutcome;

    #[test]
    fn echoes_the_task_description_back_as_success() {
        let result = EchoTaskExecutor.execute(&Task::new("read config"));
        assert_eq!(result.outcome, TaskOutcome::Success);
        assert_eq!(result.summary, "executed: read config");
    }

    #[test]
    fn fails_on_empty_description() {
        let result = EchoTaskExecutor.execute(&Task::new("  "));
        assert_eq!(result.outcome, TaskOutcome::Failure);
    }
}
