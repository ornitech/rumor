//! Background "is a newer version available?" check.
//!
//! The check is tied to *how the binary was installed* so the suggested action
//! actually works: a Homebrew install is checked with `brew outdated` (and told
//! to `brew upgrade`), anything else is checked against the GitHub Releases API
//! (and told to download from there). Every failure path (offline, no brew, no
//! curl, parse error, timeout) collapses to `None` so the UI simply shows no
//! badge. macOS-only, so `curl` is always present and no HTTP crate is needed.

use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;
use tokio::process::Command;
use tokio::time::timeout;
use tracing::debug;

use crate::app::UpdateInfo;

const GITHUB_REPO: &str = "ornitech/rumor";
const CHECK_TIMEOUT: Duration = Duration::from_secs(30);
/// The curl one-liner installer, used to upgrade non-Homebrew installs in place.
const INSTALL_URL: &str = "https://raw.githubusercontent.com/ornitech/rumor/main/install.sh";

/// How the running binary was installed, inferred from its on-disk path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstallMedium {
    Homebrew,
    Other,
}

/// Homebrew symlinks `bin/rumor` into `<prefix>/Cellar/rumor/<ver>/bin/rumor`;
/// cargo, source builds, and manual tarball extracts never land under Cellar.
fn detect_medium() -> InstallMedium {
    std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map(|p| {
            if p.to_string_lossy().contains("/Cellar/rumor/") {
                InstallMedium::Homebrew
            } else {
                InstallMedium::Other
            }
        })
        .unwrap_or(InstallMedium::Other)
}

/// Upgrade the running binary in place using the same medium it was installed
/// with: `brew upgrade rumor` for Homebrew installs, otherwise re-running the
/// curl installer (which fetches the latest release). The upgrade tool inherits
/// this process's stdio so its progress is shown live. Backs the `rumor update`
/// subcommand.
pub async fn run_self_update() -> Result<()> {
    let (program, args, human) = match detect_medium() {
        InstallMedium::Homebrew => (
            "brew",
            vec!["upgrade".to_string(), "rumor".to_string()],
            "brew upgrade rumor".to_string(),
        ),
        InstallMedium::Other => {
            let pipeline = format!("curl -fsSL {INSTALL_URL} | sh");
            ("sh", vec!["-c".to_string(), pipeline.clone()], pipeline)
        }
    };

    println!("rumor update: running `{human}`");
    let status = Command::new(program)
        .args(&args)
        .status()
        .await
        .with_context(|| format!("could not run `{human}`"))?;
    if !status.success() {
        anyhow::bail!("update failed: `{human}` exited with {status}");
    }
    println!("rumor update: up to date");
    Ok(())
}

/// Check for an update through the medium the binary was installed with.
/// Returns `None` on any failure or when already up to date.
pub async fn check_for_update(current: &str) -> Option<UpdateInfo> {
    match detect_medium() {
        InstallMedium::Homebrew => check_brew().await,
        InstallMedium::Other => check_github(current).await,
    }
}

async fn check_brew() -> Option<UpdateInfo> {
    let out = timeout(
        CHECK_TIMEOUT,
        Command::new("brew")
            .args(["outdated", "--json=v2", "rumor"])
            .output(),
    )
    .await
    .ok()?
    .ok()?;

    if !out.status.success() {
        debug!("brew outdated exited non-zero");
        return None;
    }
    parse_brew(&out.stdout)
}

async fn check_github(current: &str) -> Option<UpdateInfo> {
    let url = format!("https://api.github.com/repos/{GITHUB_REPO}/releases/latest");
    let out = timeout(
        CHECK_TIMEOUT,
        Command::new("curl")
            .args(["-fsSL", "-H", "User-Agent: rumor", &url])
            .output(),
    )
    .await
    .ok()?
    .ok()?;

    if !out.status.success() {
        debug!("curl releases/latest exited non-zero");
        return None;
    }
    parse_github(&out.stdout, current)
}

#[derive(Deserialize)]
struct BrewOutdated {
    formulae: Vec<BrewFormula>,
}

#[derive(Deserialize)]
struct BrewFormula {
    name: String,
    current_version: String,
}

fn parse_brew(stdout: &[u8]) -> Option<UpdateInfo> {
    let parsed: BrewOutdated = serde_json::from_slice(stdout).ok()?;
    let f = parsed.formulae.into_iter().find(|f| f.name == "rumor")?;
    Some(UpdateInfo {
        latest: f.current_version,
        action: "brew upgrade rumor".to_string(),
    })
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
}

fn parse_github(stdout: &[u8], current: &str) -> Option<UpdateInfo> {
    let release: GithubRelease = serde_json::from_slice(stdout).ok()?;
    let latest = release.tag_name.trim_start_matches('v').to_string();
    if is_newer(&latest, current) {
        Some(UpdateInfo {
            latest,
            action: format!("download from github.com/{GITHUB_REPO}/releases"),
        })
    } else {
        None
    }
}

/// Parse a `major.minor.patch` string into a comparable tuple, ignoring any
/// pre-release/build suffix. Non-numeric or missing parts read as 0.
fn version_tuple(v: &str) -> (u64, u64, u64) {
    let core = v.split(['-', '+']).next().unwrap_or(v);
    let mut parts = core.split('.').map(|p| p.parse::<u64>().unwrap_or(0));
    (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    )
}

/// True if `latest` is a strictly higher version than `current`.
fn is_newer(latest: &str, current: &str) -> bool {
    version_tuple(latest) > version_tuple(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_versions_detected() {
        assert!(is_newer("0.12.0", "0.11.0"));
        assert!(is_newer("1.0.0", "0.11.0"));
        assert!(is_newer("0.11.1", "0.11.0"));
        assert!(!is_newer("0.11.0", "0.11.0"));
        assert!(!is_newer("0.10.0", "0.11.0"));
        assert!(!is_newer("0.11.0", "0.11.1"));
    }

    #[test]
    fn version_suffixes_ignored() {
        assert_eq!(version_tuple("0.11.0-rc1"), (0, 11, 0));
        assert_eq!(version_tuple("0.11.0+build5"), (0, 11, 0));
        assert_eq!(version_tuple("1.2"), (1, 2, 0));
    }

    #[test]
    fn github_parse_strips_v_and_compares() {
        let body = br#"{"tag_name":"v0.12.0","name":"x"}"#;
        let info = parse_github(body, "0.11.0").expect("update");
        assert_eq!(info.latest, "0.12.0");
        assert!(info.action.contains("releases"));

        // Same version -> no update.
        assert!(parse_github(br#"{"tag_name":"v0.11.0"}"#, "0.11.0").is_none());
    }

    #[test]
    fn brew_parse_reads_current_version() {
        let body = br#"{
            "formulae":[{"name":"rumor","installed_versions":["0.10.0"],"current_version":"0.11.0"}],
            "casks":[]
        }"#;
        let info = parse_brew(body).expect("update");
        assert_eq!(info.latest, "0.11.0");
        assert_eq!(info.action, "brew upgrade rumor");
    }

    #[test]
    fn brew_parse_empty_means_up_to_date() {
        assert!(parse_brew(br#"{"formulae":[],"casks":[]}"#).is_none());
    }
}
