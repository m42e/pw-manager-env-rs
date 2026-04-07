pub mod bw;
pub mod gpg;
pub mod op;

use anyhow::Result;
use std::path::Path;

use crate::config::Config;

pub const CREATED_WITH_FIELD_NAME: &str = "created-with";
pub const PROJECT_FIELD_NAME: &str = "project";
pub const MIGRATED_FROM_FIELD_NAME: &str = "migrated_from";
pub const REPOSITORY_FIELD_NAME: &str = "repository";

/// Single shared mutex for all mock PATH manipulations in tests.
/// All backend mock helpers (bw, gpg, op) must hold this lock while modifying PATH
/// to prevent cross-module races when running tests in parallel.
#[cfg(all(test, unix))]
pub(crate) static MOCK_PATH_MUTEX: std::sync::LazyLock<std::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(()));

/// Context passed to backend operations for resolving secrets.
pub struct ResolveContext<'a> {
    pub dir: &'a Path,
    pub config: &'a Config,
    /// Auto-detected or configured project name (for disambiguating multiple matches).
    pub project: Option<String>,
    /// Selected git remote URL for repository-aware disambiguation.
    pub repository: Option<String>,
}

/// Context passed to backend operations for storing secrets.
pub struct StoreContext<'a> {
    pub dir: &'a Path,
    pub config: &'a Config,
    pub project: Option<String>,
    pub repository: Option<String>,
}

impl StoreContext<'_> {
    pub fn created_with(&self) -> String {
        format!("pw-env ({})", env!("CARGO_PKG_VERSION"))
    }

    pub fn migrated_from(&self) -> String {
        self.dir.display().to_string()
    }
}

/// Trait for password manager backends.
pub trait Backend {
    /// Resolve a key to its secret value.
    /// For reference-style values (e.g. `op://vault/item/field`), the reference is passed as-is.
    /// For key-only lookups, the key name is passed and the backend uses its config to find it.
    fn resolve(&self, key: &str, reference: Option<&str>, ctx: &ResolveContext) -> Result<String>;

    /// Store a key-value pair in the password manager.
    fn store(&self, key: &str, value: &str, ctx: &StoreContext) -> Result<()>;

    /// Check if a key exists in the password manager.
    fn has(&self, key: &str, ctx: &ResolveContext) -> Result<bool>;

    /// Return the name of this backend (for logging).
    fn name(&self) -> &str;
}

/// Create a backend instance by name.
pub fn create_backend(name: &str) -> Result<Box<dyn Backend>> {
    match name {
        "op" => Ok(Box::new(op::OpBackend)),
        "bw" => Ok(Box::new(bw::BwBackend)),
        "gpg" => Ok(Box::new(gpg::GpgBackend)),
        other => anyhow::bail!("Unknown backend: {other}. Supported: op, bw, gpg"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, Defaults, LogConfig, UpdateConfig};

    fn make_config() -> Config {
        Config {
            defaults: Defaults::default(),
            log: LogConfig::default(),
            updates: UpdateConfig::default(),
            projects: vec![],
        }
    }

    #[test]
    fn test_create_backend_op() {
        let backend = create_backend("op").unwrap();
        assert_eq!(backend.name(), "1Password");
    }

    #[test]
    fn test_create_backend_bw() {
        let backend = create_backend("bw").unwrap();
        assert_eq!(backend.name(), "Bitwarden");
    }

    #[test]
    fn test_create_backend_gpg() {
        let backend = create_backend("gpg").unwrap();
        assert_eq!(backend.name(), "GPG");
    }

    #[test]
    fn test_create_backend_unknown_returns_error() {
        let result = create_backend("unknown");
        assert!(result.is_err());
        let err_str = format!("{}", result.err().unwrap());
        assert!(err_str.contains("Unknown backend"));
    }

    #[test]
    fn test_store_context_migrated_from() {
        let config = make_config();
        let ctx = StoreContext {
            dir: std::path::Path::new("/some/project/dir"),
            config: &config,
            project: None,
            repository: None,
        };
        assert_eq!(ctx.migrated_from(), "/some/project/dir");
    }

    #[test]
    fn test_store_context_created_with() {
        let config = make_config();
        let ctx = StoreContext {
            dir: std::path::Path::new("/some/project/dir"),
            config: &config,
            project: None,
            repository: None,
        };
        assert_eq!(
            ctx.created_with(),
            format!("pw-env ({})", env!("CARGO_PKG_VERSION"))
        );
    }
}
