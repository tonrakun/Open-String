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

/// Basic web access (4.7.2): a single HTTP GET, not a search engine. There
/// is no search-provider integration yet, so this only covers "fetch a
/// known URL", not "search the web for X".
pub fn fetch_url_tool() -> ToolDefinition {
    ToolDefinition {
        name: "fetch_url".to_string(),
        description:
            "Fetch a URL via HTTP GET and return the response body as text (truncated if very large)."
                .to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "description": "The URL to fetch." }
            },
            "required": ["url"]
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
        "fetch_url" => fetch_url(string_arg(input, "url")?),
        other => Err(format!("unknown tool: {other}")),
    }
}

/// Cap on how much of a fetched response body gets handed back, so a large
/// page can't blow up the Sub Agent's context (4.7.3's "compress to what's
/// minimally sufficient" applies just as much to tool results as to the
/// final summary).
const MAX_FETCH_CHARS: usize = 20_000;

fn fetch_url(url: &str) -> Result<String, String> {
    let response =
        reqwest::blocking::get(url).map_err(|e| format!("failed to fetch {url}: {e}"))?;
    let status = response.status();
    let body = response
        .text()
        .map_err(|e| format!("failed to read response body from {url}: {e}"))?;
    let truncated = truncate_chars(&body, MAX_FETCH_CHARS);
    if status.is_success() {
        Ok(truncated)
    } else {
        Err(format!("{url} returned status {status}: {truncated}"))
    }
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let mut truncated: String = s.chars().take(max_chars).collect();
        truncated.push_str("... [truncated]");
        truncated
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

    #[test]
    fn fetch_url_returns_the_response_body() {
        let server = httpmock::MockServer::start();
        server.mock(|when, then| {
            when.method(httpmock::Method::GET).path("/page");
            then.status(200).body("hello from the web");
        });

        let result = execute(
            "fetch_url",
            &serde_json::json!({"url": format!("{}/page", server.base_url())}),
        )
        .unwrap();

        assert_eq!(result, "hello from the web");
    }

    #[test]
    fn fetch_url_reports_non_success_status_as_an_error() {
        let server = httpmock::MockServer::start();
        server.mock(|when, then| {
            when.method(httpmock::Method::GET).path("/missing");
            then.status(404).body("not found");
        });

        let err = execute(
            "fetch_url",
            &serde_json::json!({"url": format!("{}/missing", server.base_url())}),
        )
        .unwrap_err();

        assert!(err.contains("404"));
    }

    #[test]
    fn fetch_url_truncates_very_large_bodies() {
        let server = httpmock::MockServer::start();
        let big_body = "x".repeat(MAX_FETCH_CHARS + 500);
        server.mock(|when, then| {
            when.method(httpmock::Method::GET).path("/big");
            then.status(200).body(big_body.clone());
        });

        let result = execute(
            "fetch_url",
            &serde_json::json!({"url": format!("{}/big", server.base_url())}),
        )
        .unwrap();

        assert!(result.ends_with("... [truncated]"));
        assert!(result.len() < big_body.len());
    }
}
