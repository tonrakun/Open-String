use super::config::McpServerConfig;
use std::path::{Path, PathBuf};

/// Name the official Extension is registered under in `.mcp.json`,
/// matching `t0k3n setup`'s own default entry name so an existing
/// `.mcp.json` written by the user (or by `t0k3n setup` directly) is
/// recognized rather than duplicated.
pub const T0K3N_EXTENSION_NAME: &str = "t0k3n";

/// Resolves where `tonrakun/t0k3n-mcp`'s own `install.sh`/`install.ps1`
/// places the binary: `~/.t0k3n-mcp/t0k3n` on Unix, `%USERPROFILE%\
/// t0k3n-mcp\t0k3n.exe` on Windows. Returns `None` when the home directory
/// can't be determined or nothing is installed there.
pub fn installed_binary_path() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    let path = if cfg!(windows) {
        home.join("t0k3n-mcp").join("t0k3n.exe")
    } else {
        home.join(".t0k3n-mcp").join("t0k3n")
    };
    path.is_file().then_some(path)
}

fn on_path(name: &str) -> bool {
    let Some(path_var) = std::env::var_os("PATH") else {
        return false;
    };
    let exe_name = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    std::env::split_paths(&path_var).any(|dir| dir.join(&exe_name).is_file())
}

/// Whether t0k3n-mcp appears to be installed already, either at its known
/// install location or somewhere else on `PATH`. Open String only detects
/// an existing install here -- it never downloads or runs an installer on
/// the user's behalf (5.4's "無断導入を防止" principle applies just as
/// much to the official Extension as to a third-party one).
pub fn is_available() -> bool {
    installed_binary_path().is_some() || on_path("t0k3n")
}

/// Whether `name` refers to a bundled/officially verified Extension rather
/// than a third-party one. Backs both 5.3's per-call sandboxing (only
/// non-bundled tool calls go through the extra permission gate) and 5.4's
/// "信頼できないソース" warning shown before a new Extension is connected.
/// The built-in `extension install` catalog (5.3) is curated by Open
/// String itself the same way t0k3n-mcp is, so it's trusted too; a local
/// catalog file's entries are not, since nothing vets those.
pub fn is_trusted_extension_name(name: &str) -> bool {
    name == T0K3N_EXTENSION_NAME
        || super::catalog::builtin_catalog()
            .iter()
            .any(|e| e.name == name)
}

/// The command Open String should launch t0k3n-mcp with: its known
/// install path when found there, otherwise the bare name resolved
/// through `PATH`.
pub fn resolve_command() -> String {
    installed_binary_path()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "t0k3n".to_string())
}

/// Builds the `.mcp.json` entry `workspace create` registers for the
/// official bundle (5.2): pinned to the workspace via `--root` (mirroring
/// `t0k3n setup`'s own output), with both memory-tool fields pointed at
/// t0k3n's single `memory_save` tool (it has no separate "index" tool;
/// 4.7.1's index entries are just additional `memory_save` calls under a
/// different key) and a default daily version-check interval (4.2.5).
pub fn default_server_config(workspace: &Path) -> McpServerConfig {
    McpServerConfig {
        command: resolve_command(),
        args: vec!["--root".to_string(), workspace.display().to_string()],
        memory_save_tool: Some("memory_save".to_string()),
        memory_index_tool: Some("memory_save".to_string()),
        update_check_interval_hours: Some(24),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_command_falls_back_to_the_bare_name_when_not_found() {
        // This test environment has no t0k3n install at the known path or
        // (almost certainly) on PATH, so this exercises the fallback.
        if installed_binary_path().is_none() && !on_path("t0k3n") {
            assert_eq!(resolve_command(), "t0k3n");
        }
    }

    #[test]
    fn trusts_only_the_bundled_extension_name() {
        assert!(is_trusted_extension_name(T0K3N_EXTENSION_NAME));
        assert!(!is_trusted_extension_name("some-third-party-server"));
    }

    #[test]
    fn default_server_config_pins_root_to_the_workspace() {
        let workspace = Path::new("/tmp/some-workspace");
        let config = default_server_config(workspace);
        assert_eq!(
            config.args,
            vec!["--root".to_string(), workspace.display().to_string()]
        );
        assert_eq!(config.memory_save_tool, Some("memory_save".to_string()));
        assert_eq!(config.memory_index_tool, Some("memory_save".to_string()));
    }
}
