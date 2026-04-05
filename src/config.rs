use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use tracing::debug;

const PROJECT_OVERRIDE_FILE_NAME: &str = ".pw-env.toml";

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    #[serde(default)]
    pub defaults: Defaults,
    #[serde(default)]
    pub log: LogConfig,
    #[serde(default)]
    pub projects: Vec<ProjectOverride>,
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

impl Config {
    pub fn config_path() -> PathBuf {
        // Prefer XDG_CONFIG_HOME, then ~/.config (Unix convention for CLI tools)
        if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
            return PathBuf::from(xdg).join("pw-manager-env").join("config.toml");
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
                projects: vec![],
            })
        }
    }

    pub fn load_for_dir(dir: &Path) -> Result<Self> {
        let mut config = Self::load()?;

        if let Some((project_dir, override_path)) = Self::project_override_file(dir) {
            if let Some(local_override) = ProjectDirectoryOverride::load_if_approved(&override_path)? {
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

        if !path.exists() {
            return Ok(Self::default());
        }

        let contents = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read approval store from {}", path.display()))?;
        serde_json::from_str(&contents)
            .with_context(|| format!("Failed to parse approval store from {}", path.display()))
    }

    fn save(&self) -> Result<()> {
        let Some(path) = Self::path() else {
            return Ok(());
        };

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create state directory {}", parent.display()))?;
        }

        let contents = serde_json::to_string_pretty(self)
            .context("Failed to serialize approval store")?;
        std::fs::write(&path, contents)
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

    fn path() -> Option<PathBuf> {
        dirs::state_dir()
            .or_else(dirs::data_local_dir)
            .map(|dir| dir.join("pw-manager-env").join("approved-project-configs.json"))
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

    #[test]
    fn test_default_config() {
        let config = Config {
            defaults: Defaults::default(),
            log: LogConfig::default(),
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
        assert_eq!(local_override.op.and_then(|op| op.vault), Some("Work".to_string()));
    }
}
