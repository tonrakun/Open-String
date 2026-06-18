#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskOutcome {
    Success,
    Failure,
}

/// What a Sub Agent reports back to the Mediator (4.7.3). Deliberately not
/// a rigid schema: `summary` is free text so a Sub Agent can compress its
/// work into whatever shape carries the most information per token, rather
/// than being forced into fixed fields. The Mediator only ever sees this
/// reduced result, never the raw execution trace that produced it.
#[derive(Debug, Clone)]
pub struct TaskResult {
    pub outcome: TaskOutcome,
    pub summary: String,
}

impl TaskResult {
    pub fn success(summary: impl Into<String>) -> Self {
        Self {
            outcome: TaskOutcome::Success,
            summary: summary.into(),
        }
    }

    pub fn failure(summary: impl Into<String>) -> Self {
        Self {
            outcome: TaskOutcome::Failure,
            summary: summary.into(),
        }
    }
}
