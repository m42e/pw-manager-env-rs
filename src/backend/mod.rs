pub mod bw;
pub mod gpg;
pub mod op;

use anyhow::Result;
use std::path::Path;

use crate::config::Config;

pub const PROJECT_FIELD_NAME: &str = "project";
pub const MIGRATED_FROM_FIELD_NAME: &str = "migrated_from";

/// Context passed to backend operations for resolving secrets.
pub struct ResolveContext<'a> {
    pub dir: &'a Path,
    pub config: &'a Config,
    /// Auto-detected or configured project name (for disambiguating multiple matches).
    pub project: Option<String>,
}

/// Context passed to backend operations for storing secrets.
pub struct StoreContext<'a> {
    pub dir: &'a Path,
    pub config: &'a Config,
    pub project: Option<String>,
}

impl StoreContext<'_> {
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
