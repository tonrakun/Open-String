//! Fragment-based system prompt assembly for Sub Agents (requirement 4.2.1).
//!
//! Rather than a single fixed prompt string, the system prompt is composed
//! from small, independently versioned fragments selected by the current
//! permission level, read-only status, and the list of connected
//! Extensions. Each fragment carries a stable `id` and `version` so a
//! change to one fragment can be tracked/diffed across releases without
//! touching the others.

use std::path::Path;

use crate::permission::PermissionLevel;
use crate::skills::Skill;

/// A single named, versioned piece of system-prompt text.
#[derive(Debug, Clone, Copy)]
pub struct Fragment {
    pub id: &'static str,
    pub version: u32,
    pub text: &'static str,
}

impl Fragment {
    fn id_version(&self) -> (&'static str, u32) {
        (self.id, self.version)
    }
}

const NARRATION_BAN: Fragment = Fragment {
    id: "sub_agent.narration_ban",
    version: 1,
    text: "You are a disposable Sub Agent in the Open String \
system. You execute exactly one task and then terminate; you carry no state between \
invocations. Never narrate, explain, or describe what you are about to do or are doing \
(for example, never say things like \"I will search the web\" or \"Reading the file now\"). \
Respond only with the final result: the work outcome, any produced artifact paths, state \
changes, or error information. Compress your response to whatever is minimally sufficient \
for the Mediator to make its next decision.",
};

const READ_ONLY_SUFFIX: Fragment = Fragment {
    id: "sub_agent.read_only_suffix",
    version: 1,
    text: "\n\nThis task is read-only: do not perform any write, \
delete, send, or otherwise irreversible action.",
};

fn permission_fragment(level: PermissionLevel) -> Fragment {
    match level {
        PermissionLevel::GodMode => Fragment {
            id: "permission.god_mode",
            version: 1,
            text: "Active permission level: god mode. Every action, including destructive \
ones, is pre-authorized; do not pause for confirmation.",
        },
        PermissionLevel::LowSecurity => Fragment {
            id: "permission.low_security",
            version: 1,
            text: "Active permission level: low security. Most actions are pre-authorized; \
only irreversible actions (delete, send, billing, publish) require explicit confirmation \
before you perform them.",
        },
        PermissionLevel::MiddlePermission => Fragment {
            id: "permission.middle_permission",
            version: 1,
            text: "Active permission level: middle permission. Only actions inside the \
configured directory/command allowlist are pre-authorized; anything outside it requires \
confirmation before you perform it.",
        },
        PermissionLevel::HighProtect => Fragment {
            id: "permission.high_protect",
            version: 1,
            text: "Active permission level: high protect. Only read-only actions are \
pre-authorized; anything that writes, deletes, sends, or otherwise changes state requires \
confirmation before you perform it.",
        },
    }
}

/// Usage guidance for a connected Extension (4.2.1: 接続中Extension一覧に応じた
/// ツール説明の動的注入). Only Extensions that are actually connected should be
/// passed to [`SystemPromptBuilder::with_extensions`] — an unconnected
/// Extension must not contribute a fragment.
#[derive(Debug, Clone)]
pub struct ExtensionInfo {
    pub name: String,
    /// Instructions the Extension itself publishes (e.g. a `instructions`
    /// field/file in its manifest). `None` triggers the Core-generated
    /// fallback guide below.
    pub instructions: Option<String>,
}

impl ExtensionInfo {
    pub fn new(name: impl Into<String>, instructions: Option<String>) -> Self {
        Self {
            name: name.into(),
            instructions,
        }
    }

    fn fallback_instructions(&self) -> String {
        format!(
            "{} is connected as an Extension. Prefer its tools over ad-hoc \
equivalents when one covers the same need.",
            self.name
        )
    }

    fn fragment(&self) -> String {
        let body = match &self.instructions {
            Some(text) if !text.trim().is_empty() => text.clone(),
            _ => self.fallback_instructions(),
        };
        format!("\n\n## {} usage\n{}", self.name, body)
    }
}

/// Default filename for the per-workspace connected-Extensions manifest
/// (4.2.1: 接続中Extension一覧). Looked up relative to the workspace root, or
/// the current directory when no workspace is given.
pub const EXTENSIONS_MANIFEST_FILE: &str = "extensions.json";

#[derive(serde::Serialize, serde::Deserialize)]
struct ExtensionManifestEntry {
    name: String,
    instructions_path: Option<String>,
}

