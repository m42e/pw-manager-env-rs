use crate::config;
use anyhow::{Context, Result};
use semver::Version;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const RELEASE_CHECK_STATE_FILE: &str = "release-check.json";
const GITHUB_OWNER: &str = "m42e";
const GITHUB_REPO: &str = "pw-manager-env-rs";
const RELEASE_API_URL: &str = "https://api.github.com/repos/m42e/pw-manager-env-rs/releases/latest";
const RELEASES_URL: &str = "https://github.com/m42e/pw-manager-env-rs/releases/latest";
const REQUEST_TIMEOUT_SECS: u64 = 2;

#[derive(Debug, Default, Deserialize, Serialize)]
struct ReleaseCheckState {
    last_checked_at: Option<u64>,
    last_notified_version: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    html_url: String,
}

#[derive(Debug, PartialEq, Eq)]
struct ReleaseInfo {
    version: String,
    html_url: String,
}

pub fn maybe_check_for_update(config: &config::Config) -> Result<()> {
    if !config.updates.enabled {
        return Ok(());
    }

    let Some(state_path) = state_path() else {
        return Ok(());
    };

    let interval = Duration::from_secs(config.updates.check_interval_hours.saturating_mul(3600));
    let now = now_unix_timestamp();

    let mut state = ReleaseCheckState::load(&state_path)?;
    if !state.is_due(now, interval) {
        return Ok(());
    }

    state.last_checked_at = Some(now);

    match fetch_latest_release() {
        Ok(release) => {
            if is_newer_release(&release.version)? {
                if state.last_notified_version.as_deref() != Some(release.version.as_str()) {
                    eprintln!(
                        "pw-env: update available: v{} (installed: v{})",
                        release.version,
                        env!("CARGO_PKG_VERSION")
                    );
                    eprintln!("pw-env: latest release: {}", release.html_url);
                    state.last_notified_version = Some(release.version);
                }
            } else {
                state.last_notified_version = None;
            }
        }
        Err(error) => {
            tracing::debug!(error = %error, "release check request failed");
        }
    }

    state.save(&state_path)
}

fn fetch_latest_release() -> Result<ReleaseInfo> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
        .build()
        .context("failed to build release check HTTP client")?;

    let release = client
        .get(RELEASE_API_URL)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .header(reqwest::header::USER_AGENT, user_agent())
        .send()
        .context("failed to query the latest GitHub release")?
        .error_for_status()
        .context("GitHub release API returned an error")?
        .json::<GithubRelease>()
        .context("failed to decode the latest GitHub release response")?;

    let version = normalize_version(&release.tag_name)?;
    let html_url = if release.html_url.trim().is_empty() {
        RELEASES_URL.to_string()
    } else {
        release.html_url
    };

    Ok(ReleaseInfo { version, html_url })
}

fn is_newer_release(latest_version: &str) -> Result<bool> {
    let current = Version::parse(env!("CARGO_PKG_VERSION"))
        .context("failed to parse the current application version")?;
    let latest = Version::parse(latest_version)
        .with_context(|| format!("failed to parse release version `{latest_version}`"))?;
    Ok(latest > current)
}

fn normalize_version(tag_name: &str) -> Result<String> {
    let version = tag_name.trim().trim_start_matches('v');
    Version::parse(version).with_context(|| format!("failed to parse release tag `{tag_name}`"))?;
    Ok(version.to_string())
}

fn user_agent() -> String {
    format!("{}/{}", GITHUB_REPO, env!("CARGO_PKG_VERSION"))
}

fn state_path() -> Option<PathBuf> {
    dirs::state_dir()
        .or_else(dirs::data_local_dir)
        .map(|dir| dir.join("pw-manager-env").join(RELEASE_CHECK_STATE_FILE))
}

fn now_unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

impl ReleaseCheckState {
    fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }

        let contents = std::fs::read_to_string(path).with_context(|| {
            format!("failed to read release check state from {}", path.display())
        })?;
        serde_json::from_str(&contents).with_context(|| {
            format!(
                "failed to parse release check state from {}",
                path.display()
            )
        })
    }

    fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("failed to create state directory {}", parent.display())
            })?;
        }

        let contents = serde_json::to_string_pretty(self)
            .context("failed to serialize release check state")?;
        std::fs::write(path, contents)
            .with_context(|| format!("failed to write release check state to {}", path.display()))
    }

    fn is_due(&self, now: u64, interval: Duration) -> bool {
        let Some(last_checked_at) = self.last_checked_at else {
            return true;
        };

        let interval_secs = interval.as_secs();
        if interval_secs == 0 {
            return true;
        }

        now.saturating_sub(last_checked_at) >= interval_secs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_v_prefix_from_release_tags() {
        assert_eq!(normalize_version("v1.2.3").unwrap(), "1.2.3");
    }

    #[test]
    fn accepts_release_tags_without_v_prefix() {
        assert_eq!(normalize_version("1.2.3").unwrap(), "1.2.3");
    }

    #[test]
    fn rejects_invalid_release_tags() {
        assert!(normalize_version("latest").is_err());
    }

    #[test]
    fn state_is_due_when_never_checked() {
        let state = ReleaseCheckState::default();
        assert!(state.is_due(1_000, Duration::from_secs(60)));
    }

    #[test]
    fn state_is_not_due_before_interval_elapses() {
        let state = ReleaseCheckState {
            last_checked_at: Some(1_000),
            last_notified_version: None,
        };
        assert!(!state.is_due(1_030, Duration::from_secs(60)));
    }

    #[test]
    fn state_is_due_after_interval_elapses() {
        let state = ReleaseCheckState {
            last_checked_at: Some(1_000),
            last_notified_version: None,
        };
        assert!(state.is_due(1_060, Duration::from_secs(60)));
    }

    #[test]
    fn user_agent_mentions_binary_and_version() {
        let agent = user_agent();
        assert!(agent.contains(GITHUB_REPO));
        assert!(agent.contains(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn github_release_owner_constant_matches_repo_url() {
        assert_eq!(GITHUB_OWNER, "m42e");
        assert!(RELEASE_API_URL.contains(GITHUB_OWNER));
    }
}
