use super::Task;
use crate::permission::PermissionLevel;

/// Names of work-system tools a Sub Agent may be offered. Kept as plain
/// strings (rather than an enum tied to `tools.rs`) so the executor that
/// actually knows how to build a `ToolDefinition` for each name stays the
/// only place coupled to the tool registry.
const READ_ONLY_TOOLS: &[&str] = &["read_file", "fetch_url"];
const FULL_TOOLS: &[&str] = &["read_file", "write_file", "run_command", "fetch_url"];

/// The scope a Mediator hands a Sub Agent alongside a `Task` (4.7.1): which
/// permission level authorized the task, and which tools the Sub Agent is
/// allowed to use. The Mediator builds this once, during `authorize`, so a
/// Sub Agent's tool access is always traceable back to a permission
/// decision the Mediator already made — the executor never derives policy
/// on its own (4.7.2's "ツール access自体をスコープで制限").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskScope {
    pub permission_level: PermissionLevel,
    pub allowed_tools: Vec<&'static str>,
}

impl TaskScope {
    pub fn for_task(task: &Task, permission_level: PermissionLevel) -> Self {
        let allowed_tools = if task.read_only {
            READ_ONLY_TOOLS.to_vec()
        } else {
            FULL_TOOLS.to_vec()
        };
        Self {
            permission_level,
            allowed_tools,
        }
    }

    pub fn allows(&self, tool_name: &str) -> bool {
        self.allowed_tools.contains(&tool_name)
    }

    /// True when no tool capable of a write/delete/external-effect action
    /// is in scope. Used to decide whether the read-only system prompt
    /// suffix is still warranted on top of the tool-level restriction.
    pub fn is_read_only(&self) -> bool {
        !self.allows("write_file") && !self.allows("run_command")
    }

    /// Renders the scope as a system-prompt fragment so the Sub Agent's
    /// model sees exactly what it's authorized to do, in its own words
    /// rather than the Mediator's internal types.
    pub fn describe(&self) -> String {
        format!(
            "Active permission level: {}. Tools available for this task: {}.",
            self.permission_level,
            self.allowed_tools.join(", ")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_task_gets_no_write_or_command_tools() {
        let scope = TaskScope::for_task(
            &Task::read_only("inspect config"),
            PermissionLevel::HighProtect,
        );
        assert!(scope.allows("read_file"));
        assert!(scope.allows("fetch_url"));
        assert!(!scope.allows("write_file"));
        assert!(!scope.allows("run_command"));
        assert!(scope.is_read_only());
    }

    #[test]
    fn writable_task_gets_the_full_tool_set() {
        let scope = TaskScope::for_task(&Task::new("write a report"), PermissionLevel::GodMode);
        assert!(scope.allows("write_file"));
        assert!(scope.allows("run_command"));
        assert!(!scope.is_read_only());
    }

    #[test]
    fn describe_includes_permission_level_and_tool_list() {
        let scope = TaskScope::for_task(
            &Task::read_only("inspect config"),
            PermissionLevel::HighProtect,
        );
        let description = scope.describe();
        assert!(description.contains("high-protect"));
        assert!(description.contains("read_file"));
    }
}