/// Upserts one entry in the connected-Extensions manifest by name (5.2's
/// "instructions/ドキュメントをCoreのプロンプト構築ロジックに自動連携",
/// reused by any Extension that wants to publish instructions, not just
/// the bundled one). A missing or corrupt manifest is treated as empty
/// rather than failing, mirroring `load_connected_extensions`'s own
/// fail-soft policy.
pub fn register_extension(
    workspace: Option<&Path>,
    name: &str,
    instructions_path: Option<&str>,
) -> Result<(), String> {
    let manifest_path = match workspace {
        Some(dir) => dir.join(EXTENSIONS_MANIFEST_FILE),
        None => Path::new(EXTENSIONS_MANIFEST_FILE).to_path_buf(),
    };

    let mut entries: Vec<ExtensionManifestEntry> = std::fs::read_to_string(&manifest_path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();
    entries.retain(|entry| entry.name != name);
    entries.push(ExtensionManifestEntry {
        name: name.to_string(),
        instructions_path: instructions_path.map(|p| p.to_string()),
    });

    let json = serde_json::to_string_pretty(&entries).map_err(|e| e.to_string())?;
    std::fs::write(&manifest_path, json).map_err(|e| e.to_string())
}

/// Reads the connected-Extensions manifest, if any, and resolves each
/// entry's published `instructions_path` into an [`ExtensionInfo`]. A
/// missing manifest, an unreadable instructions file, or an entry with no
/// `instructions_path` all degrade to the Core-generated fallback guide
/// rather than failing — an Extension only needs to be *listed* to get a
/// usage fragment.
pub fn load_connected_extensions(workspace: Option<&Path>) -> Vec<ExtensionInfo> {
    let manifest_path = match workspace {
        Some(dir) => dir.join(EXTENSIONS_MANIFEST_FILE),
        None => Path::new(EXTENSIONS_MANIFEST_FILE).to_path_buf(),
    };

    let Ok(raw) = std::fs::read_to_string(&manifest_path) else {
        return Vec::new();
    };
    let Ok(entries) = serde_json::from_str::<Vec<ExtensionManifestEntry>>(&raw) else {
        return Vec::new();
    };

    entries
        .into_iter()
        .map(|entry| {
            let instructions = entry
                .instructions_path
                .as_deref()
                .and_then(|path| std::fs::read_to_string(path).ok());
            ExtensionInfo::new(entry.name, instructions)
        })
        .collect()
}

/// Renders a loaded SKILLS file's full body into the system prompt (5.1's
/// "SKILLS形式の拡張機能読み込み機構"). Unlike an Extension, a Skill has no
/// separate Core-generated fallback guide -- its body is itself the
/// instructions, so there is nothing sensible to fall back to when it's
/// present at all.
fn skill_fragment(skill: &Skill) -> String {
    format!(
        "\n\n## Skill: {}\n{}\n\n{}",
        skill.name, skill.description, skill.body
    )
}

/// Builds a Sub Agent system prompt from fragments chosen for the current
/// permission level, read-only status, connected Extensions, and loaded
/// SKILLS.
pub struct SystemPromptBuilder<'a> {
    permission_level: PermissionLevel,
    read_only: bool,
    extensions: &'a [ExtensionInfo],
    skills: &'a [Skill],
    scope_description: Option<String>,
}

impl<'a> SystemPromptBuilder<'a> {
    pub fn new(permission_level: PermissionLevel, read_only: bool) -> Self {
        Self {
            permission_level,
            read_only,
            extensions: &[],
            skills: &[],
            scope_description: None,
        }
    }

    pub fn with_extensions(mut self, extensions: &'a [ExtensionInfo]) -> Self {
        self.extensions = extensions;
        self
    }

    /// Registers SKILLS loaded for this run so each one's body is injected
    /// into the system prompt (5.1/5.5). A Skill not passed here contributes
    /// no prompt fragment, mirroring how an unconnected Extension behaves.
    pub fn with_skills(mut self, skills: &'a [Skill]) -> Self {
        self.skills = skills;
        self
    }

    pub fn with_scope_description(mut self, description: impl Into<String>) -> Self {
        self.scope_description = Some(description.into());
        self
    }

    pub fn build(&self) -> String {
        let mut out = String::from(NARRATION_BAN.text);
        out.push_str("\n\n");
        out.push_str(permission_fragment(self.permission_level).text);

        if let Some(description) = &self.scope_description {
            out.push_str("\n\n");
            out.push_str(description);
        }

        for extension in self.extensions {
            out.push_str(&extension.fragment());
        }

        for skill in self.skills {
            out.push_str(&skill_fragment(skill));
        }

        if self.read_only {
            out.push_str(READ_ONLY_SUFFIX.text);
        }

        out
    }

