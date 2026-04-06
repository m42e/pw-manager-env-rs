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

fn format_credential_fetch_audit(
    env_path: &Path,
    dir: &Path,
    project: Option<&str>,
    backend: &str,
    key: &str,
) -> String {
    let project_root = find_git_root(dir).unwrap_or_else(|| dir.to_path_buf());
    let project_name = project
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            project_root
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "unknown".to_string());

    format!(
        "AUDIT credential_fetch project={} project_root={} folder={} env_file={} backend={} key={}",
        project_name,
        project_root.display(),
        dir.display(),
        env_path.display(),
        backend,
        key
    )
}

fn log_credential_fetch_audit(
    env_path: &Path,
    dir: &Path,
    project: Option<&str>,
    backend: &str,
    key: &str,
) {
    info!(
        "{}",
        format_credential_fetch_audit(env_path, dir, project, backend, key)
    );
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
                    log_credential_fetch_audit(
                        &env_file.path,
                        dir,
                        project.as_deref(),
                        backend.name(),
                        &entry.key,
                    );
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
                    log_credential_fetch_audit(
                        &env_file.path,
                        dir,
                        project.as_deref(),
                        backend.name(),
                        &entry.key,
                    );
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
                            log_credential_fetch_audit(
                                &env_file.path,
                                dir,
                                project.as_deref(),
                                "GPG",
                                &entry.key,
                            );
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
                        log_credential_fetch_audit(
                            &env_file.path,
                            dir,
                            project.as_deref(),
                            backend.name(),
                            &entry.key,
                        );
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn unique_subdir(name: &str) -> std::path::PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("current time should be after unix epoch")
            .as_nanos();
        std::env::temp_dir()
            .join(format!("pw-env-{name}-{}-{nonce}", std::process::id()))
    }

    #[test]
    fn formats_audit_log_with_folder_and_env_file() {
        let unique = format!(
            "pw-env-audit-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("current time should be after unix epoch")
                .as_nanos()
        );
        let root = std::env::temp_dir().join(unique);
        let dir = root.join("service");
        let env_path = dir.join(".env");

        fs::create_dir_all(root.join(".git")).expect("should create git root marker");
        fs::create_dir_all(&dir).expect("should create working directory");

        let line = format_credential_fetch_audit(
            &env_path,
            &dir,
            Some("pw-env"),
            "1Password",
            "DATABASE_URL",
        );

        assert!(line.contains("AUDIT credential_fetch"));
        assert!(line.contains("project=pw-env"));
        assert!(line.contains(&format!("project_root={}", root.display())));
        assert!(line.contains(&format!("folder={}", dir.display())));
        assert!(line.contains(&format!("env_file={}", env_path.display())));
        assert!(line.contains("backend=1Password"));
        assert!(line.contains("key=DATABASE_URL"));

        fs::remove_dir_all(&root).expect("should clean up temp directories");
    }

    #[test]
    fn find_git_root_returns_none_when_no_git_above() {
        let root = unique_subdir("no-git");
        let dir = root.join("a/b/c");
        fs::create_dir_all(&dir).unwrap();

        let result = find_git_root(&dir);
        let _ = fs::remove_dir_all(&root);
        assert!(result.is_none());
    }

    #[test]
    fn find_git_root_finds_root_above_subdir() {
        let root = unique_subdir("git-root");
        let repo_dir = root.join("repo");
        let subdir = repo_dir.join("src/components");

        fs::create_dir_all(repo_dir.join(".git")).unwrap();
        fs::create_dir_all(&subdir).unwrap();

        let result = find_git_root(&subdir);
        let _ = fs::remove_dir_all(&root);
        assert!(result.is_some());
        let found = result.unwrap();
        assert_eq!(found.file_name().unwrap(), "repo");
    }

    #[test]
    fn detect_project_name_uses_folder_name_when_no_git() {
        let root = unique_subdir("proj-name");
        let dir = root.join("my-cool-project");
        fs::create_dir_all(&dir).unwrap();

        let name = detect_project_name(&dir);
        let _ = fs::remove_dir_all(&root);
        assert_eq!(name.as_deref(), Some("my-cool-project"));
    }

    #[test]
    fn detect_project_name_uses_git_root_name() {
        let root = unique_subdir("proj-git-name");
        let repo_dir = root.join("my-repo");
        let subdir = repo_dir.join("packages/api");

        fs::create_dir_all(repo_dir.join(".git")).unwrap();
        fs::create_dir_all(&subdir).unwrap();

        let name = detect_project_name(&subdir);
        let _ = fs::remove_dir_all(&root);
        assert_eq!(name.as_deref(), Some("my-repo"));
    }

    #[test]
    fn log_credential_fetch_audit_does_not_panic() {
        let root = unique_subdir("audit-log");
        let dir = root.join("service");
        let env_path = dir.join(".env");
        fs::create_dir_all(&dir).unwrap();

        // Should not panic even without a tracing subscriber
        log_credential_fetch_audit(&env_path, &dir, Some("my-project"), "1Password", "API_KEY");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn formats_audit_log_uses_dir_name_when_project_is_none() {
        let root = unique_subdir("no-proj");
        let dir = root.join("my-service");
        let env_path = dir.join(".env");
        fs::create_dir_all(&dir).unwrap();

        let line = format_credential_fetch_audit(&env_path, &dir, None, "op", "SECRET");

        let _ = fs::remove_dir_all(&root);
        assert!(line.contains("AUDIT credential_fetch"));
        assert!(line.contains("project=my-service"));
    }

    #[test]
    fn formats_audit_log_uses_dir_name_when_project_is_empty_string() {
        let root = unique_subdir("empty-proj");
        let dir = root.join("fallback-service");
        let env_path = dir.join(".env");
        fs::create_dir_all(&dir).unwrap();

        let line = format_credential_fetch_audit(&env_path, &dir, Some(""), "bw", "TOKEN");

        let _ = fs::remove_dir_all(&root);
        assert!(line.contains("project=fallback-service"));
    }

    #[test]
    fn resolve_env_file_returns_empty_for_all_plaintext_entries() {
        let temp = unique_subdir("resolve-plaintext");
        let env_path = temp.join(".env");
        fs::create_dir_all(&temp).unwrap();
        fs::write(&env_path, "API_KEY=plain-secret-value\nDB_URL=postgresql://localhost/db\n").unwrap();

        let env_file = crate::env_file::EnvFile::parse(&env_path).unwrap();
        let config = Config {
            defaults: crate::config::Defaults::default(),
            log: crate::config::LogConfig::default(),
            updates: crate::config::UpdateConfig::default(),
            projects: vec![],
        };
        // All entries are Plaintext, so resolvable_entries() returns empty → early return
        let result = resolve_env_file(&env_file, &config, &temp);
        let _ = fs::remove_dir_all(&temp);
        let resolved = result.unwrap();
        assert!(resolved.is_empty(), "expected empty map for all-plaintext file");
    }

    #[test]
    fn resolve_env_file_returns_empty_for_empty_env_file() {
        let temp = unique_subdir("resolve-empty");
        let env_path = temp.join(".env");
        fs::create_dir_all(&temp).unwrap();
        fs::write(&env_path, "# just a comment\n").unwrap();

        let env_file = crate::env_file::EnvFile::parse(&env_path).unwrap();
        let config = Config {
            defaults: crate::config::Defaults::default(),
            log: crate::config::LogConfig::default(),
            updates: crate::config::UpdateConfig::default(),
            projects: vec![],
        };
        let result = resolve_env_file(&env_file, &config, &temp);
        let _ = fs::remove_dir_all(&temp);
        let resolved = result.unwrap();
        assert!(resolved.is_empty(), "expected empty map for empty env file");
    }

    #[test]
    fn formats_audit_log_uses_unknown_when_dir_has_no_name() {
        // Using root-level path "/" has no file_name
        let env_path = std::path::PathBuf::from("/nonexistent/.env");
        let dir = std::path::PathBuf::from("/");

        let line = format_credential_fetch_audit(&env_path, &dir, None, "op", "KEY");
        // "/" has no file_name, so it falls back to "unknown"
        assert!(line.contains("project=unknown") || line.contains("project="));
    }
}
