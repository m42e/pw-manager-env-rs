use anyhow::Result;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

use crate::backend::{self, ResolveContext};
use crate::config::Config;
use crate::env_file::{EntryKind, EnvEntry, EnvFile};

/// Walk up from `dir` to find a `.git` directory, returning the containing folder.
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

/// Detect the project name from the git root folder name, falling back to `dir`'s name.
pub fn detect_project_name(dir: &Path) -> Option<String> {
    find_git_root(dir)
        .and_then(|root| root.file_name().map(|n| n.to_string_lossy().into_owned()))
        .or_else(|| dir.file_name().map(|n| n.to_string_lossy().into_owned()))
}

/// Resolve all entries from an .env file to their secret values.
/// Returns a map of KEY -> resolved_value.
pub fn resolve_env_file(
    env_file: &EnvFile,
    config: &Config,
    dir: &Path,
) -> Result<BTreeMap<String, String>> {
    let mut resolved = BTreeMap::new();
    let entries = env_file.resolvable_entries();

    if entries.is_empty() {
        debug!("No resolvable entries found in .env file");
        return Ok(resolved);
    }

    Config::ensure_secret_fetch_approved(&env_file.path)?;

    let project = detect_project_name(dir);
    debug!("Detected project name: {:?}", project);

    let default_backend_name = config.effective_backend(dir);
    info!(
        "Resolving {} entries using default backend '{}'",
        entries.len(),
        default_backend_name
    );

    // Group entries by which backend will handle them
    let mut op_entries: Vec<&EnvEntry> = Vec::new();
    let mut bw_entries: Vec<&EnvEntry> = Vec::new();
    let mut default_entries: Vec<&EnvEntry> = Vec::new();

    for entry in &entries {
        match &entry.kind {
            EntryKind::OpReference(_) => op_entries.push(entry),
            EntryKind::BwReference(_) => bw_entries.push(entry),
            EntryKind::Empty => default_entries.push(entry),
            EntryKind::Plaintext(_) => {} // skip plaintext, not resolvable
        }
    }

    // Resolve op:// references
    if !op_entries.is_empty() {
        let backend = backend::create_backend("op")?;
        let ctx = ResolveContext {
            dir,
            config,
            project: project.clone(),
        };
        for entry in &op_entries {
            let reference = match &entry.kind {
                EntryKind::OpReference(r) => Some(r.as_str()),
                _ => None,
            };
            match backend.resolve(&entry.key, reference, &ctx) {
                Ok(value) => {
                    info!("Resolved {} via 1Password", entry.key);
                    resolved.insert(entry.key.clone(), value);
                }
                Err(e) => {
                    warn!("Failed to resolve {} via 1Password: {e}", entry.key);
                }
            }
        }
    }

    // Resolve bw:// references
    if !bw_entries.is_empty() {
        let backend = backend::create_backend("bw")?;
        let ctx = ResolveContext {
            dir,
            config,
            project: project.clone(),
        };
        for entry in &bw_entries {
            let reference = match &entry.kind {
                EntryKind::BwReference(r) => Some(r.as_str()),
                _ => None,
            };
            match backend.resolve(&entry.key, reference, &ctx) {
                Ok(value) => {
                    info!("Resolved {} via Bitwarden", entry.key);
                    resolved.insert(entry.key.clone(), value);
                }
                Err(e) => {
                    warn!("Failed to resolve {} via Bitwarden: {e}", entry.key);
                }
            }
        }
    }

    // Resolve empty entries via the default backend
    if !default_entries.is_empty() {
        // For GPG backend, resolve all at once since it decrypts the whole file
        if default_backend_name == "gpg" {
            let ctx = ResolveContext {
                dir,
                config,
                project: project.clone(),
            };
            match crate::backend::gpg::GpgBackend::resolve_all(&ctx) {
                Ok(all_values) => {
                    for entry in &default_entries {
                        if let Some(value) = all_values.get(&entry.key) {
                            info!("Resolved {} via GPG", entry.key);
                            resolved.insert(entry.key.clone(), value.clone());
                        } else {
                            warn!("Key '{}' not found in GPG encrypted file", entry.key);
                        }
                    }
                }
                Err(e) => {
                    warn!("Failed to decrypt GPG file: {e}");
                }
            }
        } else {
            let backend = backend::create_backend(default_backend_name)?;
            let ctx = ResolveContext {
                dir,
                config,
                project: project.clone(),
            };
            for entry in &default_entries {
                match backend.resolve(&entry.key, None, &ctx) {
                    Ok(value) => {
                        info!("Resolved {} via {}", entry.key, backend.name());
                        resolved.insert(entry.key.clone(), value);
                    }
                    Err(e) => {
                        warn!(
                            "Failed to resolve {} via {}: {e}",
                            entry.key,
                            backend.name()
                        );
                    }
                }
            }
        }
    }

    Ok(resolved)
}