    /// `(id, version)` pairs for every fragment this build draws from, so
    /// callers (health checks, dashboards) can track/diff the effective
    /// template set across releases without re-rendering the full prompt.
    pub fn template_versions(&self) -> Vec<(&'static str, u32)> {
        let mut versions = vec![
            NARRATION_BAN.id_version(),
            permission_fragment(self.permission_level).id_version(),
        ];
        if self.read_only {
            versions.push(READ_ONLY_SUFFIX.id_version());
        }
        versions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_includes_narration_ban_and_permission_fragment() {
        let prompt = SystemPromptBuilder::new(PermissionLevel::GodMode, false).build();
        assert!(prompt.contains("disposable Sub Agent"));
        assert!(prompt.contains("god mode"));
    }

    #[test]
    fn read_only_appends_the_read_only_suffix() {
        let prompt = SystemPromptBuilder::new(PermissionLevel::HighProtect, true).build();
        assert!(prompt.contains("high protect"));
        assert!(prompt.contains("read-only: do not perform"));
    }

    #[test]
    fn writable_prompt_omits_the_read_only_suffix() {
        let prompt = SystemPromptBuilder::new(PermissionLevel::HighProtect, false).build();
        assert!(!prompt.contains("read-only: do not perform"));
    }

    #[test]
    fn unconnected_extensions_contribute_no_fragment() {
        let prompt = SystemPromptBuilder::new(PermissionLevel::GodMode, false).build();
        assert!(!prompt.contains("usage"));
    }

    #[test]
    fn connected_extension_with_published_instructions_uses_them_verbatim() {
        let extensions = vec![ExtensionInfo::new(
            "t0k3n-mcp",
            Some("Call read_code_skeleton before read_code_body.".to_string()),
        )];
        let prompt = SystemPromptBuilder::new(PermissionLevel::GodMode, false)
            .with_extensions(&extensions)
            .build();
        assert!(prompt.contains("## t0k3n-mcp usage"));
        assert!(prompt.contains("Call read_code_skeleton before read_code_body."));
    }

    #[test]
    fn connected_extension_without_instructions_gets_a_fallback_guide() {
        let extensions = vec![ExtensionInfo::new("custom-ext", None)];
        let prompt = SystemPromptBuilder::new(PermissionLevel::GodMode, false)
            .with_extensions(&extensions)
            .build();
        assert!(prompt.contains("## custom-ext usage"));
        assert!(prompt.contains("custom-ext is connected as an Extension"));
    }

    #[test]
    fn unconnected_skills_contribute_no_fragment() {
        let prompt = SystemPromptBuilder::new(PermissionLevel::GodMode, false).build();
        assert!(!prompt.contains("## Skill:"));
    }

    #[test]
    fn loaded_skill_contributes_its_body_to_the_prompt() {
        let skills = vec![Skill {
            name: "deploy".to_string(),
            description: "Deploys the app".to_string(),
            body: "Run `cargo build --release` then ship the binary.".to_string(),
            path: std::path::PathBuf::from("deploy.md"),
        }];
        let prompt = SystemPromptBuilder::new(PermissionLevel::GodMode, false)
            .with_skills(&skills)
            .build();
        assert!(prompt.contains("## Skill: deploy"));
        assert!(prompt.contains("Deploys the app"));
        assert!(prompt.contains("Run `cargo build --release` then ship the binary."));
    }

    #[test]
    fn load_connected_extensions_returns_empty_when_no_manifest_exists() {
        let dir = std::env::temp_dir().join("open_string_no_manifest_test");
        let _ = std::fs::remove_file(dir.join(EXTENSIONS_MANIFEST_FILE));
        let _ = std::fs::create_dir_all(&dir);

        let extensions = load_connected_extensions(Some(&dir));

        assert!(extensions.is_empty());
    }

    #[test]
    fn load_connected_extensions_reads_manifest_and_instructions_file() {
        let dir = std::env::temp_dir().join("open_string_manifest_test");
        std::fs::create_dir_all(&dir).unwrap();
        let instructions_path = dir.join("t0k3n_instructions.md");
        std::fs::write(&instructions_path, "Use read_code_skeleton first.").unwrap();
        let manifest = serde_json::json!([
            {"name": "t0k3n-mcp", "instructions_path": instructions_path.to_string_lossy()},
            {"name": "no-instructions-ext"}
        ]);
        std::fs::write(dir.join(EXTENSIONS_MANIFEST_FILE), manifest.to_string()).unwrap();

        let extensions = load_connected_extensions(Some(&dir));

        assert_eq!(extensions.len(), 2);
        assert_eq!(extensions[0].name, "t0k3n-mcp");
        assert_eq!(
            extensions[0].instructions.as_deref(),
            Some("Use read_code_skeleton first.")
        );
        assert_eq!(extensions[1].name, "no-instructions-ext");
        assert_eq!(extensions[1].instructions, None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn template_versions_reflect_the_fragments_actually_used() {
        let read_only = SystemPromptBuilder::new(PermissionLevel::HighProtect, true);
        let versions = read_only.template_versions();
        assert!(versions.contains(&("sub_agent.narration_ban", 1)));
        assert!(versions.contains(&("permission.high_protect", 1)));
        assert!(versions.contains(&("sub_agent.read_only_suffix", 1)));

        let writable = SystemPromptBuilder::new(PermissionLevel::HighProtect, false);
        assert!(
            !writable
                .template_versions()
                .contains(&("sub_agent.read_only_suffix", 1))
        );
    }
}
