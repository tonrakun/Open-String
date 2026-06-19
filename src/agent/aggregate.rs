use super::{DispatchError, Task, TaskOutcome, TaskResult};
use std::collections::HashMap;

/// One row of an aggregated report: every Sub Agent that worked on
/// `description` agreed, so the duplicate results were collapsed into a
/// single entry (4.7.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AggregatedItem {
    pub description: String,
    pub outcome: TaskOutcome,
    pub summary: String,
    pub duplicate_count: usize,
}

/// Multiple Sub Agents ran the same task description and disagreed on the
/// outcome and/or summary. The Mediator resolves the disagreement by
/// majority vote (ties favor `Failure`, the safer assumption) but keeps
/// every dissenting result visible rather than silently discarding it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    pub description: String,
    pub resolved_outcome: TaskOutcome,
    pub results: Vec<(TaskOutcome, String)>,
}

/// A task the Mediator's permission pre-check rejected before any Sub
/// Agent was generated for it (`reason` is the `DispatchError`'s message).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeniedTask {
    pub description: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AggregatedReport {
    pub items: Vec<AggregatedItem>,
    pub conflicts: Vec<Conflict>,
    pub denied: Vec<DeniedTask>,
}

pub(super) fn compute(
    tasks: &[Task],
    results: &[Result<TaskResult, DispatchError>],
) -> AggregatedReport {
    let mut groups: HashMap<&str, Vec<(TaskOutcome, &str)>> = HashMap::new();
    let mut order: Vec<&str> = Vec::new();
    let mut denied = Vec::new();

    for (task, result) in tasks.iter().zip(results.iter()) {
        match result {
            Ok(r) => {
                let description = task.description.as_str();
                if !groups.contains_key(description) {
                    order.push(description);
                }
                groups
                    .entry(description)
                    .or_default()
                    .push((r.outcome, r.summary.as_str()));
            }
            Err(e) => denied.push(DeniedTask {
                description: task.description.clone(),
                reason: e.to_string(),
            }),
        }
    }

    let mut items = Vec::new();
    let mut conflicts = Vec::new();

    for description in order {
        let group = &groups[description];
        let (first_outcome, first_summary) = group[0];
        let all_agree = group
            .iter()
            .all(|&(outcome, summary)| outcome == first_outcome && summary == first_summary);

        if all_agree {
            items.push(AggregatedItem {
                description: description.to_string(),
                outcome: first_outcome,
                summary: first_summary.to_string(),
                duplicate_count: group.len(),
            });
        } else {
            let success_count = group
                .iter()
                .filter(|(outcome, _)| *outcome == TaskOutcome::Success)
                .count();
            let failure_count = group.len() - success_count;
            let resolved_outcome = if success_count > failure_count {
                TaskOutcome::Success
            } else {
                TaskOutcome::Failure
            };

            conflicts.push(Conflict {
                description: description.to_string(),
                resolved_outcome,
                results: group
                    .iter()
                    .map(|&(outcome, summary)| (outcome, summary.to_string()))
                    .collect(),
            });
        }
    }

    AggregatedReport {
        items,
        conflicts,
        denied,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_results_for_the_same_description_collapse_into_one_item() {
        let tasks = vec![
            Task::read_only("check disk space"),
            Task::read_only("check disk space"),
            Task::read_only("check disk space"),
        ];
        let results = vec![
            Ok(TaskResult::success("42% used")),
            Ok(TaskResult::success("42% used")),
            Ok(TaskResult::success("42% used")),
        ];

        let report = compute(&tasks, &results);

        assert_eq!(report.items.len(), 1);
        assert_eq!(report.items[0].duplicate_count, 3);
        assert_eq!(report.items[0].summary, "42% used");
        assert!(report.conflicts.is_empty());
    }

    #[test]
    fn disagreeing_results_for_the_same_description_become_a_conflict() {
        let tasks = vec![
            Task::read_only("check disk space"),
            Task::read_only("check disk space"),
        ];
        let results = vec![
            Ok(TaskResult::success("42% used")),
            Ok(TaskResult::success("87% used")),
        ];

        let report = compute(&tasks, &results);

        assert!(report.items.is_empty());
        assert_eq!(report.conflicts.len(), 1);
        assert_eq!(report.conflicts[0].results.len(), 2);
        // tied 1-1 on outcome, but summaries differ -> still a conflict
        assert_eq!(report.conflicts[0].resolved_outcome, TaskOutcome::Success);
    }

    #[test]
    fn outcome_ties_resolve_to_failure() {
        let tasks = vec![Task::new("deploy service"), Task::new("deploy service")];
        let results = vec![
            Ok(TaskResult::success("deployed")),
            Ok(TaskResult::failure("timed out")),
        ];

        let report = compute(&tasks, &results);

        assert_eq!(report.conflicts.len(), 1);
        assert_eq!(report.conflicts[0].resolved_outcome, TaskOutcome::Failure);
    }

    #[test]
    fn denied_tasks_are_tracked_separately_from_successful_groups() {
        let tasks = vec![Task::new("write a file"), Task::read_only("read config")];
        let results = vec![
            Err(DispatchError::Denied),
            Ok(TaskResult::success("loaded")),
        ];

        let report = compute(&tasks, &results);

        assert_eq!(report.denied.len(), 1);
        assert_eq!(report.denied[0].description, "write a file");
        assert_eq!(report.denied[0].reason, DispatchError::Denied.to_string());
        assert_eq!(report.items.len(), 1);
    }

    #[test]
    fn distinct_descriptions_each_get_their_own_item() {
        let tasks = vec![Task::read_only("task a"), Task::read_only("task b")];
        let results = vec![
            Ok(TaskResult::success("result a")),
            Ok(TaskResult::success("result b")),
        ];

        let report = compute(&tasks, &results);

        assert_eq!(report.items.len(), 2);
        assert!(report.items.iter().all(|i| i.duplicate_count == 1));
    }
}
