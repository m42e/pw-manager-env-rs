use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use tracing::debug;

const PROJECT_OVERRIDE_FILE_NAME: &str = ".pw-env.toml";

fn base_dir_from_env(
    env_value: Option<PathBuf>,
    home_dir: Option<PathBuf>,
    fallback_components: &[&str],
) -> PathBuf {
    let has_env_override = env_value.is_some();
    let mut path = env_value.unwrap_or_else(|| home_dir.unwrap_or_else(|| PathBuf::from("~")));
    if !has_env_override {
        for component in fallback_components {
            path.push(component);
        }
    }

    path
}

pub fn config_dir() -> PathBuf {
    base_dir_from_env(
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
        dirs::home_dir(),
        &[".config"],
    )
}

pub fn state_dir() -> PathBuf {
    base_dir_from_env(
        std::env::var_os("XDG_STATE_HOME").map(PathBuf::from),
        dirs::home_dir(),
        &[".local", "state"],
    )
}

/// Write a file with owner-only permissions (0o600) on Unix.
/// The file is created with restricted permissions from the start so that
/// sensitive state is never briefly world-readable.
pub(crate) fn write_private_file(path: &Path, contents: &str) -> Result<()> {
    #[cfg(unix)]
    {
        use std::fs::OpenOptions;
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("Failed to create {}", path.display()))?;
        file.write_all(contents.as_bytes())
            .with_context(|| format!("Failed to write {}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, contents)
            .with_context(|| format!("Failed to write {}", path.display()))?;
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct ApprovedProjectConfigEntry {
    pub path: PathBuf,
    pub hash: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretFetchApprovalMode {
    CurrentEnvHash,
    ProjectWide,
}

#[derive(Debug, Clone)]
pub struct ApprovedSecretFetchEntry {
    pub project_path: PathBuf,
    pub env_hash: Option<String>,
    pub project_wide: bool,
}

#[derive(Debug, Clone)]
pub struct ProjectOverrideApprovalStatus {
    pub override_path: PathBuf,
    pub approved_hash: Option<String>,
    pub current_hash: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SecretFetchApprovalStatus {
    pub project_path: PathBuf,
    pub env_path: PathBuf,
    pub current_env_hash: Option<String>,
    pub approved_env_hashes: BTreeSet<String>,
    pub project_wide: bool,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Config {
    #[serde(default)]
    pub defaults: Defaults,
    #[serde(default)]
    pub log: LogConfig,
    #[serde(default)]
    pub updates: UpdateConfig,
    #[serde(default)]
    pub projects: Vec<ProjectOverride>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct UpdateConfig {
    #[serde(default = "default_updates_enabled")]
    pub enabled: bool,
    #[serde(default = "default_update_check_interval_hours")]
    pub check_interval_hours: u64,
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self {
            enabled: default_updates_enabled(),
            check_interval_hours: default_update_check_interval_hours(),
        }
    }
}

fn default_updates_enabled() -> bool {
    true
}

fn default_update_check_interval_hours() -> u64 {
    24
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Defaults {
    #[serde(default = "default_backend")]
    pub backend: String,
    #[serde(default = "default_search_parent_env")]
    pub search_parent_env: bool,
    /// When true, plaintext (non-secret) values from `.env` are also exported
    /// alongside the resolved secret values.
    #[serde(default)]
    pub source_all: bool,
    /// When true, a warning is printed for each .env entry that could not be resolved.
    #[serde(default)]
    pub warn_missing: bool,
    #[serde(default)]
    pub cache: CacheConfig,
    #[serde(default)]
    pub op: OpConfig,
    #[serde(default)]
    pub bw: BwConfig,
    #[serde(default)]
    pub gpg: GpgConfig,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            backend: default_backend(),
            search_parent_env: default_search_parent_env(),
            source_all: false,
            warn_missing: false,
            cache: CacheConfig::default(),
            op: OpConfig::default(),
            bw: BwConfig::default(),
            gpg: GpgConfig::default(),
        }
    }
}

fn default_backend() -> String {
    "op".to_string()
}

fn default_search_parent_env() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CacheConfig {
    #[serde(default = "default_cache_enabled")]
    pub enabled: bool,
    #[serde(default = "default_cache_ttl_hours")]
    pub ttl_hours: u64,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            enabled: default_cache_enabled(),
            ttl_hours: default_cache_ttl_hours(),
        }
    }
}

fn default_cache_enabled() -> bool {
    true
}

fn default_cache_ttl_hours() -> u64 {
    4
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct OpConfig {
    #[serde(default)]
    pub vault: Option<String>,
    #[serde(default)]
    pub account: Option<String>,
    /// Item name to look up keys as fields (if set, keys are resolved as fields of this item)
    #[serde(default)]
    pub item: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BwConfig {
    #[serde(default)]
    pub folder: Option<String>,
    #[serde(default)]
    pub organization: Option<String>,
    /// Item name to look up keys as fields (if set, keys are resolved as custom fields of this item)
    #[serde(default)]
    pub item: Option<String>,
    #[serde(default = "default_bw_sync_throttle_secs")]
    pub sync_throttle_secs: u64,
}

fn default_bw_sync_throttle_secs() -> u64 {
    3600
}

impl Default for BwConfig {
    fn default() -> Self {
        Self {
            folder: None,
            organization: None,
            item: None,
            sync_throttle_secs: default_bw_sync_throttle_secs(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GpgConfig {
    #[serde(default = "default_gpg_file_pattern")]
    pub file_pattern: String,
    #[serde(default)]
    pub recipient: Option<String>,
}

impl Default for GpgConfig {
    fn default() -> Self {
        Self {
            file_pattern: default_gpg_file_pattern(),
            recipient: None,
        }
    }
}

fn default_gpg_file_pattern() -> String {
    ".env.gpg".to_string()
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LogConfig {
    #[serde(default = "default_log_level")]
    pub level: String,
    #[serde(default = "default_log_file")]
    pub file: Option<String>,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
            file: default_log_file(),
        }
    }
}

fn default_log_level() -> String {
    "info".to_string()
}

fn default_log_file() -> Option<String> {
    Some(
        state_dir()
            .join("pw-env")
            .join("pw-env.log")
            .to_string_lossy()
            .into_owned(),
    )
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ProjectOverride {
    pub path: String,
    #[serde(default)]
    pub backend: Option<String>,
    #[serde(default)]
    pub search_parent_env: Option<bool>,
    #[serde(default)]
    pub source_all: Option<bool>,
    #[serde(default)]
    pub warn_missing: Option<bool>,
    #[serde(default)]
    pub cache: Option<CacheConfig>,
    #[serde(default)]
    pub op: Option<OpConfig>,
    #[serde(default)]
    pub bw: Option<BwConfig>,
    #[serde(default)]
    pub gpg: Option<GpgConfig>,
    /// Specific item name in the password store for this project
    #[serde(default)]
    pub item: Option<String>,
    #[serde(default)]
    pub commands: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
struct ProjectDirectoryOverride {
    #[serde(default)]
    pub backend: Option<String>,
    #[serde(default)]
    pub search_parent_env: Option<bool>,
    #[serde(default)]
    pub source_all: Option<bool>,
    #[serde(default)]
    pub warn_missing: Option<bool>,
    #[serde(default)]
    pub cache: Option<CacheConfig>,
    #[serde(default)]
    pub op: Option<OpConfig>,
    #[serde(default)]
    pub bw: Option<BwConfig>,
    #[serde(default)]
    pub gpg: Option<GpgConfig>,
    #[serde(default)]
    pub item: Option<String>,
    #[serde(default)]
    pub commands: Vec<String>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct ApprovedProjectConfigs {
    #[serde(default)]
    approved_hashes: BTreeMap<String, String>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct ApprovedSecretFetches {
    #[serde(default)]
    approved_env_hashes: BTreeMap<String, BTreeSet<String>>,
    #[serde(default)]
    project_wide: BTreeSet<String>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct ReviewedMigrations {
    #[serde(default)]
    reviewed_entry_fingerprints: BTreeMap<String, BTreeSet<String>>,
}

impl Config {
    pub fn config_path() -> PathBuf {
        config_dir().join("pw-env").join("config.toml")
    }

    pub fn load() -> Result<Self> {
        let path = Self::config_path();
        if path.exists() {
            debug!("Loading config from {}", path.display());
            let contents = std::fs::read_to_string(&path)
                .with_context(|| format!("Failed to read config from {}", path.display()))?;
            let config: Config = toml::from_str(&contents)
                .with_context(|| format!("Failed to parse config from {}", path.display()))?;
            Ok(config)
        } else {
            debug!("No config found at {}, using defaults", path.display());
            Ok(Config {
                defaults: Defaults::default(),
                log: LogConfig::default(),
                updates: UpdateConfig::default(),
                projects: vec![],
            })
        }
    }

    pub fn load_for_dir(dir: &Path) -> Result<Self> {
        let mut config = Self::load()?;

        if let Some((project_dir, override_path)) = Self::project_override_file(dir)
            && let Some(local_override) =
                ProjectDirectoryOverride::load_if_approved(&override_path)?
        {
            config.projects.push(ProjectOverride {
                path: project_dir.to_string_lossy().into_owned(),
                backend: local_override.backend,
                search_parent_env: local_override.search_parent_env,
                source_all: local_override.source_all,
                warn_missing: local_override.warn_missing,
                cache: local_override.cache,
                op: local_override.op,
                bw: local_override.bw,
                gpg: local_override.gpg,
                item: local_override.item,
                commands: local_override.commands,
            });
        }

        Ok(config)
    }

    pub fn project_override_path(dir: &Path) -> Option<PathBuf> {
        Self::project_override_file(dir).map(|(_, path)| path)
    }

    pub fn approval_store_path() -> Option<PathBuf> {
        ApprovedProjectConfigs::path()
    }

    pub fn secret_fetch_approval_store_path() -> Option<PathBuf> {
        ApprovedSecretFetches::path()
    }

    pub fn approved_project_configs() -> Result<Vec<ApprovedProjectConfigEntry>> {
        Ok(ApprovedProjectConfigs::load()?.entries())
    }

    pub fn approved_secret_fetches() -> Result<Vec<ApprovedSecretFetchEntry>> {
        Ok(ApprovedSecretFetches::load()?.entries())
    }

    pub fn project_override_approval_status(path: &Path) -> Result<ProjectOverrideApprovalStatus> {
        let override_path = resolve_project_override_target(path)?;
        let approvals = ApprovedProjectConfigs::load()?;
        let current_hash = if override_path.exists() {
            Some(hash_file(&override_path)?)
        } else {
            None
        };

        Ok(ProjectOverrideApprovalStatus {
            approved_hash: approvals
                .approved_hash(&override_path)
                .map(ToOwned::to_owned),
            override_path,
            current_hash,
        })
    }

    pub fn approve_project_override(path: &Path) -> Result<ApprovedProjectConfigEntry> {
        let override_path = resolve_project_override_target(path)?;
        validate_project_override(&override_path)?;
        let hash = hash_file(&override_path)?;

        let mut approvals = ApprovedProjectConfigs::load()?;
        approvals.approve(&override_path, hash.clone());
        approvals.save()?;

        Ok(ApprovedProjectConfigEntry {
            path: override_path,
            hash,
        })
    }

    pub fn revoke_project_override_approval(path: &Path) -> Result<bool> {
        let override_path = resolve_project_override_target(path)?;
        let mut approvals = ApprovedProjectConfigs::load()?;
        let removed = approvals.revoke(&override_path);
        if removed {
            approvals.save()?;
        }
        Ok(removed)
    }

    pub fn secret_fetch_approval_status(path: &Path) -> Result<SecretFetchApprovalStatus> {
        let (project_path, env_path) = resolve_secret_fetch_target(path)?;
        let approvals = ApprovedSecretFetches::load()?;
        let current_env_hash = if env_path.exists() {
            Some(hash_file(&env_path)?)
        } else {
            None
        };

        Ok(SecretFetchApprovalStatus {
            approved_env_hashes: approvals.approved_hashes(&project_path),
            current_env_hash,
            env_path,
            project_path: project_path.clone(),
            project_wide: approvals.is_project_wide(&project_path),
        })
    }

    pub fn approve_secret_fetch(
        path: &Path,
        mode: SecretFetchApprovalMode,
    ) -> Result<ApprovedSecretFetchEntry> {
        let (project_path, env_path) = resolve_secret_fetch_target(path)?;
        let mut approvals = ApprovedSecretFetches::load()?;

        match mode {
            SecretFetchApprovalMode::CurrentEnvHash => {
                let env_hash = hash_file(&env_path)?;
                approvals.approve_hash(&project_path, env_hash.clone());
                approvals.save()?;
                Ok(ApprovedSecretFetchEntry {
                    project_path,
                    env_hash: Some(env_hash),
                    project_wide: false,
                })
            }
            SecretFetchApprovalMode::ProjectWide => {
                approvals.allow_project_wide(&project_path);
                approvals.save()?;
                Ok(ApprovedSecretFetchEntry {
                    project_path,
                    env_hash: None,
                    project_wide: true,
                })
            }
        }
    }

    pub fn revoke_secret_fetch_approval(path: &Path) -> Result<bool> {
        let (project_path, _) = resolve_secret_fetch_target(path)?;
        let mut approvals = ApprovedSecretFetches::load()?;
        let removed = approvals.revoke(&project_path);
        if removed {
            approvals.save()?;
        }
        Ok(removed)
    }

    pub fn ensure_secret_fetch_approved(env_path: &Path) -> Result<()> {
        let (project_path, env_path) = resolve_secret_fetch_target(env_path)?;
        let env_hash = hash_file(&env_path)?;
        let mut approvals = ApprovedSecretFetches::load()?;

        if approvals.is_approved(&project_path, &env_hash) {
            debug!(
                "Credential fetch already approved for project {}",
                project_path.display()
            );
            return Ok(());
        }

        let previously_approved_hashes = approvals.approved_hashes(&project_path);
        if !stdin_is_terminal() {
            let state = if previously_approved_hashes.is_empty() {
                "has not been approved yet"
            } else {
                "changed since its last approved .env contents"
            };
            anyhow::bail!(
                "pw-env: credential fetching for project {} with {} {}. Re-run in an interactive session to approve this .env hash or allow the whole project.",
                project_path.display(),
                env_path.display(),
                state,
            );
        }

        eprintln!(
            "Credential fetch approval required for project {}",
            project_path.display()
        );
        eprintln!(".env file: {}", env_path.display());
        if previously_approved_hashes.is_empty() {
            eprintln!(
                "This .env file can trigger secret lookups from your configured password backend."
            );
        } else {
            eprintln!("This .env file changed since the last approved version.");
        }
        eprintln!(
            "Approve the current .env hash only, or allow any future .env changes in this project."
        );
        eprint!("Approve secret fetching? [y] current hash / [a]ll project changes / [N] no ");
        io::stderr().flush()?;

        let mut input = String::new();
        read_stdin_line(&mut input)?;
        let answer = input.trim().to_ascii_lowercase();

        match answer.as_str() {
            "y" | "yes" => {
                approvals.approve_hash(&project_path, env_hash);
                approvals.save()?;
                Ok(())
            }
            "a" | "all" | "always" => {
                approvals.allow_project_wide(&project_path);
                approvals.save()?;
                Ok(())
            }
            _ => anyhow::bail!(
                "Credential fetching was not approved for project {}",
                project_path.display()
            ),
        }
    }

    pub fn reviewed_migration_entry_fingerprints(env_path: &Path) -> Result<BTreeSet<String>> {
        Ok(ReviewedMigrations::load()?.fingerprints(env_path))
    }

    pub fn remember_reviewed_migration_entries(
        env_path: &Path,
        fingerprints: impl IntoIterator<Item = String>,
    ) -> Result<()> {
        let mut reviewed = ReviewedMigrations::load()?;
        reviewed.remember(env_path, fingerprints);
        reviewed.save()
    }

    pub fn forget_reviewed_migration_entries(
        env_path: &Path,
        fingerprints: impl IntoIterator<Item = String>,
    ) -> Result<()> {
        let mut reviewed = ReviewedMigrations::load()?;
        reviewed.forget(env_path, fingerprints);
        reviewed.save()
    }

    /// Find a project override matching the given directory (exact match or parent match).
    pub fn project_for(&self, dir: &Path) -> Option<&ProjectOverride> {
        self.project_index_for(dir)
            .map(|index| &self.projects[index])
    }

    pub fn with_backend_override_for_dir(&self, dir: &Path, backend: Option<&str>) -> Self {
        let Some(backend) = backend else {
            return self.clone();
        };

        let mut config = self.clone();
        if let Some(index) = config.project_index_for(dir) {
            config.projects[index].backend = Some(backend.to_string());
        } else {
            config.defaults.backend = backend.to_string();
        }

        config
    }

    /// Resolve the effective backend name for a given directory.
    pub fn effective_backend(&self, dir: &Path) -> &str {
        self.project_for(dir)
            .and_then(|p| p.backend.as_deref())
            .unwrap_or(&self.defaults.backend)
    }

    /// Resolve effective 1Password config for a given directory.
    pub fn effective_op(&self, dir: &Path) -> &OpConfig {
        self.project_for(dir)
            .and_then(|p| p.op.as_ref())
            .unwrap_or(&self.defaults.op)
    }

    /// Resolve effective cache config for a given directory.
    pub fn effective_cache(&self, dir: &Path) -> &CacheConfig {
        self.project_for(dir)
            .and_then(|p| p.cache.as_ref())
            .unwrap_or(&self.defaults.cache)
    }

    /// Resolve effective Bitwarden config for a given directory.
    pub fn effective_bw(&self, dir: &Path) -> &BwConfig {
        self.project_for(dir)
            .and_then(|p| p.bw.as_ref())
            .unwrap_or(&self.defaults.bw)
    }

    /// Resolve effective GPG config for a given directory.
    pub fn effective_gpg(&self, dir: &Path) -> &GpgConfig {
        self.project_for(dir)
            .and_then(|p| p.gpg.as_ref())
            .unwrap_or(&self.defaults.gpg)
    }

    /// Resolve effective item name for a given directory.
    pub fn effective_item(&self, dir: &Path) -> Option<&str> {
        if let Some(proj) = self.project_for(dir)
            && let Some(ref item) = proj.item
        {
            return Some(item.as_str());
        }
        // Check the backend-specific default item
        match self.effective_backend(dir) {
            "op" => self.effective_op(dir).item.as_deref(),
            "bw" => self.effective_bw(dir).item.as_deref(),
            _ => None,
        }
    }

    /// Resolve configured transient command wrappers for a given directory.
    pub fn effective_commands(&self, dir: &Path) -> &[String] {
        self.project_for(dir)
            .map(|project| project.commands.as_slice())
            .unwrap_or(&[])
    }

    pub fn effective_search_parent_env(&self, dir: &Path) -> bool {
        self.project_for(dir)
            .and_then(|project| project.search_parent_env)
            .unwrap_or(self.defaults.search_parent_env)
    }

    /// Resolve whether plaintext values should also be exported for a given directory.
    pub fn effective_source_all(&self, dir: &Path) -> bool {
        self.project_for(dir)
            .and_then(|project| project.source_all)
            .unwrap_or(self.defaults.source_all)
    }

    /// Resolve whether a warning should be emitted for unresolved .env entries for a given directory.
    pub fn effective_warn_missing(&self, dir: &Path) -> bool {
        self.project_for(dir)
            .and_then(|project| project.warn_missing)
            .unwrap_or(self.defaults.warn_missing)
    }

    fn project_index_for(&self, dir: &Path) -> Option<usize> {
        self.projects
            .iter()
            .enumerate()
            .filter_map(|(index, project)| {
                let project_path = normalized_project_path(&project.path);
                if dir.starts_with(&project_path) {
                    Some((project_path.components().count(), index))
                } else {
                    None
                }
            })
            .max_by_key(|(depth, _)| *depth)
            .map(|(_, index)| index)
    }
}

impl Config {
    fn project_override_file(dir: &Path) -> Option<(PathBuf, PathBuf)> {
        let git_root = find_git_root(dir);
        let mut current = dir.to_path_buf();

        loop {
            let candidate = current.join(PROJECT_OVERRIDE_FILE_NAME);
            if candidate.exists() {
                return Some((current, candidate));
            }

            if git_root.as_ref().is_none_or(|root| *root == current) {
                break;
            }

            if !current.pop() {
                break;
            }
        }

        None
    }
}

impl ProjectDirectoryOverride {
    fn load_if_approved(path: &Path) -> Result<Option<Self>> {
        if path.is_symlink() {
            eprintln!(
                "pw-env: refusing to follow {} symlink at {}. Use a regular file.",
                PROJECT_OVERRIDE_FILE_NAME,
                path.display()
            );
            return Ok(None);
        }
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read project override from {}", path.display()))?;
        let local_override: ProjectDirectoryOverride = toml::from_str(&contents)
            .with_context(|| format!("Failed to parse project override from {}", path.display()))?;
        let hash = sha256_hex(&contents);

        let mut approvals = ApprovedProjectConfigs::load()?;
        let previously_approved = approvals.approved_hash(path);

        if previously_approved == Some(hash.as_str()) {
            debug!("Loading approved project override from {}", path.display());
            return Ok(Some(local_override));
        }

        if !stdin_is_terminal() {
            let state = if previously_approved.is_some() {
                "changed since its last approval"
            } else {
                "has not been approved yet"
            };
            eprintln!(
                "pw-env: project override {} {}. Skipping it until you approve the current file contents in an interactive session.",
                path.display(),
                state
            );
            return Ok(None);
        }

        eprintln!("Project override found: {}", path.display());
        if previously_approved.is_some() {
            eprintln!("This file changed since the last approved version.");
        } else {
            eprintln!("This file can override pw-env settings for the current project.");
        }
        eprint!("Approve loading this file? [y/N] ");
        io::stderr().flush()?;

        let mut input = String::new();
        read_stdin_line(&mut input)?;
        let answer = input.trim().to_ascii_lowercase();
        if answer != "y" && answer != "yes" {
            eprintln!("Skipping project override: {}", path.display());
            return Ok(None);
        }

        approvals.approve(path, hash);
        approvals.save()?;
        Ok(Some(local_override))
    }
}

impl ApprovedProjectConfigs {
    fn load() -> Result<Self> {
        let Some(path) = Self::path() else {
            return Ok(Self::default());
        };

        Self::load_from_path(&path)
    }

    fn load_from_path(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }

        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read approval store from {}", path.display()))?;
        serde_json::from_str(&contents)
            .with_context(|| format!("Failed to parse approval store from {}", path.display()))
    }

    fn save(&self) -> Result<()> {
        let Some(path) = Self::path() else {
            return Ok(());
        };

        self.save_to_path(&path)
    }

    fn save_to_path(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create state directory {}", parent.display())
            })?;
        }

        let contents =
            serde_json::to_string_pretty(self).context("Failed to serialize approval store")?;
        write_private_file(path, &contents)
            .with_context(|| format!("Failed to write approval store to {}", path.display()))
    }

    fn approve(&mut self, path: &Path, hash: String) {
        self.approved_hashes.insert(normalize_path(path), hash);
    }

    fn approved_hash(&self, path: &Path) -> Option<&str> {
        self.approved_hashes
            .get(&normalize_path(path))
            .map(String::as_str)
    }

    fn revoke(&mut self, path: &Path) -> bool {
        self.approved_hashes.remove(&normalize_path(path)).is_some()
    }

    fn entries(&self) -> Vec<ApprovedProjectConfigEntry> {
        self.approved_hashes
            .iter()
            .map(|(path, hash)| ApprovedProjectConfigEntry {
                path: PathBuf::from(path),
                hash: hash.clone(),
            })
            .collect()
    }

    fn path() -> Option<PathBuf> {
        #[cfg(test)]
        if let Some(p) = TEST_APPROVAL_STORE_PATH.with(|v| v.borrow().clone()) {
            return Some(p);
        }
        Some(
            state_dir()
                .join("pw-env")
                .join("approved-project-configs.json"),
        )
    }
}

impl ApprovedSecretFetches {
    fn load() -> Result<Self> {
        let Some(path) = Self::path() else {
            return Ok(Self::default());
        };

        Self::load_from_path(&path)
    }

    fn load_from_path(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }

        let contents = std::fs::read_to_string(path).with_context(|| {
            format!(
                "Failed to read secret fetch approval store from {}",
                path.display()
            )
        })?;
        serde_json::from_str(&contents).with_context(|| {
            format!(
                "Failed to parse secret fetch approval store from {}",
                path.display()
            )
        })
    }

    fn save(&self) -> Result<()> {
        let Some(path) = Self::path() else {
            return Ok(());
        };

        self.save_to_path(&path)
    }

    fn save_to_path(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create state directory {}", parent.display())
            })?;
        }

        let contents = serde_json::to_string_pretty(self)
            .context("Failed to serialize secret fetch approval store")?;
        write_private_file(path, &contents).with_context(|| {
            format!(
                "Failed to write secret fetch approval store to {}",
                path.display()
            )
        })
    }

    fn approve_hash(&mut self, project_path: &Path, env_hash: String) {
        let hashes = self
            .approved_env_hashes
            .entry(normalize_path(project_path))
            .or_default();
        hashes.insert(env_hash);
        // Limit stored hashes per project to prevent unbounded growth.
        // Eviction order is lexicographic (BTreeSet), not chronological, but
        // the important invariant is that the set stays bounded and the
        // just-inserted hash is retained.
        const MAX_HASHES_PER_PROJECT: usize = 10;
        if hashes.len() > MAX_HASHES_PER_PROJECT {
            let to_remove: Vec<String> = hashes
                .iter()
                .take(hashes.len() - MAX_HASHES_PER_PROJECT)
                .cloned()
                .collect();
            for key in to_remove {
                hashes.remove(&key);
            }
        }
    }

    fn allow_project_wide(&mut self, project_path: &Path) {
        self.project_wide.insert(normalize_path(project_path));
    }

    fn approved_hashes(&self, project_path: &Path) -> BTreeSet<String> {
        self.approved_env_hashes
            .get(&normalize_path(project_path))
            .cloned()
            .unwrap_or_default()
    }

    fn is_project_wide(&self, project_path: &Path) -> bool {
        self.project_wide.contains(&normalize_path(project_path))
    }

    fn is_approved(&self, project_path: &Path, env_hash: &str) -> bool {
        self.is_project_wide(project_path)
            || self
                .approved_env_hashes
                .get(&normalize_path(project_path))
                .is_some_and(|hashes| hashes.contains(env_hash))
    }

    fn revoke(&mut self, project_path: &Path) -> bool {
        let normalized = normalize_path(project_path);
        let removed_hashes = self.approved_env_hashes.remove(&normalized).is_some();
        let removed_project = self.project_wide.remove(&normalized);
        removed_hashes || removed_project
    }

    fn entries(&self) -> Vec<ApprovedSecretFetchEntry> {
        let mut entries = Vec::new();

        for project_path in &self.project_wide {
            entries.push(ApprovedSecretFetchEntry {
                project_path: PathBuf::from(project_path),
                env_hash: None,
                project_wide: true,
            });
        }

        for (project_path, hashes) in &self.approved_env_hashes {
            for hash in hashes {
                entries.push(ApprovedSecretFetchEntry {
                    project_path: PathBuf::from(project_path),
                    env_hash: Some(hash.clone()),
                    project_wide: false,
                });
            }
        }

        entries
    }

    fn path() -> Option<PathBuf> {
        #[cfg(test)]
        if let Some(p) = TEST_SECRET_FETCH_STORE_PATH.with(|v| v.borrow().clone()) {
            return Some(p);
        }
        Some(
            state_dir()
                .join("pw-env")
                .join("approved-secret-fetches.json"),
        )
    }
}

impl ReviewedMigrations {
    fn load() -> Result<Self> {
        let Some(path) = Self::path() else {
            return Ok(Self::default());
        };

        Self::load_from_path(&path)
    }

    fn load_from_path(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }

        let contents = std::fs::read_to_string(path).with_context(|| {
            format!(
                "Failed to read reviewed migration store from {}",
                path.display()
            )
        })?;
        serde_json::from_str(&contents).with_context(|| {
            format!(
                "Failed to parse reviewed migration store from {}",
                path.display()
            )
        })
    }

    fn save(&self) -> Result<()> {
        let Some(path) = Self::path() else {
            return Ok(());
        };

        self.save_to_path(&path)
    }

    fn save_to_path(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create state directory {}", parent.display())
            })?;
        }

        let contents = serde_json::to_string_pretty(self)
            .context("Failed to serialize reviewed migration store")?;
        write_private_file(path, &contents).with_context(|| {
            format!(
                "Failed to write reviewed migration store to {}",
                path.display()
            )
        })
    }

    fn remember(&mut self, env_path: &Path, fingerprints: impl IntoIterator<Item = String>) {
        let entry = self
            .reviewed_entry_fingerprints
            .entry(normalize_path(env_path))
            .or_default();
        entry.extend(fingerprints);
    }

    fn forget(&mut self, env_path: &Path, fingerprints: impl IntoIterator<Item = String>) {
        let normalized_path = normalize_path(env_path);
        let Some(entry) = self.reviewed_entry_fingerprints.get_mut(&normalized_path) else {
            return;
        };

        for fingerprint in fingerprints {
            entry.remove(&fingerprint);
        }

        if entry.is_empty() {
            self.reviewed_entry_fingerprints.remove(&normalized_path);
        }
    }

    fn fingerprints(&self, env_path: &Path) -> BTreeSet<String> {
        self.reviewed_entry_fingerprints
            .get(&normalize_path(env_path))
            .cloned()
            .unwrap_or_default()
    }

    fn path() -> Option<PathBuf> {
        #[cfg(test)]
        if let Some(p) = TEST_REVIEWED_MIGRATIONS_PATH.with(|v| v.borrow().clone()) {
            return Some(p);
        }
        Some(state_dir().join("pw-env").join("reviewed-migrations.json"))
    }
}

fn expand_path(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    PathBuf::from(path)
}

fn normalized_project_path(path: &str) -> PathBuf {
    let expanded = expand_path(path);
    expanded.canonicalize().unwrap_or(expanded)
}

fn normalize_path(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

fn resolve_project_override_target(path: &Path) -> Result<PathBuf> {
    let candidate = if path.is_dir() {
        Config::project_override_path(path).ok_or_else(|| {
            anyhow::anyhow!(
                "No {} file found for {}",
                PROJECT_OVERRIDE_FILE_NAME,
                path.display()
            )
        })?
    } else {
        path.to_path_buf()
    };

    if candidate.file_name().and_then(|name| name.to_str()) != Some(PROJECT_OVERRIDE_FILE_NAME) {
        anyhow::bail!(
            "Expected a {} file or a directory containing one: {}",
            PROJECT_OVERRIDE_FILE_NAME,
            path.display()
        );
    }

    if !candidate.exists() {
        anyhow::bail!("Project override file not found: {}", candidate.display());
    }

    candidate
        .canonicalize()
        .with_context(|| format!("Failed to resolve {}", candidate.display()))
}

fn stdin_is_terminal() -> bool {
    #[cfg(test)]
    {
        return MOCK_IS_TERMINAL.with(|v| v.get());
    }
    #[allow(unreachable_code)]
    std::io::stdin().is_terminal()
}

fn read_stdin_line(buf: &mut String) -> io::Result<usize> {
    #[cfg(test)]
    if let Some(line) = MOCK_STDIN_LINE.with(|m| m.borrow_mut().take()) {
        *buf = line;
        return Ok(buf.len());
    }
    io::stdin().read_line(buf)
}

#[cfg(test)]
use std::cell::{Cell, RefCell};

#[cfg(test)]
thread_local! {
    static MOCK_IS_TERMINAL: Cell<bool> = const { Cell::new(false) };
    static MOCK_STDIN_LINE: RefCell<Option<String>> = const { RefCell::new(None) };
    /// Per-test override for the `ApprovedProjectConfigs` store path.
    static TEST_APPROVAL_STORE_PATH: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
    /// Per-test override for the `ApprovedSecretFetches` store path.
    static TEST_SECRET_FETCH_STORE_PATH: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
    /// Per-test override for the `ReviewedMigrations` store path.
    static TEST_REVIEWED_MIGRATIONS_PATH: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

fn resolve_secret_fetch_target(path: &Path) -> Result<(PathBuf, PathBuf)> {
    let env_path = if path.is_dir() {
        let config = Config::load_for_dir(path)?;
        crate::env_file::EnvFile::find_with_parents(path, config.effective_search_parent_env(path))
            .ok_or_else(|| anyhow::anyhow!(".env file not found: {}", path.display()))?
    } else {
        path.to_path_buf()
    };

    if env_path.file_name().and_then(|name| name.to_str()) != Some(".env") {
        anyhow::bail!(
            "Expected a .env file or a directory containing one: {}",
            path.display()
        );
    }

    if !env_path.exists() {
        anyhow::bail!(".env file not found: {}", env_path.display());
    }

    let env_path = env_path
        .canonicalize()
        .with_context(|| format!("Failed to resolve {}", env_path.display()))?;
    let project_dir = env_path
        .parent()
        .and_then(find_git_root)
        .or_else(|| env_path.parent().map(Path::to_path_buf))
        .ok_or_else(|| anyhow::anyhow!("Failed to determine project for {}", env_path.display()))?;
    let project_path = project_dir
        .canonicalize()
        .with_context(|| format!("Failed to resolve {}", project_dir.display()))?;

    Ok((project_path, env_path))
}

fn hash_file(path: &Path) -> Result<String> {
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read file {}", path.display()))?;
    Ok(sha256_hex(&contents))
}

fn validate_project_override(path: &Path) -> Result<ProjectDirectoryOverride> {
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read project override from {}", path.display()))?;
    toml::from_str(&contents)
        .with_context(|| format!("Failed to parse project override from {}", path.display()))
}

fn sha256_hex(contents: &str) -> String {
    let digest = Sha256::digest(contents.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn find_git_root(dir: &Path) -> Option<PathBuf> {
    let mut current = dir.to_path_buf();
    loop {
        if current.join(".git").exists() {
            return Some(current);
        }
        if !current.pop() {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tempfile::TempDir;

    #[test]
    fn test_default_config() {
        let config = Config {
            defaults: Defaults::default(),
            log: LogConfig::default(),
            updates: UpdateConfig::default(),
            projects: vec![],
        };
        assert_eq!(config.defaults.backend, "op");
    }

    #[test]
    fn test_parse_minimal_config() {
        let toml_str = r#"
[defaults]
backend = "bw"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.defaults.backend, "bw");
    }

    #[test]
    fn test_parse_full_config() {
        let toml_str = r#"
[defaults]
backend = "op"

[defaults.op]
vault = "Development"
account = "my-account"
item = "project-secrets"

[defaults.bw]
folder = "env-secrets"

[defaults.gpg]
file_pattern = ".secrets.gpg"
recipient = "user@example.com"

[log]
level = "debug"

[[projects]]
path = "/home/user/project-a"
backend = "bw"
item = "project-a-env"

[[projects]]
path = "/home/user/project-b"
backend = "gpg"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.defaults.backend, "op");
        assert_eq!(config.defaults.op.vault.as_deref(), Some("Development"));
        assert_eq!(config.projects.len(), 2);
        assert_eq!(config.projects[0].backend.as_deref(), Some("bw"));

        let dir = Path::new("/home/user/project-a");
        assert_eq!(config.effective_backend(dir), "bw");
        assert_eq!(config.effective_item(dir), Some("project-a-env"));

        let dir2 = Path::new("/home/user/other");
        assert_eq!(config.effective_backend(dir2), "op");
    }

    #[test]
    fn test_project_for_prefers_most_specific_match() {
        let config = Config {
            defaults: Defaults::default(),
            log: LogConfig::default(),
            updates: UpdateConfig::default(),
            projects: vec![
                ProjectOverride {
                    path: "/home/user/work".to_string(),
                    backend: Some("bw".to_string()),
                    ..ProjectOverride::default()
                },
                ProjectOverride {
                    path: "/home/user/work/service-a".to_string(),
                    backend: Some("gpg".to_string()),
                    ..ProjectOverride::default()
                },
            ],
        };

        let project = config
            .project_for(Path::new("/home/user/work/service-a/api"))
            .unwrap();
        assert_eq!(project.backend.as_deref(), Some("gpg"));
    }

    #[test]
    fn test_parse_project_directory_override() {
        let toml_str = r#"
backend = "op"
item = "service-a-env"
commands = ["cargo", "npm"]

[op]
vault = "Work"
"#;

        let local_override: ProjectDirectoryOverride = toml::from_str(toml_str).unwrap();
        assert_eq!(local_override.backend.as_deref(), Some("op"));
        assert_eq!(local_override.item.as_deref(), Some("service-a-env"));
        assert_eq!(local_override.commands, vec!["cargo", "npm"]);
        assert_eq!(
            local_override.op.and_then(|op| op.vault),
            Some("Work".to_string())
        );
    }

    #[test]
    fn test_effective_commands_for_project() {
        let config = Config {
            defaults: Defaults::default(),
            log: LogConfig::default(),
            updates: UpdateConfig::default(),
            projects: vec![ProjectOverride {
                path: "/home/user/work/service-a".to_string(),
                commands: vec!["cargo".to_string(), "npm".to_string()],
                ..ProjectOverride::default()
            }],
        };

        assert_eq!(
            config.effective_commands(Path::new("/home/user/work/service-a/api")),
            ["cargo".to_string(), "npm".to_string()]
        );
        assert!(
            config
                .effective_commands(Path::new("/home/user/other"))
                .is_empty()
        );
    }

    #[test]
    fn test_approval_store_round_trip_and_revoke() {
        let test_dir = unique_test_dir("approval-store");
        let override_path = test_dir.join(PROJECT_OVERRIDE_FILE_NAME);
        let store_path = test_dir.join("approved-project-configs.json");

        fs::create_dir_all(&test_dir).unwrap();
        fs::write(&override_path, "backend = \"op\"\n").unwrap();

        let approved_hash = hash_file(&override_path).unwrap();
        let mut approvals = ApprovedProjectConfigs::default();
        approvals.approve(&override_path, approved_hash.clone());
        approvals.save_to_path(&store_path).unwrap();

        let loaded = ApprovedProjectConfigs::load_from_path(&store_path).unwrap();
        assert_eq!(
            loaded.approved_hash(&override_path),
            Some(approved_hash.as_str())
        );
        assert_eq!(loaded.entries().len(), 1);

        let mut loaded = loaded;
        assert!(loaded.revoke(&override_path));
        assert_eq!(loaded.approved_hash(&override_path), None);

        let _ = fs::remove_dir_all(&test_dir);
    }

    #[test]
    fn test_secret_fetch_approval_store_round_trip_and_revoke() {
        let test_dir = unique_test_dir("secret-fetch-approval-store");
        let project_dir = test_dir.join("project");
        let env_path = project_dir.join(".env");
        let store_path = test_dir.join("approved-secret-fetches.json");

        fs::create_dir_all(&project_dir).unwrap();
        fs::write(&env_path, "API_KEY=op://vault/item/api_key\n").unwrap();

        let env_hash = hash_file(&env_path).unwrap();
        let mut approvals = ApprovedSecretFetches::default();
        approvals.approve_hash(&project_dir, env_hash.clone());
        approvals.allow_project_wide(&project_dir);
        approvals.save_to_path(&store_path).unwrap();

        let loaded = ApprovedSecretFetches::load_from_path(&store_path).unwrap();
        assert!(loaded.is_approved(&project_dir, &env_hash));
        assert!(loaded.is_project_wide(&project_dir));
        assert_eq!(
            loaded.approved_hashes(&project_dir),
            BTreeSet::from([env_hash])
        );

        let mut loaded = loaded;
        assert!(loaded.revoke(&project_dir));
        assert!(!loaded.is_project_wide(&project_dir));
        assert!(loaded.approved_hashes(&project_dir).is_empty());

        let _ = fs::remove_dir_all(&test_dir);
    }

    #[test]
    fn test_resolve_secret_fetch_target_prefers_git_root_as_project() {
        let test_dir = unique_test_dir("resolve-secret-fetch-target");
        let git_root = test_dir.join("repo");
        let service_dir = git_root.join("services/api");
        let env_path = service_dir.join(".env");

        fs::create_dir_all(git_root.join(".git")).unwrap();
        fs::create_dir_all(&service_dir).unwrap();
        fs::write(&env_path, "API_KEY=\n").unwrap();

        let (project_path, resolved_env_path) = resolve_secret_fetch_target(&service_dir).unwrap();
        assert_eq!(project_path, git_root.canonicalize().unwrap());
        assert_eq!(resolved_env_path, env_path.canonicalize().unwrap());

        let _ = fs::remove_dir_all(&test_dir);
    }

    #[test]
    fn test_validate_project_override() {
        let test_dir = unique_test_dir("validate-project-override");
        let override_path = test_dir.join(PROJECT_OVERRIDE_FILE_NAME);
        fs::create_dir_all(&test_dir).unwrap();
        fs::write(
            &override_path,
            "backend = \"op\"\ncommands = [\"cargo\"]\n[op]\nvault = \"Work\"\n",
        )
        .unwrap();

        let parsed = validate_project_override(&override_path).unwrap();
        assert_eq!(parsed.backend.as_deref(), Some("op"));
        assert_eq!(parsed.commands, vec!["cargo"]);
        assert_eq!(
            parsed.op.and_then(|config| config.vault),
            Some("Work".to_string())
        );

        let _ = fs::remove_dir_all(&test_dir);
    }

    #[test]
    fn test_reviewed_migrations_round_trip_and_forget() {
        let test_dir = unique_test_dir("reviewed-migrations");
        let env_path = test_dir.join(".env");
        let store_path = test_dir.join("reviewed-migrations.json");

        fs::create_dir_all(&test_dir).unwrap();
        fs::write(&env_path, "API_KEY=value\n").unwrap();

        let mut reviewed = ReviewedMigrations::default();
        reviewed.remember(&env_path, ["fp-1".to_string(), "fp-2".to_string()]);
        reviewed.save_to_path(&store_path).unwrap();

        let loaded = ReviewedMigrations::load_from_path(&store_path).unwrap();
        assert_eq!(
            loaded.fingerprints(&env_path),
            BTreeSet::from(["fp-1".to_string(), "fp-2".to_string()])
        );

        let mut loaded = loaded;
        loaded.forget(&env_path, ["fp-1".to_string(), "fp-2".to_string()]);
        assert!(loaded.fingerprints(&env_path).is_empty());

        let _ = fs::remove_dir_all(&test_dir);
    }

    #[test]
    fn test_sha256_hex_produces_correct_digest() {
        // SHA-256("hello") is a well-known value.
        assert_eq!(
            sha256_hex("hello"),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn test_sha256_hex_empty_string() {
        assert_eq!(
            sha256_hex(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn test_normalize_path_returns_nonempty_canonical_string() {
        let dir = std::env::temp_dir();
        let result = normalize_path(&dir);
        assert!(!result.is_empty());
        // The canonical path should match what std::fs::canonicalize returns.
        let expected = dir.canonicalize().unwrap().to_string_lossy().into_owned();
        assert_eq!(result, expected);
    }

    #[test]
    fn test_hash_file_matches_sha256_hex_of_content() {
        let test_dir = unique_test_dir("hash-file");
        let path = test_dir.join("content.txt");
        let content = "backend = \"op\"\n";
        fs::create_dir_all(&test_dir).unwrap();
        fs::write(&path, content).unwrap();

        let result = hash_file(&path).unwrap();
        assert_eq!(result, sha256_hex(content));

        let _ = fs::remove_dir_all(&test_dir);
    }

    fn unique_test_dir(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("pw-env-{name}-{}-{nonce}", std::process::id()))
    }

    #[test]
    fn test_sha256_hex_produces_64_hex_chars() {
        let result = sha256_hex("hello world");
        assert_eq!(result.len(), 64);
        assert!(result.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_expand_path_with_tilde_prefix() {
        let result = expand_path("~/foo/bar");
        let s = result.to_string_lossy();
        assert!(!s.starts_with('~'), "tilde should be expanded");
        assert!(s.ends_with("foo/bar"));
    }

    #[test]
    fn test_expand_path_without_tilde() {
        let result = expand_path("/absolute/path");
        assert_eq!(result, PathBuf::from("/absolute/path"));
    }

    #[test]
    fn test_expand_path_relative_unchanged() {
        let result = expand_path("relative/path");
        assert_eq!(result, PathBuf::from("relative/path"));
    }

    #[test]
    fn test_effective_op_uses_project_override() {
        let config = Config {
            defaults: Defaults {
                backend: "op".to_string(),
                op: OpConfig {
                    vault: Some("DefaultVault".to_string()),
                    ..Default::default()
                },
                ..Defaults::default()
            },
            log: LogConfig::default(),
            updates: UpdateConfig::default(),
            projects: vec![ProjectOverride {
                path: "/home/user/work".to_string(),
                op: Some(OpConfig {
                    vault: Some("WorkVault".to_string()),
                    ..Default::default()
                }),
                ..ProjectOverride::default()
            }],
        };
        let op = config.effective_op(Path::new("/home/user/work/api"));
        assert_eq!(op.vault.as_deref(), Some("WorkVault"));
    }

    #[test]
    fn test_effective_cache_uses_project_override() {
        let config = Config {
            defaults: Defaults {
                cache: CacheConfig {
                    enabled: true,
                    ttl_hours: 4,
                },
                ..Defaults::default()
            },
            log: LogConfig::default(),
            updates: UpdateConfig::default(),
            projects: vec![ProjectOverride {
                path: "/home/user/work".to_string(),
                cache: Some(CacheConfig {
                    enabled: false,
                    ttl_hours: 1,
                }),
                ..ProjectOverride::default()
            }],
        };

        let cache = config.effective_cache(Path::new("/home/user/work/service"));
        assert!(!cache.enabled);
        assert_eq!(cache.ttl_hours, 1);
    }

    #[test]
    fn test_effective_bw_uses_project_override() {
        let config = Config {
            defaults: Defaults {
                backend: "bw".to_string(),
                bw: BwConfig {
                    folder: Some("default-folder".to_string()),
                    sync_throttle_secs: 300,
                    ..Default::default()
                },
                ..Defaults::default()
            },
            log: LogConfig::default(),
            updates: UpdateConfig::default(),
            projects: vec![ProjectOverride {
                path: "/home/user/work".to_string(),
                bw: Some(BwConfig {
                    folder: Some("work-folder".to_string()),
                    sync_throttle_secs: 30,
                    ..Default::default()
                }),
                ..ProjectOverride::default()
            }],
        };
        let bw = config.effective_bw(Path::new("/home/user/work/service"));
        assert_eq!(bw.folder.as_deref(), Some("work-folder"));
        assert_eq!(bw.sync_throttle_secs, 30);
    }

    #[test]
    fn test_bw_config_default_sync_throttle_is_set() {
        assert_eq!(BwConfig::default().sync_throttle_secs, 3600);
    }

    #[test]
    fn test_cache_config_defaults_are_enabled_for_four_hours() {
        let cache = CacheConfig::default();
        assert!(cache.enabled);
        assert_eq!(cache.ttl_hours, 4);
    }

    #[test]
    fn test_effective_gpg_uses_project_override() {
        let config = Config {
            defaults: Defaults {
                gpg: GpgConfig {
                    file_pattern: ".env.gpg".to_string(),
                    recipient: None,
                },
                ..Defaults::default()
            },
            log: LogConfig::default(),
            updates: UpdateConfig::default(),
            projects: vec![ProjectOverride {
                path: "/home/user/personal".to_string(),
                gpg: Some(GpgConfig {
                    file_pattern: ".secrets.gpg".to_string(),
                    recipient: Some("me@example.com".to_string()),
                }),
                ..ProjectOverride::default()
            }],
        };
        let gpg = config.effective_gpg(Path::new("/home/user/personal/blog"));
        assert_eq!(gpg.file_pattern, ".secrets.gpg");
    }

    #[test]
    fn test_effective_item_from_project() {
        let config = Config {
            defaults: Defaults::default(),
            log: LogConfig::default(),
            updates: UpdateConfig::default(),
            projects: vec![ProjectOverride {
                path: "/home/user/work".to_string(),
                item: Some("project-item".to_string()),
                ..ProjectOverride::default()
            }],
        };
        assert_eq!(
            config.effective_item(Path::new("/home/user/work/api")),
            Some("project-item")
        );
    }

    #[test]
    fn test_effective_item_from_op_config() {
        let config = Config {
            defaults: Defaults {
                backend: "op".to_string(),
                op: OpConfig {
                    item: Some("default-op-item".to_string()),
                    ..Default::default()
                },
                ..Defaults::default()
            },
            log: LogConfig::default(),
            updates: UpdateConfig::default(),
            projects: vec![],
        };
        assert_eq!(
            config.effective_item(Path::new("/any/dir")),
            Some("default-op-item")
        );
    }

    #[test]
    fn test_effective_item_from_bw_config() {
        let config = Config {
            defaults: Defaults {
                backend: "bw".to_string(),
                bw: BwConfig {
                    item: Some("default-bw-item".to_string()),
                    ..Default::default()
                },
                ..Defaults::default()
            },
            log: LogConfig::default(),
            updates: UpdateConfig::default(),
            projects: vec![],
        };
        assert_eq!(
            config.effective_item(Path::new("/any/dir")),
            Some("default-bw-item")
        );
    }

    #[test]
    fn test_effective_item_none_when_not_configured() {
        let config = Config {
            defaults: Defaults::default(),
            log: LogConfig::default(),
            updates: UpdateConfig::default(),
            projects: vec![],
        };
        assert_eq!(config.effective_item(Path::new("/any/dir")), None);
    }

    #[test]
    fn test_effective_source_all_defaults_to_false() {
        let config = Config {
            defaults: Defaults::default(),
            log: LogConfig::default(),
            updates: UpdateConfig::default(),
            projects: vec![],
        };
        assert!(!config.effective_source_all(Path::new("/any/dir")));
    }

    #[test]
    fn test_effective_source_all_uses_defaults_when_enabled() {
        let config = Config {
            defaults: Defaults {
                source_all: true,
                ..Defaults::default()
            },
            log: LogConfig::default(),
            updates: UpdateConfig::default(),
            projects: vec![],
        };
        assert!(config.effective_source_all(Path::new("/any/dir")));
    }

    #[test]
    fn test_effective_source_all_project_override_takes_precedence() {
        let config = Config {
            defaults: Defaults {
                source_all: false,
                ..Defaults::default()
            },
            log: LogConfig::default(),
            updates: UpdateConfig::default(),
            projects: vec![ProjectOverride {
                path: "/home/user/work".to_string(),
                source_all: Some(true),
                ..ProjectOverride::default()
            }],
        };
        assert!(config.effective_source_all(Path::new("/home/user/work/api")));
        assert!(!config.effective_source_all(Path::new("/home/user/other")));
    }

    #[test]
    fn test_source_all_parsed_from_toml() {
        let toml_str = r#"
[defaults]
source_all = true
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.defaults.source_all);
    }

    #[test]
    fn test_source_all_defaults_to_false_when_omitted_from_toml() {
        let toml_str = r#"
[defaults]
backend = "op"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(!config.defaults.source_all);
    }

    #[test]
    fn test_config_path_contains_pw_env_and_toml() {
        let path = Config::config_path();
        let s = path.to_string_lossy();
        assert!(s.contains("pw-env"), "path should contain 'pw-env'");
        assert!(
            s.ends_with("config.toml"),
            "path should end with config.toml"
        );
    }

    #[test]
    fn base_dir_from_env_prefers_explicit_env_value() {
        let path = base_dir_from_env(
            Some(PathBuf::from("/tmp/xdg-state")),
            Some(PathBuf::from("/tmp/home")),
            &[".local", "state"],
        );

        assert_eq!(path, PathBuf::from("/tmp/xdg-state"));
    }

    #[test]
    fn base_dir_from_env_uses_home_with_fallback_components() {
        let path = base_dir_from_env(None, Some(PathBuf::from("/tmp/home")), &[".config"]);

        assert_eq!(path, PathBuf::from("/tmp/home/.config"));
    }

    #[test]
    fn state_dir_path_returns_local_state_location() {
        let path = state_dir();
        let display = path.to_string_lossy();

        assert!(display.contains(".local") || display.contains("local"));
        assert!(display.contains("state"));
    }

    #[test]
    fn test_approved_project_configs_load_from_missing_path_returns_empty() {
        let test_dir = unique_test_dir("apc-missing");
        let store_path = test_dir.join("approved-project-configs.json");
        // File doesn't exist
        let loaded = ApprovedProjectConfigs::load_from_path(&store_path).unwrap();
        assert_eq!(loaded.entries().len(), 0);
    }

    #[test]
    fn test_approved_secret_fetches_load_from_missing_path_returns_empty() {
        let test_dir = unique_test_dir("asf-missing");
        let store_path = test_dir.join("approved-secret-fetches.json");
        let loaded = ApprovedSecretFetches::load_from_path(&store_path).unwrap();
        assert_eq!(loaded.entries().len(), 0);
    }

    #[test]
    fn test_hash_file_known_content() {
        let test_dir = unique_test_dir("hash-file");
        fs::create_dir_all(&test_dir).unwrap();
        let file_path = test_dir.join("test.txt");
        fs::write(&file_path, "hello").unwrap();
        let hash = hash_file(&file_path).unwrap();
        // SHA256 of "hello"
        assert_eq!(
            hash,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        let _ = fs::remove_dir_all(&test_dir);
    }

    #[test]
    fn test_normalize_path_nonexistent_returns_input() {
        let path = PathBuf::from("/nonexistent/path/that/does/not/exist/12345");
        let normalized = normalize_path(&path);
        assert_eq!(normalized, "/nonexistent/path/that/does/not/exist/12345");
    }

    #[test]
    fn test_normalize_path_existing_dir() {
        let test_dir = unique_test_dir("normalize-path");
        fs::create_dir_all(&test_dir).unwrap();
        let normalized = normalize_path(&test_dir);
        assert!(!normalized.is_empty());
        let _ = fs::remove_dir_all(&test_dir);
    }

    #[test]
    fn test_secret_fetch_is_approved_project_wide() {
        let test_dir = unique_test_dir("sf-project-wide");
        fs::create_dir_all(&test_dir).unwrap();
        let project_dir = test_dir.join("project");
        fs::create_dir_all(&project_dir).unwrap();

        let mut approvals = ApprovedSecretFetches::default();
        approvals.allow_project_wide(&project_dir);

        assert!(approvals.is_approved(&project_dir, "any-hash-at-all"));
        assert!(approvals.is_project_wide(&project_dir));
        let _ = fs::remove_dir_all(&test_dir);
    }

    #[test]
    fn test_secret_fetch_is_approved_by_hash() {
        let test_dir = unique_test_dir("sf-by-hash");
        fs::create_dir_all(&test_dir).unwrap();
        let project_dir = test_dir.join("project");
        fs::create_dir_all(&project_dir).unwrap();

        let mut approvals = ApprovedSecretFetches::default();
        approvals.approve_hash(&project_dir, "abc123".to_string());

        assert!(approvals.is_approved(&project_dir, "abc123"));
        assert!(!approvals.is_approved(&project_dir, "different-hash"));
        let _ = fs::remove_dir_all(&test_dir);
    }

    #[test]
    fn approve_hash_evicts_excess_entries_keeping_exactly_max() {
        // Insert MAX_HASHES_PER_PROJECT + 2 (12) hashes.  After each insertion the
        // function must trim to exactly 10.  With the `-` → `+` mutation, take(22)
        // would drain the set to 0.  With the `-` → `/` mutation, take(12/10 = 1)
        // would leave 11 entries.
        let test_dir = unique_test_dir("asf-evict-excess");
        fs::create_dir_all(&test_dir).unwrap();
        let project_dir = test_dir.join("project");
        fs::create_dir_all(&project_dir).unwrap();

        let mut approvals = ApprovedSecretFetches::default();
        for i in 0..12usize {
            approvals.approve_hash(&project_dir, format!("hash-{i:02}"));
        }

        let count = approvals.approved_hashes(&project_dir).len();
        let _ = fs::remove_dir_all(&test_dir);

        assert_eq!(
            count, 10,
            "approve_hash should retain exactly 10 hashes, got {count}"
        );
    }

    #[test]
    fn test_reviewed_migrations_remember_and_fingerprints() {
        let test_dir = unique_test_dir("rm-remember");
        fs::create_dir_all(&test_dir).unwrap();
        let env_path = test_dir.join(".env");
        fs::write(&env_path, "API_KEY=value\n").unwrap();

        let mut reviewed = ReviewedMigrations::default();
        reviewed.remember(&env_path, ["fp-a".to_string(), "fp-b".to_string()]);

        let fps = reviewed.fingerprints(&env_path);
        assert!(fps.contains("fp-a"));
        assert!(fps.contains("fp-b"));
        let _ = fs::remove_dir_all(&test_dir);
    }

    #[test]
    fn test_reviewed_migrations_save_and_load() {
        let test_dir = unique_test_dir("rm-save-load");
        fs::create_dir_all(&test_dir).unwrap();
        let env_path = test_dir.join(".env");
        let store_path = test_dir.join("reviewed.json");
        fs::write(&env_path, "API_KEY=value\n").unwrap();

        let mut reviewed = ReviewedMigrations::default();
        reviewed.remember(&env_path, ["fp-1".to_string()]);
        reviewed.save_to_path(&store_path).unwrap();

        let loaded = ReviewedMigrations::load_from_path(&store_path).unwrap();
        let fps = loaded.fingerprints(&env_path);
        assert!(fps.contains("fp-1"));
        let _ = fs::remove_dir_all(&test_dir);
    }

    #[test]
    fn test_config_approval_store_path_does_not_panic() {
        let _ = Config::approval_store_path();
    }

    #[test]
    fn test_config_secret_fetch_approval_store_path_does_not_panic() {
        let _ = Config::secret_fetch_approval_store_path();
    }

    #[test]
    fn test_config_approved_project_configs_returns_result() {
        let result = Config::approved_project_configs();
        assert!(result.is_ok());
    }

    #[test]
    fn test_config_approved_secret_fetches_returns_result() {
        let result = Config::approved_secret_fetches();
        assert!(result.is_ok());
    }

    #[test]
    fn test_config_project_override_approval_status_with_temp_file() {
        let test_dir = unique_test_dir("proj-override-status");
        fs::create_dir_all(&test_dir).unwrap();
        let override_path = test_dir.join(PROJECT_OVERRIDE_FILE_NAME);
        fs::write(&override_path, "backend = \"op\"\n").unwrap();

        let result = Config::project_override_approval_status(&override_path);
        assert!(result.is_ok());
        let status = result.unwrap();
        assert!(status.current_hash.is_some());
        assert!(status.approved_hash.is_none()); // not yet approved

        let _ = fs::remove_dir_all(&test_dir);
    }

    #[test]
    fn test_config_secret_fetch_approval_status_with_temp_env() {
        let test_dir = unique_test_dir("sf-status");
        fs::create_dir_all(&test_dir).unwrap();
        let env_path = test_dir.join(".env");
        fs::write(&env_path, "API_KEY=\n").unwrap();

        let result = Config::secret_fetch_approval_status(&env_path);
        assert!(result.is_ok());
        let status = result.unwrap();
        assert!(status.current_env_hash.is_some());
        assert!(!status.project_wide);

        let _ = fs::remove_dir_all(&test_dir);
    }

    #[test]
    fn test_ensure_secret_fetch_approved_bails_when_non_interactive() {
        let test_dir = unique_test_dir("ensure-sf-approved");
        fs::create_dir_all(&test_dir).unwrap();
        let env_path = test_dir.join(".env");
        fs::write(&env_path, "API_KEY=op://vault/item/field\n").unwrap();

        // In tests, stdin is not a terminal, so this should bail
        let result = Config::ensure_secret_fetch_approved(&env_path);
        assert!(result.is_err());

        let _ = fs::remove_dir_all(&test_dir);
    }

    #[test]
    fn test_revoke_project_override_approval_returns_false_when_not_approved() {
        let test_dir = unique_test_dir("revoke-proj-override");
        fs::create_dir_all(&test_dir).unwrap();
        let override_path = test_dir.join(PROJECT_OVERRIDE_FILE_NAME);
        fs::write(&override_path, "backend = \"bw\"\n").unwrap();

        let result = Config::revoke_project_override_approval(&override_path);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), false); // nothing to revoke

        let _ = fs::remove_dir_all(&test_dir);
    }

    #[test]
    fn test_revoke_secret_fetch_approval_returns_false_when_not_approved() {
        let test_dir = unique_test_dir("revoke-sf");
        fs::create_dir_all(&test_dir).unwrap();
        let env_path = test_dir.join(".env");
        fs::write(&env_path, "DB_URL=\n").unwrap();

        let result = Config::revoke_secret_fetch_approval(&env_path);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), false);

        let _ = fs::remove_dir_all(&test_dir);
    }

    #[test]
    fn test_config_load_for_dir_with_no_override_returns_config() {
        let test_dir = unique_test_dir("load-for-dir");
        fs::create_dir_all(&test_dir).unwrap();

        // Dir with no .pw-env.toml override
        let result = Config::load_for_dir(&test_dir);
        assert!(result.is_ok());

        let _ = fs::remove_dir_all(&test_dir);
    }

    #[test]
    fn test_find_git_root_in_config_module() {
        let test_dir = unique_test_dir("find-git-root-config");
        let repo_dir = test_dir.join("repo");
        let subdir = repo_dir.join("src");

        fs::create_dir_all(repo_dir.join(".git")).unwrap();
        fs::create_dir_all(&subdir).unwrap();

        let result = find_git_root(&subdir);
        let _ = fs::remove_dir_all(&test_dir);

        assert!(result.is_some());
        assert_eq!(result.unwrap().file_name().unwrap(), "repo");
    }

    #[test]
    fn test_resolve_secret_fetch_target_with_direct_env_file() {
        let test_dir = unique_test_dir("sf-target-direct");
        fs::create_dir_all(&test_dir).unwrap();
        let env_path = test_dir.join(".env");
        fs::write(&env_path, "KEY=value\n").unwrap();

        let result = resolve_secret_fetch_target(&env_path);
        assert!(result.is_ok());

        let _ = fs::remove_dir_all(&test_dir);
    }

    #[test]
    fn test_resolve_secret_fetch_target_with_directory() {
        let test_dir = unique_test_dir("sf-target-dir");
        fs::create_dir_all(&test_dir).unwrap();
        let env_path = test_dir.join(".env");
        fs::write(&env_path, "KEY=value\n").unwrap();

        // Pass the directory instead of the file
        let result = resolve_secret_fetch_target(&test_dir);
        assert!(result.is_ok());

        let _ = fs::remove_dir_all(&test_dir);
    }

    #[test]
    fn test_resolve_project_override_target_with_directory() {
        let test_dir = unique_test_dir("po-target-dir");
        fs::create_dir_all(&test_dir).unwrap();
        let override_path = test_dir.join(PROJECT_OVERRIDE_FILE_NAME);
        fs::write(&override_path, "backend = \"op\"\n").unwrap();

        let result = resolve_project_override_target(&test_dir);
        assert!(result.is_ok());
        assert_eq!(
            result.unwrap().file_name().unwrap(),
            PROJECT_OVERRIDE_FILE_NAME
        );

        let _ = fs::remove_dir_all(&test_dir);
    }

    #[test]
    fn test_reviewed_migrations_load_from_missing_path_returns_empty() {
        let test_dir = unique_test_dir("rm-missing");
        let store_path = test_dir.join("reviewed-migrations.json");
        let loaded = ReviewedMigrations::load_from_path(&store_path).unwrap();
        assert!(loaded.fingerprints(&store_path).is_empty());
    }

    #[test]
    fn test_approved_project_configs_load_returns_invalid_json_error() {
        let test_dir = unique_test_dir("apc-bad-json");
        fs::create_dir_all(&test_dir).unwrap();
        let store_path = test_dir.join("bad.json");
        fs::write(&store_path, "not valid json").unwrap();
        let result = ApprovedProjectConfigs::load_from_path(&store_path);
        assert!(result.is_err());
        let _ = fs::remove_dir_all(&test_dir);
    }

    #[test]
    fn test_approved_secret_fetches_load_returns_invalid_json_error() {
        let test_dir = unique_test_dir("asf-bad-json");
        fs::create_dir_all(&test_dir).unwrap();
        let store_path = test_dir.join("bad.json");
        fs::write(&store_path, "not valid json").unwrap();
        let result = ApprovedSecretFetches::load_from_path(&store_path);
        assert!(result.is_err());
        let _ = fs::remove_dir_all(&test_dir);
    }

    #[test]
    fn effective_item_with_bw_backend_returns_bw_item() {
        let config = Config {
            defaults: Defaults {
                backend: "bw".to_string(),
                bw: BwConfig {
                    item: Some("my-bw-item".to_string()),
                    ..Default::default()
                },
                ..Defaults::default()
            },
            log: LogConfig::default(),
            updates: UpdateConfig::default(),
            projects: vec![],
        };
        assert_eq!(
            config.effective_item(std::path::Path::new("/tmp")),
            Some("my-bw-item")
        );
    }

    #[test]
    fn effective_item_with_no_backend_item_returns_none() {
        let config = Config {
            defaults: Defaults {
                backend: "gpg".to_string(),
                ..Defaults::default()
            },
            log: LogConfig::default(),
            updates: UpdateConfig::default(),
            projects: vec![],
        };
        assert_eq!(config.effective_item(std::path::Path::new("/tmp")), None);
    }

    #[test]
    fn effective_commands_with_matching_project_returns_commands() {
        let temp_dir = TempDir::new().unwrap();
        let canonical_path = temp_dir.path().canonicalize().unwrap();
        let config = Config {
            defaults: Defaults::default(),
            log: LogConfig::default(),
            updates: UpdateConfig::default(),
            projects: vec![ProjectOverride {
                path: canonical_path.to_string_lossy().into_owned(),
                commands: vec!["echo".to_string(), "cat".to_string()],
                backend: None,
                search_parent_env: None,
                source_all: None,
                warn_missing: None,
                cache: None,
                op: None,
                bw: None,
                gpg: None,
                item: None,
            }],
        };
        let cmds = config.effective_commands(&canonical_path);
        assert_eq!(cmds, &["echo", "cat"]);
    }

    #[test]
    fn effective_commands_without_matching_project_returns_empty() {
        let config = Config {
            defaults: Defaults::default(),
            log: LogConfig::default(),
            updates: UpdateConfig::default(),
            projects: vec![],
        };
        let cmds = config.effective_commands(std::path::Path::new("/tmp"));
        assert!(cmds.is_empty());
    }

    #[test]
    fn reviewed_migration_entry_fingerprints_returns_empty_for_new_path() {
        let test_dir = unique_test_dir("rmef-new");
        let env_path = std::path::Path::new(&test_dir).join(".env");
        // Should return empty set since no fingerprints have been stored
        let result = Config::reviewed_migration_entry_fingerprints(&env_path);
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn default_updates_enabled_returns_true() {
        assert!(UpdateConfig::default().enabled);
    }

    #[test]
    fn default_update_check_interval_returns_24() {
        assert_eq!(UpdateConfig::default().check_interval_hours, 24);
    }

    #[test]
    fn default_gpg_file_pattern_returns_env_gpg() {
        assert_eq!(GpgConfig::default().file_pattern, ".env.gpg");
    }

    #[test]
    fn default_log_level_returns_info() {
        assert_eq!(LogConfig::default().level, "info");
    }

    #[test]
    fn default_log_file_returns_some_nonempty_path() {
        // The default log file path is derived from the system state/data dir and must
        // be non-empty and end with the expected filename.
        let log_file = LogConfig::default().file;
        assert!(log_file.is_some(), "log file default should be Some");
        let path = log_file.unwrap();
        assert!(!path.is_empty());
        assert!(
            path.ends_with("pw-env.log"),
            "expected log path ending in pw-env.log, got {path}"
        );
    }

    #[test]
    fn approved_secret_fetches_entries_returns_project_wide_entry() {
        let test_dir = unique_test_dir("asf-entries-pw");
        fs::create_dir_all(&test_dir).unwrap();
        let project_dir = test_dir.join("project");
        fs::create_dir_all(&project_dir).unwrap();

        let mut approvals = ApprovedSecretFetches::default();
        approvals.allow_project_wide(&project_dir);

        let entries = approvals.entries();
        let _ = fs::remove_dir_all(&test_dir);

        assert_eq!(entries.len(), 1);
        assert!(entries[0].project_wide);
    }

    #[test]
    fn approved_secret_fetches_entries_returns_hash_based_entry() {
        let test_dir = unique_test_dir("asf-entries-hash");
        fs::create_dir_all(&test_dir).unwrap();
        let project_dir = test_dir.join("project");
        fs::create_dir_all(&project_dir).unwrap();

        let mut approvals = ApprovedSecretFetches::default();
        approvals.approve_hash(&project_dir, "abc123".to_string());

        let entries = approvals.entries();
        let _ = fs::remove_dir_all(&test_dir);

        assert_eq!(entries.len(), 1);
        assert!(!entries[0].project_wide);
        assert_eq!(entries[0].env_hash.as_deref(), Some("abc123"));
    }

    #[test]
    fn approved_secret_fetches_revoke_returns_true_when_hash_removed() {
        // Revoking a hash-only approval must return true (not false from && mutation).
        let test_dir = unique_test_dir("asf-revoke-hash");
        fs::create_dir_all(&test_dir).unwrap();
        let project_dir = test_dir.join("project");
        fs::create_dir_all(&project_dir).unwrap();

        let mut approvals = ApprovedSecretFetches::default();
        approvals.approve_hash(&project_dir, "abc123".to_string());

        // removed_hashes=true, removed_project=false → true || false = true
        // With && mutation: true && false = false → test fails → kills mutant
        let revoked = approvals.revoke(&project_dir);
        let _ = fs::remove_dir_all(&test_dir);

        assert!(revoked);
    }

    #[test]
    fn approval_store_path_returns_some_nonempty_path() {
        // Must not return None (kills the -> None mutation).
        let path = Config::approval_store_path();
        assert!(
            path.is_some(),
            "approval store path should be Some on this platform"
        );
        let p = path.unwrap();
        assert!(!p.as_os_str().is_empty());
        assert!(
            p.to_string_lossy()
                .ends_with("approved-project-configs.json"),
            "unexpected path: {}",
            p.display()
        );
    }

    #[test]
    fn secret_fetch_approval_store_path_returns_some_nonempty_path() {
        let path = Config::secret_fetch_approval_store_path();
        assert!(
            path.is_some(),
            "secret fetch approval store path should be Some on this platform"
        );
        let p = path.unwrap();
        assert!(!p.as_os_str().is_empty());
        assert!(
            p.to_string_lossy()
                .ends_with("approved-secret-fetches.json"),
            "unexpected path: {}",
            p.display()
        );
    }

    #[test]
    fn project_override_path_returns_some_when_override_file_exists_in_dir() {
        // Creates a temp dir containing .pw-env.toml (no git root) and verifies
        // that project_override_path returns Some pointing to that file.
        let test_dir = unique_test_dir("proj-override-path");
        fs::create_dir_all(&test_dir).unwrap();
        let override_file = test_dir.join(".pw-env.toml");
        fs::write(&override_file, "backend = \"op\"\n").unwrap();

        let result = Config::project_override_path(&test_dir);
        let _ = fs::remove_dir_all(&test_dir);

        assert!(result.is_some(), "expected Some path, got None");
        let path = result.unwrap();
        assert!(path.to_string_lossy().ends_with(".pw-env.toml"));
    }

    #[test]
    fn project_override_path_returns_none_when_no_override_file() {
        // A dir with no .pw-env.toml and no git root must return None.
        let test_dir = unique_test_dir("proj-override-none");
        fs::create_dir_all(&test_dir).unwrap();

        let result = Config::project_override_path(&test_dir);
        let _ = fs::remove_dir_all(&test_dir);

        assert!(result.is_none(), "expected None, got {:?}", result);
    }

    #[test]
    fn approved_project_configs_round_trip_via_custom_path() {
        // Saves an approval, loads it back, and verifies entries() is non-empty.
        // This kills the ApprovedProjectConfigs::load -> Ok(Default::default()) mutation.
        let test_dir = unique_test_dir("apc-round-trip");
        fs::create_dir_all(&test_dir).unwrap();
        let store_path = test_dir.join("approved-project-configs.json");
        let project_path = test_dir.join("project");
        fs::create_dir_all(&project_path).unwrap();

        let mut approvals = ApprovedProjectConfigs::default();
        approvals.approve(&project_path, "abc123hash".to_string());
        approvals.save_to_path(&store_path).unwrap();

        let loaded = ApprovedProjectConfigs::load_from_path(&store_path).unwrap();
        let entries = loaded.entries();
        let _ = fs::remove_dir_all(&test_dir);

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].hash, "abc123hash");
    }

    #[test]
    fn approved_project_configs_revoke_removes_entry() {
        let test_dir = unique_test_dir("apc-revoke");
        fs::create_dir_all(&test_dir).unwrap();
        let project_path = test_dir.join("project");
        fs::create_dir_all(&project_path).unwrap();

        let mut approvals = ApprovedProjectConfigs::default();
        approvals.approve(&project_path, "hash1".to_string());
        assert_eq!(approvals.entries().len(), 1);

        let removed = approvals.revoke(&project_path);
        let _ = fs::remove_dir_all(&test_dir);

        assert!(removed);
        assert!(approvals.entries().is_empty());
    }

    // ── Tests that exercise the Config-level approval APIs with isolated stores ──

    fn set_test_approval_store(dir: &std::path::Path) {
        TEST_APPROVAL_STORE_PATH
            .with(|v| *v.borrow_mut() = Some(dir.join("approved-project-configs.json")));
    }

    fn set_test_secret_fetch_store(dir: &std::path::Path) {
        TEST_SECRET_FETCH_STORE_PATH
            .with(|v| *v.borrow_mut() = Some(dir.join("approved-secret-fetches.json")));
    }

    fn set_test_reviewed_migrations_store(dir: &std::path::Path) {
        TEST_REVIEWED_MIGRATIONS_PATH
            .with(|v| *v.borrow_mut() = Some(dir.join("reviewed-migrations.json")));
    }

    #[test]
    fn approved_project_configs_contains_entry_after_approval() {
        let test_dir = unique_test_dir("apc-entries-after-approval");
        fs::create_dir_all(&test_dir).unwrap();
        set_test_approval_store(&test_dir);
        let override_path = test_dir.join(PROJECT_OVERRIDE_FILE_NAME);
        fs::write(&override_path, "backend = \"op\"\n").unwrap();

        Config::approve_project_override(&override_path).unwrap();
        let configs = Config::approved_project_configs().unwrap();
        let canonical = override_path.canonicalize().unwrap();
        let found = configs.iter().any(|e| e.path == canonical);
        let _ = fs::remove_dir_all(&test_dir);

        assert!(
            found,
            "approved_project_configs() should contain the newly approved entry"
        );
    }

    #[test]
    fn revoke_project_override_approval_returns_true_for_approved_entry() {
        let test_dir = unique_test_dir("revoke-proj-override-true");
        fs::create_dir_all(&test_dir).unwrap();
        set_test_approval_store(&test_dir);
        let override_path = test_dir.join(PROJECT_OVERRIDE_FILE_NAME);
        fs::write(&override_path, "backend = \"op\"\n").unwrap();

        Config::approve_project_override(&override_path).unwrap();
        let result = Config::revoke_project_override_approval(&override_path).unwrap();
        let _ = fs::remove_dir_all(&test_dir);

        assert!(result, "revoking an approved override should return true");
    }

    #[test]
    fn approved_secret_fetches_contains_entry_after_approval() {
        let test_dir = unique_test_dir("asf-entries-after-approval");
        fs::create_dir_all(&test_dir).unwrap();
        set_test_secret_fetch_store(&test_dir);
        let env_path = test_dir.join(".env");
        fs::write(&env_path, "KEY=op://vault/item/key\n").unwrap();

        Config::approve_secret_fetch(&env_path, SecretFetchApprovalMode::CurrentEnvHash).unwrap();
        let fetches = Config::approved_secret_fetches().unwrap();
        let project_dir = test_dir.canonicalize().unwrap();
        let found = fetches
            .iter()
            .any(|e| e.project_path == project_dir && !e.project_wide);
        let _ = fs::remove_dir_all(&test_dir);

        assert!(
            found,
            "approved_secret_fetches() should contain the newly approved entry"
        );
    }

    #[test]
    fn revoke_secret_fetch_approval_returns_true_for_approved_entry() {
        let test_dir = unique_test_dir("revoke-sf-true");
        fs::create_dir_all(&test_dir).unwrap();
        set_test_secret_fetch_store(&test_dir);
        let env_path = test_dir.join(".env");
        fs::write(&env_path, "KEY=op://vault/item/key\n").unwrap();

        Config::approve_secret_fetch(&env_path, SecretFetchApprovalMode::CurrentEnvHash).unwrap();
        let result = Config::revoke_secret_fetch_approval(&env_path).unwrap();
        let _ = fs::remove_dir_all(&test_dir);

        assert!(
            result,
            "revoking an approved secret fetch should return true"
        );
    }

    // ── Tests for ensure_secret_fetch_approved interactive/non-interactive paths ─

    #[test]
    fn ensure_secret_fetch_approved_non_interactive_error_mentions_interactive_session() {
        let test_dir = unique_test_dir("ensure-sf-msg");
        fs::create_dir_all(&test_dir).unwrap();
        set_test_secret_fetch_store(&test_dir);
        let env_path = test_dir.join(".env");
        fs::write(&env_path, "KEY=op://vault/item/key\n").unwrap();

        // Provide mock stdin "y" so that if we accidentally enter the interactive path
        // (L431 delete-! mutant, or L987→true mutant) we get Ok(()) instead of Err,
        // which makes the assertion fail and kills the mutant.
        MOCK_STDIN_LINE.with(|m| *m.borrow_mut() = Some("y\n".to_string()));
        let result = Config::ensure_secret_fetch_approved(&env_path);
        let _ = fs::remove_dir_all(&test_dir);

        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("interactive"),
            "expected 'interactive session' message, got: {msg}"
        );
    }

    #[test]
    fn ensure_secret_fetch_approved_y_answer_approves_hash() {
        let test_dir = unique_test_dir("ensure-sf-y");
        fs::create_dir_all(&test_dir).unwrap();
        set_test_secret_fetch_store(&test_dir);
        let env_path = test_dir.join(".env");
        fs::write(&env_path, "KEY=op://vault/item/key\n").unwrap();

        MOCK_IS_TERMINAL.with(|v| v.set(true));
        MOCK_STDIN_LINE.with(|m| *m.borrow_mut() = Some("y\n".to_string()));
        let result = Config::ensure_secret_fetch_approved(&env_path);
        let _ = fs::remove_dir_all(&test_dir);

        assert!(
            result.is_ok(),
            "answer 'y' should approve and return Ok(())"
        );
    }

    #[test]
    fn ensure_secret_fetch_approved_a_answer_approves_project_wide() {
        let test_dir = unique_test_dir("ensure-sf-a");
        fs::create_dir_all(&test_dir).unwrap();
        set_test_secret_fetch_store(&test_dir);
        let env_path = test_dir.join(".env");
        fs::write(&env_path, "KEY=op://vault/item/key\n").unwrap();

        MOCK_IS_TERMINAL.with(|v| v.set(true));
        MOCK_STDIN_LINE.with(|m| *m.borrow_mut() = Some("a\n".to_string()));
        let result = Config::ensure_secret_fetch_approved(&env_path);
        let _ = fs::remove_dir_all(&test_dir);

        assert!(
            result.is_ok(),
            "answer 'a' should approve project-wide and return Ok(())"
        );
    }

    // ── Tests for project_override_file ancestor traversal (L582, L586) ────────

    #[test]
    fn project_override_path_finds_file_in_ancestor_at_git_root() {
        let test_dir = unique_test_dir("ancestor-override");
        let repo_dir = test_dir.join("repo");
        let subdir = repo_dir.join("src").join("lib");

        fs::create_dir_all(repo_dir.join(".git")).unwrap();
        fs::create_dir_all(&subdir).unwrap();
        fs::write(
            repo_dir.join(PROJECT_OVERRIDE_FILE_NAME),
            "backend = \"op\"\n",
        )
        .unwrap();

        let result = Config::project_override_path(&subdir);
        let _ = fs::remove_dir_all(&test_dir);

        assert!(
            result.is_some(),
            "should find override in ancestor git root"
        );
        assert!(result.unwrap().ends_with(PROJECT_OVERRIDE_FILE_NAME));
    }

    // ── Tests for ProjectDirectoryOverride::load_if_approved (L597, L606, L611, L637) ─

    #[test]
    fn load_if_approved_returns_some_for_pre_approved_hash() {
        let test_dir = unique_test_dir("load-if-approved-some");
        fs::create_dir_all(&test_dir).unwrap();
        set_test_approval_store(&test_dir);
        let override_path = test_dir.join(PROJECT_OVERRIDE_FILE_NAME);
        fs::write(&override_path, "backend = \"bw\"\n").unwrap();

        Config::approve_project_override(&override_path).unwrap();
        let result = ProjectDirectoryOverride::load_if_approved(&override_path);
        let _ = fs::remove_dir_all(&test_dir);

        assert!(result.is_ok());
        assert!(
            result.unwrap().is_some(),
            "pre-approved file should load as Some"
        );
    }

    #[test]
    fn load_if_approved_returns_none_for_unapproved_in_non_terminal() {
        let test_dir = unique_test_dir("load-if-approved-none");
        fs::create_dir_all(&test_dir).unwrap();
        set_test_approval_store(&test_dir);
        let override_path = test_dir.join(PROJECT_OVERRIDE_FILE_NAME);
        fs::write(&override_path, "backend = \"op\"\n").unwrap();

        // MOCK_IS_TERMINAL defaults to false; file not pre-approved
        let result = ProjectDirectoryOverride::load_if_approved(&override_path);
        let _ = fs::remove_dir_all(&test_dir);

        assert!(result.is_ok());
        assert!(
            result.unwrap().is_none(),
            "unapproved file in non-terminal should return None"
        );
    }

    #[test]
    fn load_if_approved_non_terminal_ignores_mock_stdin_y() {
        // Provides mock stdin "y" so if we accidentally enter the interactive path
        // (L611 delete-! mutant) the function returns Some instead of None, killing the mutant.
        let test_dir = unique_test_dir("load-if-approved-non-term-y");
        fs::create_dir_all(&test_dir).unwrap();
        set_test_approval_store(&test_dir);
        let override_path = test_dir.join(PROJECT_OVERRIDE_FILE_NAME);
        fs::write(&override_path, "backend = \"op\"\n").unwrap();

        // MOCK_IS_TERMINAL = false (default): non-interactive path should be taken
        MOCK_STDIN_LINE.with(|m| *m.borrow_mut() = Some("y\n".to_string()));
        let result = ProjectDirectoryOverride::load_if_approved(&override_path);
        let _ = fs::remove_dir_all(&test_dir);

        assert!(result.is_ok());
        assert!(
            result.unwrap().is_none(),
            "non-interactive path must return None without reading stdin"
        );
    }

    #[test]
    fn load_if_approved_y_answer_in_terminal_returns_some() {
        let test_dir = unique_test_dir("load-if-approved-y");
        fs::create_dir_all(&test_dir).unwrap();
        set_test_approval_store(&test_dir);
        let override_path = test_dir.join(PROJECT_OVERRIDE_FILE_NAME);
        fs::write(&override_path, "backend = \"bw\"\n").unwrap();

        MOCK_IS_TERMINAL.with(|v| v.set(true));
        MOCK_STDIN_LINE.with(|m| *m.borrow_mut() = Some("y\n".to_string()));
        let result = ProjectDirectoryOverride::load_if_approved(&override_path);
        let _ = fs::remove_dir_all(&test_dir);

        assert!(result.is_ok());
        assert!(
            result.unwrap().is_some(),
            "answer 'y' should approve and return Some"
        );
    }

    #[test]
    fn load_if_approved_yes_answer_in_terminal_returns_some() {
        let test_dir = unique_test_dir("load-if-approved-yes");
        fs::create_dir_all(&test_dir).unwrap();
        set_test_approval_store(&test_dir);
        let override_path = test_dir.join(PROJECT_OVERRIDE_FILE_NAME);
        fs::write(&override_path, "backend = \"bw\"\n").unwrap();

        MOCK_IS_TERMINAL.with(|v| v.set(true));
        MOCK_STDIN_LINE.with(|m| *m.borrow_mut() = Some("yes\n".to_string()));
        let result = ProjectDirectoryOverride::load_if_approved(&override_path);
        let _ = fs::remove_dir_all(&test_dir);

        assert!(result.is_ok());
        assert!(
            result.unwrap().is_some(),
            "answer 'yes' should approve and return Some"
        );
    }

    // ── Tests for ReviewedMigrations round-trip through Config (L483, L490, L499, L845, L872) ─

    #[test]
    fn remember_and_reviewed_migration_entries_round_trip() {
        let test_dir = unique_test_dir("rm-round-trip-config");
        fs::create_dir_all(&test_dir).unwrap();
        set_test_reviewed_migrations_store(&test_dir);
        let env_path = test_dir.join(".env");
        fs::write(&env_path, "KEY=value\n").unwrap();

        let unique_fp = format!("test-fp-{}-{}", std::process::id(), {
            use std::time::{SystemTime, UNIX_EPOCH};
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        });

        Config::remember_reviewed_migration_entries(&env_path, [unique_fp.clone()]).unwrap();
        let fps = Config::reviewed_migration_entry_fingerprints(&env_path).unwrap();
        Config::forget_reviewed_migration_entries(&env_path, [unique_fp.clone()]).unwrap();
        let _ = fs::remove_dir_all(&test_dir);

        assert!(
            fps.contains(&unique_fp),
            "fingerprint should be retrievable after remember()"
        );
    }

    #[test]
    fn forget_reviewed_migration_entries_removes_fingerprint() {
        let test_dir = unique_test_dir("rm-forget-removes");
        fs::create_dir_all(&test_dir).unwrap();
        set_test_reviewed_migrations_store(&test_dir);
        let env_path = test_dir.join(".env");
        fs::write(&env_path, "KEY=value\n").unwrap();

        let fp = "test-forget-fingerprint";
        Config::remember_reviewed_migration_entries(&env_path, [fp.to_string()]).unwrap();

        let before = Config::reviewed_migration_entry_fingerprints(&env_path).unwrap();
        assert!(
            before.contains(fp),
            "fingerprint should be present after remember()"
        );

        Config::forget_reviewed_migration_entries(&env_path, [fp.to_string()]).unwrap();

        let after = Config::reviewed_migration_entry_fingerprints(&env_path).unwrap();
        let _ = fs::remove_dir_all(&test_dir);

        assert!(
            !after.contains(fp),
            "fingerprint should be absent after forget()"
        );
    }

    // ── Test for ReviewedMigrations::path (L927) ──────────────────────────────

    #[test]
    fn reviewed_migrations_path_is_some_and_points_to_expected_file() {
        let path = ReviewedMigrations::path();
        assert!(
            path.is_some(),
            "ReviewedMigrations::path() should return Some on this platform"
        );
        let p = path.unwrap();
        assert!(
            p.to_string_lossy().contains("pw-env"),
            "path should contain 'pw-env', got: {}",
            p.display()
        );
        assert!(
            p.to_string_lossy().ends_with("reviewed-migrations.json"),
            "path should end with reviewed-migrations.json, got: {}",
            p.display()
        );
    }

    #[cfg(unix)]
    #[test]
    fn state_files_are_written_with_restricted_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("test-state.json");
        write_private_file(&path, "{}").unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "state file should be owner-only (0o600)");
    }

    #[test]
    fn approval_hashes_are_bounded_per_project() {
        let temp_dir = TempDir::new().unwrap();
        let store_path = temp_dir.path().join("secret-fetches.json");
        let project_path = Path::new("/test/project");

        let mut approvals = ApprovedSecretFetches::default();
        // Insert more hashes than the limit (10)
        for i in 0..15 {
            approvals.approve_hash(project_path, format!("hash_{:02}", i));
        }
        approvals.save_to_path(&store_path).unwrap();

        let loaded = ApprovedSecretFetches::load_from_path(&store_path).unwrap();
        let hashes = loaded.approved_hashes(project_path);
        assert!(
            hashes.len() <= 10,
            "should limit hashes to 10 per project, got {}",
            hashes.len()
        );
    }

    #[cfg(unix)]
    #[test]
    fn project_override_symlink_is_rejected() {
        let temp_dir = TempDir::new().unwrap();
        let real_file = temp_dir.path().join("real.toml");
        fs::write(&real_file, "backend = \"op\"\n").unwrap();
        let symlink_path = temp_dir.path().join(".pw-env.toml");
        std::os::unix::fs::symlink(&real_file, &symlink_path).unwrap();

        let result = ProjectDirectoryOverride::load_if_approved(&symlink_path).unwrap();
        assert!(
            result.is_none(),
            "symlinked .pw-env.toml should be rejected"
        );
    }
}
