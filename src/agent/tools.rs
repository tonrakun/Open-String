use std::process::Command;

use crate::llm::ToolDefinition;

/// Work-system tools available to a Sub Agent (4.7.2). Which of these are
/// handed to the model is decided by the caller based on `Task::read_only`
/// (see `claude_executor.rs`) — `run_command`/`write_file` are simply never
/// listed for a read-only task, so the model has no way to invoke them.
pub fn read_file_tool() -> ToolDefinition {
    ToolDefinition {
        name: "read_file".to_string(),
        description: "Read the full contents of a text file at the given path.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the file to read." }
            },
            "required": ["path"]
        }),
    }
}

pub fn write_file_tool() -> ToolDefinition {
    ToolDefinition {
        name: "write_file".to_string(),
        description:
            "Write text content to a file at the given path, overwriting it if it already exists."
                .to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the file to write." },
                "content": { "type": "string", "description": "Text content to write." }
            },
            "required": ["path", "content"]
        }),
    }
}

pub fn run_command_tool() -> ToolDefinition {
    ToolDefinition {
        name: "run_command".to_string(),
        description: "Run a shell command and return its combined stdout/stderr.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "The shell command line to execute." }
            },
            "required": ["command"]
        }),
    }
}

/// Executes one tool call by name. Returns the text to send back as the
/// `tool_result` content on success, or an error message to send back as
/// an error `tool_result` on failure (4.7.3: the Sub Agent itself decides
/// how to compress this back to the model, not just to the Mediator).
pub fn execute(name: &str, input: &serde_json::Value) -> Result<String, String> {
    match name {
        "read_file" => {
            let path = string_arg(input, "path")?;
            std::fs::read_to_string(path).map_err(|e| format!("failed to read {path}: {e}"))
        }
        "write_file" => {
            let path = string_arg(input, "path")?;
            let content = string_arg(input, "content")?;
            std::fs::write(path, content)
                .map(|_| format!("wrote {} bytes to {path}", content.len()))
                .map_err(|e| format!("failed to write {path}: {e}"))
        }
        "run_command" => run_shell_command(string_arg(input, "command")?),
        other => Err(format!("unknown tool: {other}")),
    }
}

fn string_arg<'a>(input: &'a serde_json::Value, field: &str) -> Result<&'a str, String> {
    input
        .get(field)
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("missing required field: {field}"))
}

fn run_shell_command(command: &str) -> Result<String, String> {
    let output = if cfg!(target_os = "windows") {
        Command::new("cmd").args(["/C", command]).output()
    } else {
        Command::new("sh").args(["-c", command]).output()
    }
    .map_err(|e| format!("failed to spawn command: {e}"))?;

    let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
    if !output.stderr.is_empty() {
        combined.push_str("\n[stderr]\n");
        combined.push_str(&String::from_utf8_lossy(&output.stderr));
    }

    if output.status.success() {
        Ok(combined)
    } else {
        Err(format!(
            "command exited with status {}: {combined}",
            output.status
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_file_round_trips_through_write_file() {
        let path = std::env::temp_dir().join("open_string_tools_test_read_write.txt");
        let path_str = path.to_string_lossy().to_string();

        let write_result = execute(
            "write_file",
            &serde_json::json!({"path": path_str, "content": "hello tools"}),
        )
        .unwrap();
        assert!(write_result.contains("wrote"));

        let read_result = execute("read_file", &serde_json::json!({"path": path_str})).unwrap();
        assert_eq!(read_result, "hello tools");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn read_file_reports_missing_file_as_an_error() {
        let err = execute(
            "read_file",
            &serde_json::json!({"path": "this/path/does/not/exist.txt"}),
        )
        .unwrap_err();
        assert!(err.contains("failed to read"));
    }

    #[test]
    fn run_command_captures_stdout() {
        let output = execute("run_command", &serde_json::json!({"command": "echo hello"})).unwrap();
        assert!(output.contains("hello"));
    }

    #[test]
    fn run_command_reports_failure_on_nonzero_exit() {
        let err = execute("run_command", &serde_json::json!({"command": "exit 1"})).unwrap_err();
        assert!(err.contains("exited with status"));
    }

    #[test]
    fn unknown_tool_name_is_an_error() {
        let err = execute("delete_universe", &serde_json::json!({})).unwrap_err();
        assert!(err.contains("unknown tool"));
    }
}
