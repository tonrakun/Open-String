use super::system_prompt;
use crate::mcp;
use std::path::Path;

/// Condensed usage guidance for the official t0k3n-mcp bundle (5.2's
/// "t0k3n-mcpのinstructions/ドキュメントをCoreのプロンプト構築ロジックに
/// 自動連携"). t0k3n-mcp does not publish this text over MCP itself
/// (there is no `instructions` field in its `initialize` response), so
/// Core ships a short summary of its own rather than leaving the
/// connected-Extensions fragment empty.
const T0K3N_INSTRUCTIONS: &str = "\
t0k3n-mcp is connected. Prefer its tools over plain file reads for code, \
since a full read sends far more tokens than needed:
- Read code structure first with read_code_skeleton, then read_code_body \
for only the symbols you actually need.
- Use project_digest at the start of a task for a cached architecture \
summary instead of walking the tree yourself.
- Use memory_save/get/list/delete for anything that should survive past \
this task, and session_snapshot/restore for resuming earlier work.
- Combine related reads into one batch_read call instead of many separate \
ones.";

const T0K3N_INSTRUCTIONS_FILE: &str = "t0k3n-instructions.md";

/// Registers the official t0k3n-mcp Extension for `workspace` (5.2) if the
/// binary is installed and no entry named `t0k3n` already exists: a
/// `.mcp.json` entry pinned to the workspace via `--root`, plus an
/// `extensions.json` entry pointing at a bundled instructions file so the
/// connected-Extension prompt fragment (4.2.1) has real guidance instead
/// of the generic fallback. Returns `false` (not an error) when t0k3n
/// isn't installed or is already registered -- Open String never installs
/// it automatically.
pub fn auto_register_t0k3n(workspace: &Path) -> Result<bool, String> {
    if !mcp::is_available() {
        return Ok(false);
    }

    let mut config = mcp::load(Some(workspace))?;
    if config.mcp_servers.contains_key(mcp::T0K3N_EXTENSION_NAME) {
        return Ok(false);
    }
    config.mcp_servers.insert(
        mcp::T0K3N_EXTENSION_NAME.to_string(),
        mcp::default_server_config(workspace),
    );
    mcp::save(Some(workspace), &config)?;

    let state_dir = workspace.join(".open-string");
    std::fs::create_dir_all(&state_dir).map_err(|e| e.to_string())?;
    let instructions_path = state_dir.join(T0K3N_INSTRUCTIONS_FILE);
    std::fs::write(&instructions_path, T0K3N_INSTRUCTIONS).map_err(|e| e.to_string())?;

    system_prompt::register_extension(
        Some(workspace),
        mcp::T0K3N_EXTENSION_NAME,
        Some(&instructions_path.display().to_string()),
    )?;

    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn does_not_register_when_t0k3n_is_not_installed() {
        // This test environment has no t0k3n binary installed, so
        // `mcp::is_available()` is false and nothing should be written.
        if mcp::is_available() {
            return;
        }
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let workspace = env::temp_dir().join(format!("open-string-bundled-ext-test-{id}"));
        std::fs::create_dir_all(&workspace).unwrap();

        let registered = auto_register_t0k3n(&workspace).unwrap();
        assert!(!registered);
        assert!(mcp::load(Some(&workspace)).unwrap().mcp_servers.is_empty());

        std::fs::remove_dir_all(&workspace).ok();
    }
}
