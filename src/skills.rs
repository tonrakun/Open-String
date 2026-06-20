use std::path::{Path, PathBuf};

pub const SKILLS_DIR: &str = "skills";

/// One SKILLS-format extension: a Markdown file with a small YAML
/// frontmatter block (`name`, `description`) followed by the instructions
/// the Mediator/Sub Agent should follow when invoking it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub body: String,
    pub path: PathBuf,
}

/// The directory a workspace's (or the global) SKILLS files are loaded
/// from, exposed so callers that need to watch it for changes (5.5's
/// hot reload) don't have to duplicate this layout decision.
pub fn skills_dir(workspace: Option<&Path>) -> PathBuf {
    match workspace {
        Some(ws) => ws.join(".open-string").join(SKILLS_DIR),
        None => PathBuf::from(SKILLS_DIR),
    }
}

/// Loads every SKILLS-format Markdown file from a workspace's (or the
/// global) `skills/` directory (5.1's "SKILLS形式の拡張機能読み込み機構").
/// A file that fails to parse is skipped rather than failing the whole
/// load -- the same fail-soft policy `load_connected_extensions` already
/// uses for the Extension manifest.
pub fn load_skills(workspace: Option<&Path>) -> Vec<Skill> {
    let dir = skills_dir(workspace);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };

    let mut skills = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
            continue;
        }
        let Ok(contents) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Some(skill) = parse_skill(&contents, path.clone()) {
            skills.push(skill);
        }
    }
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

/// Parses a SKILLS Markdown file's `---`-delimited frontmatter (flat
/// `key: value` lines only, no nesting) and body. Returns `None` when the
/// frontmatter is missing or has no `name`, since that field is required
/// to refer to the skill at all.
fn parse_skill(contents: &str, path: PathBuf) -> Option<Skill> {
    let rest = contents.strip_prefix("---\n")?;
    let end = rest.find("\n---")?;
    let frontmatter = &rest[..end];
    let body = rest[end..]
        .trim_start_matches("\n---")
        .trim_start()
        .to_string();

    let mut name = None;
    let mut description = None;
    for line in frontmatter.lines() {
        if let Some((key, value)) = line.split_once(':') {
            match key.trim() {
                "name" => name = Some(value.trim().to_string()),
                "description" => description = Some(value.trim().to_string()),
                _ => {}
            }
        }
    }

    Some(Skill {
        name: name?,
        description: description.unwrap_or_default(),
        body,
        path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_workspace() -> PathBuf {
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = env::temp_dir().join(format!("open-string-skills-test-{id}"));
        std::fs::create_dir_all(dir.join(".open-string").join(SKILLS_DIR)).unwrap();
        dir
    }

    #[test]
    fn parse_skill_extracts_name_description_and_body() {
        let contents = "---\nname: deploy\ndescription: Deploys the app\n---\nRun `cargo build --release` then ship the binary.\n";
        let skill = parse_skill(contents, PathBuf::from("deploy.md")).unwrap();
        assert_eq!(skill.name, "deploy");
        assert_eq!(skill.description, "Deploys the app");
        assert_eq!(
            skill.body,
            "Run `cargo build --release` then ship the binary.\n"
        );
    }

    #[test]
    fn parse_skill_returns_none_without_a_name_field() {
        let contents = "---\ndescription: missing a name\n---\nbody\n";
        assert!(parse_skill(contents, PathBuf::from("x.md")).is_none());
    }

    #[test]
    fn parse_skill_returns_none_without_frontmatter() {
        assert!(parse_skill("just a plain markdown file\n", PathBuf::from("x.md")).is_none());
    }

    #[test]
    fn load_skills_reads_every_md_file_in_the_workspace_skills_dir_sorted_by_name() {
        let workspace = temp_workspace();
        let dir = workspace.join(".open-string").join(SKILLS_DIR);
        std::fs::write(
            dir.join("b-skill.md"),
            "---\nname: b-skill\ndescription: second\n---\nbody b\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("a-skill.md"),
            "---\nname: a-skill\ndescription: first\n---\nbody a\n",
        )
        .unwrap();
        std::fs::write(dir.join("not-a-skill.txt"), "ignored").unwrap();

        let skills = load_skills(Some(&workspace));
        assert_eq!(skills.len(), 2);
        assert_eq!(skills[0].name, "a-skill");
        assert_eq!(skills[1].name, "b-skill");

        std::fs::remove_dir_all(&workspace).ok();
    }

    #[test]
    fn load_skills_returns_empty_when_the_directory_does_not_exist() {
        let workspace = env::temp_dir().join("open-string-skills-missing-test");
        let _ = std::fs::remove_dir_all(&workspace);
        assert!(load_skills(Some(&workspace)).is_empty());
    }
}
