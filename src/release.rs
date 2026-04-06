use crate::config;
use anyhow::{Context, Result};
use flate2::read::GzDecoder;
use semver::Version;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tar::Archive;
use tempfile::TempDir;
#[cfg(target_os = "windows")]
use zip::ZipArchive;

const RELEASE_CHECK_STATE_FILE: &str = "release-check.json";
const GITHUB_OWNER: &str = "m42e";
const GITHUB_REPO: &str = "pw-env";
const RELEASE_API_URL: &str = "https://api.github.com/repos/m42e/pw-env/releases/latest";
const RELEASES_URL: &str = "https://github.com/m42e/pw-env/releases/latest";
const REQUEST_TIMEOUT_SECS: u64 = 2;
const DOWNLOAD_TIMEOUT_SECS: u64 = 120;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
enum ArchiveFormat {
    TarGz,
    #[cfg(target_os = "windows")]
    Zip,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ReleaseAsset {
    target: &'static str,
    archive_format: ArchiveFormat,
    binary_name: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedRelease {
    tag: String,
    version: String,
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

pub fn update(requested_version: Option<&str>) -> Result<()> {
    let current_version = env!("CARGO_PKG_VERSION");
    let release = resolve_release(requested_version)?;

    if release.version == current_version {
        eprintln!("pw-env is already at v{}", current_version);
        return Ok(());
    }

    let asset = detect_release_asset()?;
    let archive_name = release_archive_name(&release.tag, &asset);
    let download_url = release_download_url(&release.tag, &archive_name);
    let current_exe =
        std::env::current_exe().context("failed to determine the current executable path")?;
    let tempdir = tempfile::Builder::new()
        .prefix("pw-env-update-")
        .tempdir()
        .context("failed to create a temporary directory for the update")?;
    let archive_path = tempdir.path().join(&archive_name);

    eprintln!(
        "pw-env: downloading v{} for {}",
        release.version, asset.target
    );
    download_file(&download_url, &archive_path)?;

    let extracted_binary_path = extract_binary_from_archive(&archive_path, &tempdir, &asset)
        .with_context(|| format!("failed to extract {}", archive_name))?;

    self_replace::self_replace(&extracted_binary_path).with_context(|| {
        format!(
            "failed to replace the current binary at {}",
            current_exe.display()
        )
    })?;

    eprintln!(
        "pw-env: updated {} from v{} to v{}",
        current_exe.display(),
        current_version,
        release.version
    );

    Ok(())
}

fn fetch_latest_release() -> Result<ReleaseInfo> {
    let client = http_client(Duration::from_secs(REQUEST_TIMEOUT_SECS))?;

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

fn resolve_release(requested_version: Option<&str>) -> Result<ResolvedRelease> {
    match requested_version {
        None => {
            let latest = fetch_latest_release()?;
            Ok(ResolvedRelease {
                tag: format!("v{}", latest.version),
                version: latest.version,
            })
        }
        Some(version) if version.trim().eq_ignore_ascii_case("latest") => {
            let latest = fetch_latest_release()?;
            Ok(ResolvedRelease {
                tag: format!("v{}", latest.version),
                version: latest.version,
            })
        }
        Some(version) => {
            let tag = normalize_tag(version);
            let normalized_version = normalize_version(&tag)?;
            Ok(ResolvedRelease {
                tag,
                version: normalized_version,
            })
        }
    }
}

fn detect_release_asset() -> Result<ReleaseAsset> {
    detect_release_asset_impl()
}

#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
fn detect_release_asset_impl() -> Result<ReleaseAsset> {
    Ok(ReleaseAsset {
        target: "x86_64-pc-windows-msvc",
        archive_format: ArchiveFormat::Zip,
        binary_name: "pw-env.exe",
    })
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn detect_release_asset_impl() -> Result<ReleaseAsset> {
    Ok(ReleaseAsset {
        target: "aarch64-apple-darwin",
        archive_format: ArchiveFormat::TarGz,
        binary_name: "pw-env",
    })
}

#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
fn detect_release_asset_impl() -> Result<ReleaseAsset> {
    Ok(ReleaseAsset {
        target: "x86_64-apple-darwin",
        archive_format: ArchiveFormat::TarGz,
        binary_name: "pw-env",
    })
}

#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
fn detect_release_asset_impl() -> Result<ReleaseAsset> {
    Ok(ReleaseAsset {
        target: "aarch64-unknown-linux-gnu",
        archive_format: ArchiveFormat::TarGz,
        binary_name: "pw-env",
    })
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn detect_release_asset_impl() -> Result<ReleaseAsset> {
    Ok(ReleaseAsset {
        target: "x86_64-unknown-linux-gnu",
        archive_format: ArchiveFormat::TarGz,
        binary_name: "pw-env",
    })
}

#[cfg(not(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64"),
    all(target_os = "macos", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "aarch64"),
    all(target_os = "linux", target_arch = "x86_64")
)))]
fn detect_release_asset_impl() -> Result<ReleaseAsset> {
    Err(anyhow::anyhow!(
        "self-update is not supported for this platform"
    ))
}

fn release_archive_name(tag: &str, asset: &ReleaseAsset) -> String {
    format!(
        "pw-env-{tag}-{}.{}",
        asset.target,
        asset.archive_format.extension()
    )
}

fn release_download_url(tag: &str, archive_name: &str) -> String {
    format!(
        "https://github.com/{}/{}/releases/download/{}/{}",
        GITHUB_OWNER, GITHUB_REPO, tag, archive_name
    )
}

fn download_file(url: &str, destination: &Path) -> Result<()> {
    let client = http_client(Duration::from_secs(DOWNLOAD_TIMEOUT_SECS))?;
    let mut response = client
        .get(url)
        .header(reqwest::header::USER_AGENT, user_agent())
        .send()
        .with_context(|| format!("failed to download {}", url))?
        .error_for_status()
        .with_context(|| format!("release download returned an error for {}", url))?;

    let mut file = File::create(destination)
        .with_context(|| format!("failed to create {}", destination.display()))?;
    io::copy(&mut response, &mut file)
        .with_context(|| format!("failed to write {}", destination.display()))?;
    Ok(())
}

fn extract_binary_from_archive(
    archive_path: &Path,
    tempdir: &TempDir,
    asset: &ReleaseAsset,
) -> Result<PathBuf> {
    let extracted_binary_path = tempdir.path().join(asset.binary_name);

    match asset.archive_format {
        ArchiveFormat::TarGz => {
            let archive_file = File::open(archive_path)
                .with_context(|| format!("failed to open {}", archive_path.display()))?;
            let decoder = GzDecoder::new(archive_file);
            let mut archive = Archive::new(decoder);

            for entry in archive.entries().context("failed to read tar archive")? {
                let mut entry = entry.context("failed to read tar archive entry")?;
                let entry_path = entry
                    .path()
                    .context("failed to read tar archive entry path")?;
                if entry_path
                    .file_name()
                    .and_then(|name| name.to_str())
                    == Some(asset.binary_name)
                {
                    entry
                        .unpack(&extracted_binary_path)
                        .with_context(|| {
                            format!(
                                "failed to unpack {} to {}",
                                asset.binary_name,
                                extracted_binary_path.display()
                            )
                        })?;
                    return Ok(extracted_binary_path);
                }
            }
        }
        #[cfg(target_os = "windows")]
        ArchiveFormat::Zip => {
            let archive_file = File::open(archive_path)
                .with_context(|| format!("failed to open {}", archive_path.display()))?;
            let mut archive =
                ZipArchive::new(archive_file).context("failed to read zip archive")?;

            for index in 0..archive.len() {
                let mut entry = archive
                    .by_index(index)
                    .context("failed to read zip archive entry")?;
                let entry_name = entry.name().replace('\\', "/");
                if entry_name.rsplit('/').next() == Some(asset.binary_name) {
                    let mut output = File::create(&extracted_binary_path).with_context(|| {
                        format!("failed to create {}", extracted_binary_path.display())
                    })?;
                    io::copy(&mut entry, &mut output).with_context(|| {
                        format!("failed to unpack {}", asset.binary_name)
                    })?;
                    return Ok(extracted_binary_path);
                }
            }
        }
    }

    Err(anyhow::anyhow!(
        "archive did not contain {}",
        asset.binary_name
    ))
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

fn normalize_tag(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.starts_with('v') {
        trimmed.to_string()
    } else {
        format!("v{trimmed}")
    }
}

fn http_client(timeout: Duration) -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .timeout(timeout)
        .build()
        .context("failed to build release HTTP client")
}

fn user_agent() -> String {
    format!("{}/{}", GITHUB_REPO, env!("CARGO_PKG_VERSION"))
}

impl ArchiveFormat {
    fn extension(self) -> &'static str {
        match self {
            ArchiveFormat::TarGz => "tar.gz",
            #[cfg(target_os = "windows")]
            ArchiveFormat::Zip => "zip",
        }
    }
}

fn state_path() -> Option<PathBuf> {
    dirs::state_dir()
        .or_else(dirs::data_local_dir)
        .map(|dir| dir.join("pw-env").join(RELEASE_CHECK_STATE_FILE))
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
        std::fs::write(path, &contents)
            .with_context(|| format!("failed to write release check state to {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o600);
            std::fs::set_permissions(path, perms).with_context(|| {
                format!("failed to set permissions on {}", path.display())
            })?;
        }
        Ok(())
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
    fn adds_v_prefix_to_requested_tags() {
        assert_eq!(normalize_tag("1.2.3"), "v1.2.3");
    }

    #[test]
    fn keeps_existing_v_prefix_on_requested_tags() {
        assert_eq!(normalize_tag("v1.2.3"), "v1.2.3");
    }

    #[test]
    fn rejects_invalid_release_tags() {
        assert!(normalize_version("latest").is_err());
    }

    #[test]
    fn release_archive_name_matches_installer_convention() {
        let asset = ReleaseAsset {
            target: "x86_64-apple-darwin",
            archive_format: ArchiveFormat::TarGz,
            binary_name: "pw-env",
        };

        assert_eq!(
            release_archive_name("v1.2.3", &asset),
            "pw-env-v1.2.3-x86_64-apple-darwin.tar.gz"
        );
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
    fn is_newer_release_returns_true_for_higher_patch() {
        let current = Version::parse(env!("CARGO_PKG_VERSION")).unwrap();
        let newer = Version::new(current.major, current.minor, current.patch + 1);
        assert!(is_newer_release(&newer.to_string()).unwrap());
    }

    #[test]
    fn is_newer_release_returns_false_for_current_version() {
        assert!(!is_newer_release(env!("CARGO_PKG_VERSION")).unwrap());
    }

    #[test]
    fn is_newer_release_returns_false_for_older_version() {
        let current = Version::parse(env!("CARGO_PKG_VERSION")).unwrap();
        // Build a version guaranteed to be older: major.minor.0 unless already patch 0.
        let older = if current.patch > 0 {
            Version::new(current.major, current.minor, current.patch - 1)
        } else if current.minor > 0 {
            Version::new(current.major, current.minor - 1, 0)
        } else {
            // Current is 0.0.0; no older version to construct — skip the assertion.
            return;
        };
        assert!(!is_newer_release(&older.to_string()).unwrap());
    }

    #[test]
    fn is_newer_release_returns_true_for_higher_major() {
        let current = Version::parse(env!("CARGO_PKG_VERSION")).unwrap();
        let newer = Version::new(current.major + 1, 0, 0);
        assert!(is_newer_release(&newer.to_string()).unwrap());
    }

    #[test]
    fn github_release_owner_constant_matches_repo_url() {
        assert!(RELEASE_API_URL.contains("/m42e/"));
    }

    #[test]
    fn release_download_url_has_expected_format() {
        let url =
            release_download_url("v1.2.3", "pw-env-v1.2.3-x86_64-apple-darwin.tar.gz");
        assert!(url.contains("github.com"));
        assert!(url.contains("m42e/pw-env"));
        assert!(url.contains("v1.2.3"));
        assert!(url.contains("pw-env-v1.2.3-x86_64-apple-darwin.tar.gz"));
    }

    #[test]
    fn is_newer_release_returns_false_for_same_version() {
        let current = env!("CARGO_PKG_VERSION");
        let result = is_newer_release(current).unwrap();
        assert!(!result);
    }

    #[test]
    fn is_newer_release_returns_true_for_higher_version() {
        let result = is_newer_release("999.0.0").unwrap();
        assert!(result);
    }

    #[test]
    fn is_newer_release_returns_false_for_lower_version() {
        // Current version is 0.2.10, so 0.0.1 should be lower
        let result = is_newer_release("0.0.1").unwrap();
        assert!(!result);
    }

    #[test]
    fn is_newer_release_rejects_invalid_version() {
        let result = is_newer_release("not-a-version");
        assert!(result.is_err());
    }

    #[test]
    fn archive_format_extension_tar_gz() {
        assert_eq!(ArchiveFormat::TarGz.extension(), "tar.gz");
    }

    #[test]
    fn release_check_state_is_due_with_zero_interval() {
        let state = ReleaseCheckState {
            last_checked_at: Some(1_000),
            last_notified_version: None,
        };
        // interval of 0 seconds means always due
        assert!(state.is_due(1_000, Duration::from_secs(0)));
    }

    #[test]
    fn release_check_state_load_returns_default_for_missing_path() {
        let path = PathBuf::from("/nonexistent/path/that/does/not/exist/release-check.json");
        let state = ReleaseCheckState::load(&path).unwrap();
        assert!(state.last_checked_at.is_none());
        assert!(state.last_notified_version.is_none());
    }

    #[test]
    fn release_check_state_save_and_load_round_trips() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("state.json");

        let state = ReleaseCheckState {
            last_checked_at: Some(12345),
            last_notified_version: Some("1.0.0".to_string()),
        };
        state.save(&path).unwrap();

        let loaded = ReleaseCheckState::load(&path).unwrap();
        assert_eq!(loaded.last_checked_at, Some(12345));
        assert_eq!(loaded.last_notified_version.as_deref(), Some("1.0.0"));
    }

    #[test]
    fn release_check_state_load_returns_error_for_invalid_json() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("bad-state.json");
        std::fs::write(&path, "not valid json { ").unwrap();

        let result = ReleaseCheckState::load(&path);
        assert!(result.is_err());
    }

    #[test]
    fn now_unix_timestamp_is_reasonable() {
        let ts = now_unix_timestamp();
        // After 2024-01-01
        assert!(ts > 1_700_000_000);
        // Before 2100-01-01
        assert!(ts < 4_100_000_000);
    }

    #[test]
    fn resolve_release_with_explicit_version_no_network() {
        // Passing an explicit version should not require network access
        let result = resolve_release(Some("1.2.3"));
        let resolved = result.unwrap();
        assert_eq!(resolved.version, "1.2.3");
        assert_eq!(resolved.tag, "v1.2.3");
    }

    #[test]
    fn detect_release_asset_returns_asset_for_current_platform() {
        let result = detect_release_asset();
        assert!(result.is_ok(), "platform should be supported");
        let asset = result.unwrap();
        assert!(!asset.target.is_empty());
        assert!(!asset.binary_name.is_empty());
    }

    #[test]
    fn maybe_check_for_update_returns_ok_immediately_when_updates_disabled() {
        let config = crate::config::Config {
            defaults: crate::config::Defaults::default(),
            log: crate::config::LogConfig::default(),
            updates: crate::config::UpdateConfig {
                enabled: false,
                check_interval_hours: 24,
            },
            projects: vec![],
        };
        let result = maybe_check_for_update(&config);
        assert!(result.is_ok());
    }

    #[test]
    fn maybe_check_for_update_returns_ok_when_state_is_not_due() {
        // Write a "just checked" state to the actual state path so is_due() returns false
        // and no network call is made.
        let Some(state_path) = state_path() else {
            return; // platform has no state dir; skip
        };

        let original = if state_path.exists() {
            std::fs::read_to_string(&state_path).ok()
        } else {
            None
        };

        let fresh = ReleaseCheckState {
            last_checked_at: Some(now_unix_timestamp()),
            last_notified_version: None,
        };
        if fresh.save(&state_path).is_err() {
            return; // can't write state; skip
        }

        let config = crate::config::Config {
            defaults: crate::config::Defaults::default(),
            log: crate::config::LogConfig::default(),
            updates: crate::config::UpdateConfig {
                enabled: true,
                check_interval_hours: 24,
            },
            projects: vec![],
        };
        let result = maybe_check_for_update(&config);

        // Restore the original state
        match original {
            Some(content) => { let _ = std::fs::write(&state_path, content); }
            None => { let _ = std::fs::remove_file(&state_path); }
        }

        assert!(result.is_ok());
    }

    #[test]
    fn http_client_builds_successfully_with_short_timeout() {
        let result = http_client(Duration::from_secs(1));
        assert!(result.is_ok());
    }

    #[test]
    fn state_path_does_not_panic() {
        let _path = state_path(); // May be Some or None depending on platform; must not panic
    }

    #[test]
    fn release_check_state_is_not_due_when_checked_just_now() {
        let now = now_unix_timestamp();
        let state = ReleaseCheckState {
            last_checked_at: Some(now),
            last_notified_version: None,
        };
        let interval = Duration::from_secs(3600);
        assert!(!state.is_due(now, interval));
    }

    #[test]
    fn release_check_state_save_creates_parent_directory() {
        let temp_dir = TempDir::new().unwrap();
        let nested = temp_dir.path().join("a/b/c/state.json");
        let state = ReleaseCheckState {
            last_checked_at: Some(99),
            last_notified_version: Some("0.1.0".to_string()),
        };
        state.save(&nested).unwrap();
        assert!(nested.exists());
    }

    #[test]
    fn normalize_tag_handles_whitespace() {
        assert_eq!(normalize_tag("  1.2.3  "), "v1.2.3");
        assert_eq!(normalize_tag("  v1.2.3  "), "v1.2.3");
    }

    #[test]
    fn resolve_release_with_explicit_v_prefix() {
        let result = resolve_release(Some("v1.5.0")).unwrap();
        assert_eq!(result.version, "1.5.0");
        assert_eq!(result.tag, "v1.5.0");
    }

    #[test]
    fn release_archive_name_uses_extension_from_asset() {
        let asset = ReleaseAsset {
            target: "x86_64-unknown-linux-gnu",
            archive_format: ArchiveFormat::TarGz,
            binary_name: "pw-env",
        };
        let name = release_archive_name("v0.2.0", &asset);
        assert!(name.ends_with(".tar.gz"));
        assert!(name.contains("x86_64-unknown-linux-gnu"));
    }
}
