//! 4.9: self-update. Checks GitHub Releases for a newer published version
//! of this binary and, on confirmation, downloads the matching platform
//! asset and replaces the currently running executable in place.

use serde::Deserialize;
use std::path::{Path, PathBuf};

const REPO_OWNER: &str = "tonrakun";
const REPO_NAME: &str = "Open-String";
const USER_AGENT: &str = concat!("open-string-cli/", env!("CARGO_PKG_VERSION"));

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateCheck {
    pub current_version: String,
    pub latest_version: String,
    pub is_newer: bool,
    pub download_url: Option<String>,
}

#[derive(Deserialize)]
struct ReleaseResponse {
    tag_name: String,
    assets: Vec<ReleaseAsset>,
}

#[derive(Deserialize)]
struct ReleaseAsset {
    name: String,
    browser_download_url: String,
}

/// Maps an OS/arch pair to the standalone binary asset name published by
/// `.github/workflows/release.yml`. Only the platforms that workflow
/// actually builds are recognized. macOS's GitHub-hosted `macos-latest`
/// runner builds natively for `aarch64` (Apple Silicon), not `x86_64`, so
/// that's the only Mac arch published; Windows/Linux runners stay `x86_64`.
fn asset_name_for(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("windows", "x86_64") => Some("open-string-windows-x86_64.exe"),
        ("macos", "aarch64") => Some("open-string-macos-aarch64"),
        ("linux", "x86_64") => Some("open-string-linux-x86_64"),
        _ => None,
    }
}

fn platform_asset_name() -> Option<&'static str> {
    asset_name_for(std::env::consts::OS, std::env::consts::ARCH)
}

/// Parses a `major.minor.patch` version string (with or without a leading
/// `v`) into a tuple for ordering. Open String never publishes pre-release
/// suffixes, so this stays a plain numeric compare rather than pulling in a
/// full semver dependency.
fn parse_version(raw: &str) -> Option<(u64, u64, u64)> {
    let trimmed = raw.trim().trim_start_matches('v');
    let mut parts = trimmed.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    Some((major, minor, patch))
}

/// Queries GitHub's "latest release" endpoint and compares its tag against
/// `CARGO_PKG_VERSION`, resolving the download URL for this platform's
/// standalone binary asset if one was published.
pub fn check_for_update(client: &reqwest::blocking::Client) -> Result<UpdateCheck, String> {
    let url = format!("https://api.github.com/repos/{REPO_OWNER}/{REPO_NAME}/releases/latest");
    let response = client
        .get(&url)
        .header("User-Agent", USER_AGENT)
        .header("Accept", "application/vnd.github+json")
        .send()
        .map_err(|e| format!("failed to reach GitHub: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("GitHub API returned {}", response.status()));
    }
    let release: ReleaseResponse = response
        .json()
        .map_err(|e| format!("failed to parse GitHub response: {e}"))?;

    let current_version = env!("CARGO_PKG_VERSION").to_string();
    let current = parse_version(&current_version).ok_or("invalid current version")?;
    let latest = parse_version(&release.tag_name)
        .ok_or_else(|| format!("unrecognized release tag: {}", release.tag_name))?;
    let latest_version = release.tag_name.trim_start_matches('v').to_string();
    let is_newer = latest > current;

    let download_url = platform_asset_name().and_then(|asset_name| {
        release
            .assets
            .iter()
            .find(|a| a.name == asset_name)
            .map(|a| a.browser_download_url.clone())
    });

    Ok(UpdateCheck {
        current_version,
        latest_version,
        is_newer,
        download_url,
    })
}

fn cleanup_stale_artifacts(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(".open-string-old-") || name.starts_with(".open-string-update-") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Downloads `download_url` and atomically replaces the currently running
/// executable. Renaming a binary's path is allowed on every supported OS
/// even while it's executing (only deleting/overwriting its content in
/// place is not), so the swap is: download to a sibling temp file, rename
/// the live exe out of the way, then rename the download into its place.
pub fn apply_update(
    client: &reqwest::blocking::Client,
    download_url: &str,
) -> Result<PathBuf, String> {
    let current_exe =
        std::env::current_exe().map_err(|e| format!("failed to locate running executable: {e}"))?;
    let parent = current_exe
        .parent()
        .ok_or("running executable has no parent directory")?;
    cleanup_stale_artifacts(parent);

    let mut response = client
        .get(download_url)
        .header("User-Agent", USER_AGENT)
        .send()
        .map_err(|e| format!("failed to download update: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("download failed with status {}", response.status()));
    }

    let pid = std::process::id();
    let tmp_path = parent.join(format!(".open-string-update-{pid}"));
    {
        let mut file = std::fs::File::create(&tmp_path)
            .map_err(|e| format!("failed to create temp file: {e}"))?;
        response
            .copy_to(&mut file)
            .map_err(|e| format!("failed to write downloaded binary: {e}"))?;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("failed to mark update executable: {e}"))?;
    }

    let old_path = parent.join(format!(".open-string-old-{pid}"));
    std::fs::rename(&current_exe, &old_path)
        .map_err(|e| format!("failed to move aside the running executable: {e}"))?;
    if let Err(e) = std::fs::rename(&tmp_path, &current_exe) {
        // Best-effort rollback so a failed swap doesn't leave the user without a binary.
        let _ = std::fs::rename(&old_path, &current_exe);
        return Err(format!("failed to install the new binary: {e}"));
    }
    // On Windows this removal fails while this process still holds the
    // image file open; `cleanup_stale_artifacts` sweeps it up on the next run.
    let _ = std::fs::remove_file(&old_path);

    Ok(current_exe)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_version_accepts_a_leading_v() {
        assert_eq!(parse_version("v1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("1.2.3"), Some((1, 2, 3)));
    }

    #[test]
    fn parse_version_rejects_malformed_input() {
        assert_eq!(parse_version("not-a-version"), None);
        assert_eq!(parse_version("1.2"), None);
    }

    #[test]
    fn asset_name_for_matches_release_workflow_naming() {
        // Exercised against literal (os, arch) pairs rather than gated on
        // `target_os`/`target_arch`, so this doesn't silently go stale the
        // next time a GitHub-hosted runner's default arch changes (as
        // happened when `macos-latest` moved to Apple Silicon).
        assert_eq!(
            asset_name_for("windows", "x86_64"),
            Some("open-string-windows-x86_64.exe")
        );
        assert_eq!(
            asset_name_for("macos", "aarch64"),
            Some("open-string-macos-aarch64")
        );
        assert_eq!(
            asset_name_for("linux", "x86_64"),
            Some("open-string-linux-x86_64")
        );
        assert_eq!(asset_name_for("macos", "x86_64"), None);
        assert_eq!(asset_name_for("linux", "aarch64"), None);
    }
}
