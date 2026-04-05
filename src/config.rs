use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use tracing::debug;

const PROJECT_OVERRIDE_FILE_NAME: &str = ".pw-env.toml";

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

#[derive(Debug, Clone, Deserialize, Serialize)]
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
            op: OpConfig::default(),
            bw: BwConfig::default(),
            gpg: GpgConfig::default(),
        }
    }
}

fn default_backend() -> String {
    "op".to_string()
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

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct BwConfig {
    #[serde(default)]
    pub folder: Option<String>,
    #[serde(default)]
    pub organization: Option<String>,
    /// Item name to look up keys as fields (if set, keys are resolved as custom fields of this item)
    #[serde(default)]
    pub item: Option<String>,
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
    dirs::state_dir()
        .or_else(|| dirs::data_local_dir())
        .map(|d| {
            d.join("pw-manager-env")
                .join("pw-env.log")
                .to_string_lossy()
                .into_owned()
        })
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ProjectOverride {
    pub path: String,
    #[serde(default)]
    pub backend: Option<String>,
    #[serde(default)]
    pub op: Option<OpConfig>,
    #[serde(default)]
    pub bw: Option<BwConfig>,
    #[serde(default)]
    pub gpg: Option<GpgConfig>,
    /// Specific item name in the password store for this project
    #[serde(default)]
    pub item: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
struct ProjectDirectoryOverride {
    #[serde(default)]
    pub backend: Option<String>,
    #[serde(default)]
    pub op: Option<OpConfig>,
    #[serde(default)]
    pub bw: Option<BwConfig>,
    #[serde(default)]
    pub gpg: Option<GpgConfig>,
    #[serde(default)]
    pub item: Option<String>,
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
        // Prefer XDG_CONFIG_HOME, then ~/.config (Unix convention for CLI tools)
        if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
            return PathBuf::from(xdg)
                .join("pw-manager-env")
                .join("config.toml");
        }
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("~"))
            .join(".config")
            .join("pw-manager-env")
            .join("config.toml")
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

        if let Some((project_dir, override_path)) = Self::project_override_file(dir) {
            if let Some(local_override) =
                ProjectDirectoryOverride::load_if_approved(&override_path)?
            {
                config.projects.push(ProjectOverride {
                    path: project_dir.to_string_lossy().into_owned(),
                    backend: local_override.backend,
                    op: local_override.op,
                    bw: local_override.bw,
                    gpg: local_override.gpg,
                    item: local_override.item,
                });
            }
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
        if !io::stdin().is_terminal() {
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

        eprintln!("Credential fetch approval required for project {}", project_path.display());
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
        io::stdin().read_line(&mut input)?;
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
        self.projects
            .iter()
            .filter_map(|project| {
                let project_path = expand_path(&project.path);
                if dir.starts_with(&project_path) {
                    Some((project_path.components().count(), project))
                } else {
                    None
                }
            })
            .max_by_key(|(depth, _)| *depth)
            .map(|(_, project)| project)
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
        if let Some(proj) = self.project_for(dir) {
            if let Some(ref item) = proj.item {
                return Some(item.as_str());
            }
        }
        // Check the backend-specific default item
        match self.effective_backend(dir) {
            "op" => self.effective_op(dir).item.as_deref(),
            "bw" => self.effective_bw(dir).item.as_deref(),
            _ => None,
        }
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

        if !io::stdin().is_terminal() {
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
        io::stdin().read_line(&mut input)?;
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
        std::fs::write(path, contents)
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
        dirs::state_dir().or_else(dirs::data_local_dir).map(|dir| {
            dir.join("pw-manager-env")
                .join("approved-project-configs.json")
        })
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
        std::fs::write(path, contents).with_context(|| {
            format!(
                "Failed to write secret fetch approval store to {}",
                path.display()
            )
        })
    }

    fn approve_hash(&mut self, project_path: &Path, env_hash: String) {
        self.approved_env_hashes
            .entry(normalize_path(project_path))
            .or_default()
            .insert(env_hash);
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
        dirs::state_dir().or_else(dirs::data_local_dir).map(|dir| {
            dir.join("pw-manager-env")
                .join("approved-secret-fetches.json")
        })
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
        std::fs::write(path, contents).with_context(|| {
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
        dirs::state_dir()
            .or_else(dirs::data_local_dir)
            .map(|dir| dir.join("pw-manager-env").join("reviewed-migrations.json"))
    }
}

fn expand_path(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(path)
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

fn resolve_secret_fetch_target(path: &Path) -> Result<(PathBuf, PathBuf)> {
    let env_path = if path.is_dir() {
        path.join(".env")
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
    format!("{digest:x}")
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

[op]
vault = "Work"
"#;

        let local_override: ProjectDirectoryOverride = toml::from_str(toml_str).unwrap();
        assert_eq!(local_override.backend.as_deref(), Some("op"));
        assert_eq!(local_override.item.as_deref(), Some("service-a-env"));
        assert_eq!(
            local_override.op.and_then(|op| op.vault),
            Some("Work".to_string())
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
        assert_eq!(loaded.approved_hashes(&project_dir), BTreeSet::from([env_hash]));

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
        fs::write(&override_path, "backend = \"op\"\n[op]\nvault = \"Work\"\n").unwrap();

        let parsed = validate_project_override(&override_path).unwrap();
        assert_eq!(parsed.backend.as_deref(), Some("op"));
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

    fn unique_test_dir(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("pw-env-{name}-{}-{nonce}", std::process::id()))
    }
}
