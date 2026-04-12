use anyhow::Result;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;
use tracing::{debug, info, warn};

use crate::backend::bw::BwBackend;
use crate::backend::{self, ResolveContext};
use crate::cache::{SecretCacheKey, SecretValueCache};
use crate::config::Config;
use crate::env_file::{EntryKind, EnvEntry, EnvFile};
use crate::progress::ActivitySpinner;

/// Walk up from `dir` to find a `.git` directory, returning the containing folder.
pub(crate) fn find_git_root(dir: &Path) -> Option<PathBuf> {
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

fn find_git_dir(dir: &Path) -> Option<PathBuf> {
    let git_marker = find_git_root(dir)?.join(".git");

    if git_marker.is_dir() {
        return Some(git_marker);
    }

    let gitdir_contents = std::fs::read_to_string(&git_marker).ok()?;
    let gitdir = gitdir_contents.strip_prefix("gitdir:")?.trim();
    let gitdir_path = Path::new(gitdir);

    Some(if gitdir_path.is_absolute() {
        gitdir_path.to_path_buf()
    } else {
        git_marker.parent()?.join(gitdir_path)
    })
}

fn current_git_branch(git_dir: &Path) -> Option<String> {
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let reference = head.strip_prefix("ref:")?.trim();
    reference.strip_prefix("refs/heads/").map(ToOwned::to_owned)
}

fn parse_git_config(contents: &str) -> (BTreeMap<String, String>, BTreeMap<String, String>) {
    let mut remotes = BTreeMap::new();
    let mut branch_remotes = BTreeMap::new();
    let mut current_remote: Option<String> = None;
    let mut current_branch: Option<String> = None;

    for raw_line in contents.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }

        if matches!(line.as_bytes().first(), Some(b'#' | b';')) {
            continue;
        }

        if let Some(section) = line
            .strip_prefix('[')
            .and_then(|value| value.strip_suffix(']'))
        {
            current_remote = None;
            current_branch = None;

            if let Some(name) = section
                .strip_prefix("remote \"")
                .and_then(|value| value.strip_suffix('"'))
            {
                current_remote = Some(name.to_string());
            } else if let Some(name) = section
                .strip_prefix("branch \"")
                .and_then(|value| value.strip_suffix('"'))
            {
                current_branch = Some(name.to_string());
            }
            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            continue;
        };

        let key = key.trim();
        let value = value.trim();
        if key == "url"
            && let Some(remote) = current_remote.as_ref()
        {
            remotes.entry(remote.clone()).or_insert(value.to_string());
        } else if key == "remote"
            && let Some(branch) = current_branch.as_ref()
        {
            branch_remotes.insert(branch.clone(), value.to_string());
        }
    }

    (remotes, branch_remotes)
}

fn select_git_remote_name(
    remotes: &BTreeMap<String, String>,
    configured_remote: Option<&str>,
) -> Option<String> {
    if remotes.len() == 1 {
        remotes.keys().next().cloned()
    } else if remotes.contains_key("origin") {
        Some("origin".to_string())
    } else {
        configured_remote
            .filter(|remote| remotes.contains_key(*remote))
            .map(ToOwned::to_owned)
    }
}

fn normalize_git_remote_url(url: &str) -> Option<String> {
    let trimmed = url.trim();
    if trimmed.is_empty()
        || trimmed.starts_with("file://")
        || trimmed.starts_with('/')
        || trimmed.starts_with("./")
        || trimmed.starts_with("../")
        || trimmed.starts_with('~')
    {
        return None;
    }

    let windows_drive = trimmed.len() >= 3
        && trimmed.as_bytes()[1] == b':'
        && trimmed.as_bytes()[2] == b'/'
        && trimmed.as_bytes()[0].is_ascii_alphabetic();
    if windows_drive {
        return None;
    }

    if trimmed.contains("://") || trimmed.starts_with("git@") {
        return Some(trimmed.to_string());
    }

    let (host, path) = trimmed.split_once(':')?;
    if host.contains('/') || host.is_empty() {
        return None;
    }
    if host.len() == 1 && path.starts_with('/') && !host.as_bytes()[0].is_ascii_alphabetic() {
        return None;
    }

    Some(trimmed.to_string())
}

pub(crate) fn detect_repository_remote(dir: &Path) -> Option<String> {
    let git_dir = find_git_dir(dir)?;
    let config_contents = std::fs::read_to_string(git_dir.join("config")).ok()?;
    let (remotes, branch_remotes) = parse_git_config(&config_contents);
    let current_branch = current_git_branch(&git_dir);
    let configured_remote = current_branch
        .as_deref()
        .and_then(|branch| branch_remotes.get(branch))
        .map(String::as_str);
    let selected_remote = select_git_remote_name(&remotes, configured_remote)?;
    normalize_git_remote_url(remotes.get(&selected_remote)?.as_str())
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

fn cache_entry_kind(entry: &EnvEntry) -> &'static str {
    match entry.kind {
        EntryKind::Empty => "empty",
        EntryKind::OpReference(_) => "op-reference",
        EntryKind::BwReference(_) => "bw-reference",
        EntryKind::Plaintext(_) => "plaintext",
    }
}

fn cache_backend_config(config: &Config, dir: &Path, backend: &str) -> String {
    let serialized = match backend {
        "op" => serde_json::to_string(config.effective_op(dir)),
        "bw" => serde_json::to_string(config.effective_bw(dir)),
        "gpg" => serde_json::to_string(config.effective_gpg(dir)),
        _ => Ok("{}".to_string()),
    };

    serialized.unwrap_or_else(|_| "{}".to_string())
}

fn build_secret_cache_key(
    env_path: &Path,
    entry: &EnvEntry,
    backend: &str,
    ctx: &ResolveContext,
) -> SecretCacheKey {
    SecretCacheKey {
        env_path: env_path.to_string_lossy().into_owned(),
        backend: backend.to_string(),
        entry_key: entry.key.clone(),
        entry_kind: cache_entry_kind(entry).to_string(),
        raw_value: entry.raw_value.clone(),
        project: ctx.project.clone(),
        repository: ctx.repository.clone(),
        effective_item: match backend {
            "op" | "bw" => ctx.config.effective_item(ctx.dir).map(ToOwned::to_owned),
            _ => None,
        },
        backend_config: cache_backend_config(ctx.config, ctx.dir, backend),
    }
}

/// Resolve all entries from an .env file to their secret values.
/// Returns a map of KEY -> resolved_value.
/// When `config.effective_source_all(dir)` is true, plaintext (non-secret) values from the
/// `.env` file are also included in the returned map alongside the resolved secret values.
pub fn resolve_env_file(
    env_file: &EnvFile,
    config: &Config,
    dir: &Path,
) -> Result<BTreeMap<String, String>> {
    let started_at = Instant::now();
    let mut resolved = BTreeMap::new();
    let mut bitwarden_duration_ms = 0u128;
    let source_all = config.effective_source_all(dir);
    let entries = env_file.resolvable_entries();

    if entries.is_empty() && !source_all {
        debug!("No resolvable entries found in .env file");
        return Ok(resolved);
    }

    // When source_all is enabled but there are no resolvable (secret) entries,
    // skip backend resolution and just return the plaintext values.
    if entries.is_empty() {
        debug!("No resolvable entries found; returning plaintext values via source_all");
        for entry in env_file.entries() {
            if let EntryKind::Plaintext(value) = &entry.kind {
                resolved.insert(entry.key.clone(), value.clone());
            }
        }
        return Ok(resolved);
    }

    Config::ensure_secret_fetch_approved(&env_file.path)?;

    let project = detect_project_name(dir);
    let repository = detect_repository_remote(dir);
    debug!("Detected project name: {:?}", project);

    let ctx = ResolveContext {
        dir,
        config,
        project: project.clone(),
        repository: repository.clone(),
    };
    let mut secret_cache = SecretValueCache::load(config.effective_cache(dir));

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
        for entry in &op_entries {
            let cache_key = build_secret_cache_key(&env_file.path, entry, "op", &ctx);
            if let Some(value) = secret_cache.get(&cache_key) {
                debug!("Resolved {} via cached 1Password value", entry.key);
                resolved.insert(entry.key.clone(), value);
                continue;
            }

            let reference = match &entry.kind {
                EntryKind::OpReference(r) => Some(r.as_str()),
                _ => None,
            };
            match backend.resolve(&entry.key, reference, &ctx) {
                Ok(value) => {
                    info!("Resolved {} via 1Password", entry.key);
                    secret_cache.set(&cache_key, &value);
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

    let use_bitwarden_batch_for_defaults = default_backend_name == "bw";
    if use_bitwarden_batch_for_defaults {
        bw_entries.extend(default_entries.iter().copied());
    }

    // Resolve all Bitwarden-backed entries through the batch path.
    if !bw_entries.is_empty() {
        let bw_started_at = Instant::now();
        let mut uncached_bw_entries: Vec<&EnvEntry> = Vec::new();
        let mut bw_cache_keys: BTreeMap<String, SecretCacheKey> = BTreeMap::new();

        for entry in &bw_entries {
            let cache_key = build_secret_cache_key(&env_file.path, entry, "bw", &ctx);
            if let Some(value) = secret_cache.get(&cache_key) {
                debug!("Resolved {} via cached Bitwarden value", entry.key);
                resolved.insert(entry.key.clone(), value);
                continue;
            }

            bw_cache_keys.insert(entry.key.clone(), cache_key);
            uncached_bw_entries.push(entry);
        }

        if !uncached_bw_entries.is_empty() {
            // Collect all (key, reference) pairs for the batch call
            let batch_keys: Vec<(&str, Option<&str>)> = uncached_bw_entries
                .iter()
                .map(|entry| {
                    let reference = match &entry.kind {
                        EntryKind::BwReference(r) => Some(r.as_str()),
                        _ => None,
                    };
                    (entry.key.as_str(), reference)
                })
                .collect();

            let step_label = format!(
                "Bitwarden: resolving {} uncached entries",
                uncached_bw_entries.len()
            );
            let mut spinner = ActivitySpinner::new(step_label);

            let batch_results = BwBackend::resolve_batch(&batch_keys, &ctx);

            for (idx, entry) in uncached_bw_entries.iter().enumerate() {
                spinner.set_message(format!(
                    "Bitwarden: resolving {} ({}/{})",
                    entry.key,
                    idx + 1,
                    uncached_bw_entries.len()
                ));
                match batch_results.get(&entry.key) {
                    Some(Ok(value)) => {
                        spinner.set_message(format!(
                            "Bitwarden: resolved {} ({}/{})",
                            entry.key,
                            idx + 1,
                            uncached_bw_entries.len()
                        ));
                        info!("Resolved {} via Bitwarden", entry.key);
                        if let Some(cache_key) = bw_cache_keys.get(&entry.key) {
                            secret_cache.set(cache_key, value);
                        }
                        log_credential_fetch_audit(
                            &env_file.path,
                            dir,
                            project.as_deref(),
                            "Bitwarden",
                            &entry.key,
                        );
                        resolved.insert(entry.key.clone(), value.clone());
                    }
                    Some(Err(e)) => {
                        warn!("Failed to resolve {} via Bitwarden: {e}", entry.key);
                    }
                    None => {
                        warn!("No result for {} from Bitwarden batch resolve", entry.key);
                    }
                }
            }
            spinner.finish(format!(
                "Bitwarden: resolved {}/{} uncached entries",
                uncached_bw_entries
                    .iter()
                    .filter(|entry| resolved.contains_key(&entry.key))
                    .count(),
                uncached_bw_entries.len()
            ));
            bitwarden_duration_ms = bw_started_at.elapsed().as_millis();
        }
    }

    // Resolve empty entries via the default backend
    if !default_entries.is_empty() && !use_bitwarden_batch_for_defaults {
        // For GPG backend, resolve all at once since it decrypts the whole file
        if default_backend_name == "gpg" {
            let mut uncached_gpg_entries: Vec<&EnvEntry> = Vec::new();
            let mut gpg_cache_keys: BTreeMap<String, SecretCacheKey> = BTreeMap::new();

            for entry in &default_entries {
                let cache_key = build_secret_cache_key(&env_file.path, entry, "gpg", &ctx);
                if let Some(value) = secret_cache.get(&cache_key) {
                    debug!("Resolved {} via cached GPG value", entry.key);
                    resolved.insert(entry.key.clone(), value);
                    continue;
                }

                gpg_cache_keys.insert(entry.key.clone(), cache_key);
                uncached_gpg_entries.push(entry);
            }

            if uncached_gpg_entries.is_empty() {
                debug!("Resolved all GPG-backed entries from cache");
            } else {
                match crate::backend::gpg::GpgBackend::resolve_all(&ctx) {
                    Ok(all_values) => {
                        for entry in &uncached_gpg_entries {
                            if let Some(value) = all_values.get(&entry.key) {
                                info!("Resolved {} via GPG", entry.key);
                                if let Some(cache_key) = gpg_cache_keys.get(&entry.key) {
                                    secret_cache.set(cache_key, value);
                                }
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
            }
        } else {
            let backend = backend::create_backend(default_backend_name)?;
            let bitwarden_default_started_at =
                matches!(backend.name(), "Bitwarden").then(Instant::now);
            for entry in &default_entries {
                let cache_key =
                    build_secret_cache_key(&env_file.path, entry, default_backend_name, &ctx);
                if let Some(value) = secret_cache.get(&cache_key) {
                    debug!("Resolved {} via cached {} value", entry.key, backend.name());
                    resolved.insert(entry.key.clone(), value);
                    continue;
                }

                match backend.resolve(&entry.key, None, &ctx) {
                    Ok(value) => {
                        info!("Resolved {} via {}", entry.key, backend.name());
                        secret_cache.set(&cache_key, &value);
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
            if let Some(backend_started_at) = bitwarden_default_started_at {
                bitwarden_duration_ms =
                    bitwarden_duration_ms.saturating_add(backend_started_at.elapsed().as_millis());
            }
        }
    }

    // When source_all is enabled, include plaintext (non-secret) values in the output.
    // Secret-resolved entries take precedence; plaintext values fill in the rest.
    if source_all {
        for entry in env_file.entries() {
            if let EntryKind::Plaintext(value) = &entry.kind {
                resolved
                    .entry(entry.key.clone())
                    .or_insert_with(|| value.clone());
            }
        }
    }

    let total_duration_ms = started_at.elapsed().as_millis();
    debug!(
        total_entries = entries.len(),
        resolved_count = resolved.len(),
        unresolved_count = entries.len().saturating_sub(resolved.len()),
        total_duration_ms,
        bitwarden_duration_ms,
        bitwarden_share_percent = if total_duration_ms > 0 {
            (bitwarden_duration_ms * 100 / total_duration_ms) as u64
        } else {
            0
        },
        "Resolve env file finished"
    );

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
        std::env::temp_dir().join(format!("pw-env-{name}-{}-{nonce}", std::process::id()))
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
    fn find_git_dir_resolves_relative_gitdir_file() {
        let root = unique_subdir("gitdir-relative");
        let repo_dir = root.join("repo");
        let worktree_dir = repo_dir.join("worktree");
        let actual_git_dir = repo_dir.join(".git/worktrees/worktree");

        fs::create_dir_all(&worktree_dir).unwrap();
        fs::create_dir_all(&actual_git_dir).unwrap();
        fs::write(
            worktree_dir.join(".git"),
            "gitdir: ../.git/worktrees/worktree\n",
        )
        .unwrap();

        let result = find_git_dir(&worktree_dir);
        let _ = fs::remove_dir_all(&root);

        assert_eq!(
            result,
            Some(worktree_dir.join("../.git/worktrees/worktree"))
        );
    }

    #[test]
    fn find_git_dir_resolves_absolute_gitdir_file() {
        let root = unique_subdir("gitdir-absolute");
        let repo_dir = root.join("repo");
        let actual_git_dir = root.join("actual.git");

        fs::create_dir_all(&repo_dir).unwrap();
        fs::create_dir_all(&actual_git_dir).unwrap();
        fs::write(
            repo_dir.join(".git"),
            format!("gitdir: {}\n", actual_git_dir.display()),
        )
        .unwrap();

        let result = find_git_dir(&repo_dir);
        let _ = fs::remove_dir_all(&root);

        assert_eq!(result, Some(actual_git_dir));
    }

    #[test]
    fn find_git_dir_returns_none_for_malformed_gitdir_file() {
        let root = unique_subdir("gitdir-malformed");
        let repo_dir = root.join("repo");

        fs::create_dir_all(&repo_dir).unwrap();
        fs::write(repo_dir.join(".git"), "not-a-gitdir-line\n").unwrap();

        let result = find_git_dir(&repo_dir);
        let _ = fs::remove_dir_all(&root);

        assert_eq!(result, None);
    }

    #[test]
    fn current_git_branch_returns_none_for_detached_head() {
        let root = unique_subdir("git-branch-detached");
        let git_dir = root.join(".git");

        fs::create_dir_all(&git_dir).unwrap();
        fs::write(git_dir.join("HEAD"), "0123456789abcdef\n").unwrap();

        let result = current_git_branch(&git_dir);
        let _ = fs::remove_dir_all(&root);

        assert_eq!(result, None);
    }

    #[test]
    fn current_git_branch_returns_none_for_non_branch_reference() {
        let root = unique_subdir("git-branch-non-head");
        let git_dir = root.join(".git");

        fs::create_dir_all(&git_dir).unwrap();
        fs::write(git_dir.join("HEAD"), "ref: refs/tags/v1.0.0\n").unwrap();

        let result = current_git_branch(&git_dir);
        let _ = fs::remove_dir_all(&root);

        assert_eq!(result, None);
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
    fn detect_repository_remote_prefers_origin_when_multiple_remotes_exist() {
        let root = unique_subdir("remote-origin");
        let repo_dir = root.join("repo");
        let subdir = repo_dir.join("service");
        let git_dir = repo_dir.join(".git");

        fs::create_dir_all(&git_dir).unwrap();
        fs::create_dir_all(&subdir).unwrap();
        fs::write(git_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::write(
            git_dir.join("config"),
            "[remote \"backup\"]\n\turl = git@github.com:example/backup.git\n[remote \"origin\"]\n\turl = git@github.com:example/origin.git\n[branch \"main\"]\n\tremote = backup\n",
        )
        .unwrap();

        let remote = detect_repository_remote(&subdir);
        let _ = fs::remove_dir_all(&root);

        assert_eq!(remote.as_deref(), Some("git@github.com:example/origin.git"));
    }

    #[test]
    fn detect_repository_remote_uses_branch_remote_when_multiple_without_origin() {
        let root = unique_subdir("remote-branch");
        let repo_dir = root.join("repo");
        let subdir = repo_dir.join("service");
        let git_dir = repo_dir.join(".git");

        fs::create_dir_all(&git_dir).unwrap();
        fs::create_dir_all(&subdir).unwrap();
        fs::write(git_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::write(
            git_dir.join("config"),
            "[remote \"upstream\"]\n\turl = https://github.com/example/upstream.git\n[remote \"mirror\"]\n\turl = https://github.com/example/mirror.git\n[branch \"main\"]\n\tremote = upstream\n",
        )
        .unwrap();

        let remote = detect_repository_remote(&subdir);
        let _ = fs::remove_dir_all(&root);

        assert_eq!(
            remote.as_deref(),
            Some("https://github.com/example/upstream.git")
        );
    }

    #[test]
    fn detect_repository_remote_returns_none_for_local_path_remote() {
        let root = unique_subdir("remote-local");
        let repo_dir = root.join("repo");
        let subdir = repo_dir.join("service");
        let git_dir = repo_dir.join(".git");

        fs::create_dir_all(&git_dir).unwrap();
        fs::create_dir_all(&subdir).unwrap();
        fs::write(git_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::write(
            git_dir.join("config"),
            "[remote \"origin\"]\n\turl = ../local/repo.git\n[branch \"main\"]\n\tremote = origin\n",
        )
        .unwrap();

        let remote = detect_repository_remote(&subdir);
        let _ = fs::remove_dir_all(&root);

        assert!(remote.is_none());
    }

    #[test]
    fn parse_git_config_ignores_comments_and_malformed_sections() {
        let config = r#"
[remote "origin"]
  url = git@github.com:example/repo.git
[branch "main"]
  remote = origin
; comment line should be ignored
#[branch "shadow"]
branch "broken"]
  remote = backup
"#;

        let (remotes, branch_remotes) = parse_git_config(config);
        assert_eq!(remotes.len(), 1);
        assert_eq!(
            remotes.get("origin").map(String::as_str),
            Some("git@github.com:example/repo.git")
        );
        assert_eq!(branch_remotes.len(), 1);
        assert_eq!(
            branch_remotes.get("main").map(String::as_str),
            Some("backup")
        );
    }

    #[test]
    fn select_git_remote_name_handles_empty_and_single_remote_maps() {
        let empty = BTreeMap::new();
        assert_eq!(select_git_remote_name(&empty, Some("origin")), None);

        let mut single = BTreeMap::new();
        single.insert(
            "upstream".to_string(),
            "git@github.com:example/repo.git".to_string(),
        );
        assert_eq!(
            select_git_remote_name(&single, Some("origin")).as_deref(),
            Some("upstream")
        );
    }

    #[test]
    fn normalize_git_remote_url_rejects_local_and_windows_paths() {
        for candidate in [
            "",
            "file:///tmp/repo.git",
            "/tmp/repo.git",
            "./repo.git",
            "../repo.git",
            "~/repo.git",
            "C:/repo.git",
        ] {
            assert_eq!(
                normalize_git_remote_url(candidate),
                None,
                "candidate={candidate}"
            );
        }
    }

    #[test]
    fn normalize_git_remote_url_accepts_supported_remote_forms() {
        assert_eq!(
            normalize_git_remote_url("https://github.com/example/repo.git").as_deref(),
            Some("https://github.com/example/repo.git")
        );
        assert_eq!(
            normalize_git_remote_url("git@github.com:example/repo.git").as_deref(),
            Some("git@github.com:example/repo.git")
        );
        assert_eq!(
            normalize_git_remote_url("github.com:example/repo.git").as_deref(),
            Some("github.com:example/repo.git")
        );
    }

    #[test]
    fn normalize_git_remote_url_rejects_invalid_scp_like_hosts() {
        assert_eq!(normalize_git_remote_url("/github.com:repo.git"), None);
        assert_eq!(normalize_git_remote_url(":repo.git"), None);
    }

    #[test]
    fn normalize_git_remote_url_rejects_non_alphabetic_windows_drive_prefixes() {
        assert_eq!(normalize_git_remote_url("1:/repo.git"), None);
        assert_eq!(normalize_git_remote_url("_:/repo.git"), None);
    }

    #[test]
    fn normalize_git_remote_url_rejects_local_prefixes_even_with_protocol_like_content() {
        // Inputs that start with ./ ../ or ~ but also contain :// so they would
        // survive SCP/host:path fallback – only the prefix guard rejects them.
        assert_eq!(normalize_git_remote_url("./://x"), None);
        assert_eq!(normalize_git_remote_url("../://x"), None);
        assert_eq!(normalize_git_remote_url("~://x"), None);
    }

    #[test]
    fn normalize_git_remote_url_rejects_false_windows_drive_from_third_byte_slash() {
        // "ht://..." has [2]=='/' and [0] is alpha; the windows-drive guard
        // must NOT reject it (the && chain must stay intact).
        assert_eq!(
            normalize_git_remote_url("ht://example.com").as_deref(),
            Some("ht://example.com")
        );
    }

    #[test]
    fn normalize_git_remote_url_accepts_scheme_only_url() {
        // "://repo" has no scheme name but contains "://" – the protocol-OR
        // branch must still accept it; regression for || → && mutation.
        assert_eq!(
            normalize_git_remote_url("://repo").as_deref(),
            Some("://repo")
        );
    }

    #[test]
    fn normalize_git_remote_url_accepts_non_alpha_single_char_host_without_slash() {
        // "1:repo" is an SCP-style remote with a non-alphabetic single-char
        // host and a path that does NOT start with '/'.  The guard on line 151
        // must only reject when host.len()==1 AND path.starts_with('/') AND
        // !host[0].is_ascii_alphabetic().
        assert_eq!(
            normalize_git_remote_url("1:repo.git").as_deref(),
            Some("1:repo.git")
        );
    }

    #[test]
    fn cache_entry_kind_returns_expected_labels() {
        let mk_entry = |kind: EntryKind| EnvEntry {
            key: "K".to_string(),
            raw_value: "v".to_string(),
            trailing_comment: None,
            no_migrate: false,
            kind,
        };

        assert_eq!(cache_entry_kind(&mk_entry(EntryKind::Empty)), "empty");
        assert_eq!(
            cache_entry_kind(&mk_entry(EntryKind::OpReference("op://a/b/c".to_string()))),
            "op-reference"
        );
        assert_eq!(
            cache_entry_kind(&mk_entry(EntryKind::BwReference("bw://a/b".to_string()))),
            "bw-reference"
        );
        assert_eq!(
            cache_entry_kind(&mk_entry(EntryKind::Plaintext("x".to_string()))),
            "plaintext"
        );
    }

    #[test]
    fn cache_backend_config_serializes_known_backends_and_defaults_unknown() {
        let config = Config {
            defaults: crate::config::Defaults {
                op: crate::config::OpConfig {
                    item: Some("op-item".to_string()),
                    ..crate::config::OpConfig::default()
                },
                bw: crate::config::BwConfig {
                    item: Some("bw-item".to_string()),
                    ..crate::config::BwConfig::default()
                },
                gpg: crate::config::GpgConfig {
                    file_pattern: ".env.secrets.gpg".to_string(),
                    ..crate::config::GpgConfig::default()
                },
                ..crate::config::Defaults::default()
            },
            log: crate::config::LogConfig::default(),
            updates: crate::config::UpdateConfig::default(),
            projects: vec![],
        };
        let dir = Path::new("/tmp");

        let op = cache_backend_config(&config, dir, "op");
        let bw = cache_backend_config(&config, dir, "bw");
        let gpg = cache_backend_config(&config, dir, "gpg");
        let unknown = cache_backend_config(&config, dir, "unknown");

        assert!(op.contains("op-item"));
        assert!(bw.contains("bw-item"));
        assert!(gpg.contains(".env.secrets.gpg"));
        assert_eq!(unknown, "{}");
    }

    #[test]
    fn build_secret_cache_key_uses_effective_item_only_for_op_and_bw() {
        let entry = EnvEntry {
            key: "DATABASE_URL".to_string(),
            raw_value: "op://vault/item/field".to_string(),
            trailing_comment: None,
            no_migrate: false,
            kind: EntryKind::OpReference("op://vault/item/field".to_string()),
        };

        let op_config = Config {
            defaults: crate::config::Defaults {
                backend: "op".to_string(),
                op: crate::config::OpConfig {
                    item: Some("app-secrets".to_string()),
                    ..crate::config::OpConfig::default()
                },
                ..crate::config::Defaults::default()
            },
            log: crate::config::LogConfig::default(),
            updates: crate::config::UpdateConfig::default(),
            projects: vec![],
        };
        let dir = Path::new("/tmp");
        let op_ctx = ResolveContext {
            dir,
            config: &op_config,
            project: Some("proj".to_string()),
            repository: Some("git@github.com:example/repo.git".to_string()),
        };
        let op_key = build_secret_cache_key(Path::new("/tmp/.env"), &entry, "op", &op_ctx);
        assert_eq!(op_key.effective_item.as_deref(), Some("app-secrets"));

        let gpg_config = Config {
            defaults: crate::config::Defaults {
                backend: "gpg".to_string(),
                ..crate::config::Defaults::default()
            },
            log: crate::config::LogConfig::default(),
            updates: crate::config::UpdateConfig::default(),
            projects: vec![],
        };
        let gpg_ctx = ResolveContext {
            dir,
            config: &gpg_config,
            project: None,
            repository: None,
        };
        let gpg_key = build_secret_cache_key(Path::new("/tmp/.env"), &entry, "gpg", &gpg_ctx);
        assert_eq!(gpg_key.effective_item, None);
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
        fs::write(
            &env_path,
            "API_KEY=plain-secret-value\nDB_URL=postgresql://localhost/db\n",
        )
        .unwrap();

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
        assert!(
            resolved.is_empty(),
            "expected empty map for all-plaintext file"
        );
    }

    #[test]
    fn resolve_env_file_with_source_all_includes_plaintext_entries() {
        let temp = unique_subdir("resolve-source-all-plaintext");
        let env_path = temp.join(".env");
        fs::create_dir_all(&temp).unwrap();
        fs::write(&env_path, "LOG_LEVEL=debug\nAPP_ENV=production\n").unwrap();

        let env_file = crate::env_file::EnvFile::parse(&env_path).unwrap();
        let config = Config {
            defaults: crate::config::Defaults {
                source_all: true,
                ..crate::config::Defaults::default()
            },
            log: crate::config::LogConfig::default(),
            updates: crate::config::UpdateConfig::default(),
            projects: vec![],
        };
        let result = resolve_env_file(&env_file, &config, &temp);
        let _ = fs::remove_dir_all(&temp);
        let resolved = result.unwrap();
        assert_eq!(resolved.get("LOG_LEVEL").map(String::as_str), Some("debug"));
        assert_eq!(
            resolved.get("APP_ENV").map(String::as_str),
            Some("production")
        );
    }

    #[test]
    fn resolve_env_file_without_source_all_omits_plaintext_entries() {
        let temp = unique_subdir("resolve-no-source-all-plaintext");
        let env_path = temp.join(".env");
        fs::create_dir_all(&temp).unwrap();
        fs::write(&env_path, "LOG_LEVEL=debug\nAPP_ENV=production\n").unwrap();

        let env_file = crate::env_file::EnvFile::parse(&env_path).unwrap();
        let config = Config {
            defaults: crate::config::Defaults::default(), // source_all = false
            log: crate::config::LogConfig::default(),
            updates: crate::config::UpdateConfig::default(),
            projects: vec![],
        };
        let result = resolve_env_file(&env_file, &config, &temp);
        let _ = fs::remove_dir_all(&temp);
        let resolved = result.unwrap();
        assert!(
            resolved.is_empty(),
            "expected plaintext entries to be omitted when source_all is false"
        );
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
    fn formats_audit_log_ignores_empty_project_string_and_uses_dir_name() {
        // When project is Some(""), the empty string must be filtered out
        // and the git root / dir name used instead.
        // This kills the `delete !` mutation at line 39 (filter keeps empty names without !).
        let root = unique_subdir("empty-proj");
        let repo_dir = root.join("my-repo");
        let dir = repo_dir.join("api");
        let env_path = dir.join(".env");

        fs::create_dir_all(repo_dir.join(".git")).unwrap();
        fs::create_dir_all(&dir).unwrap();

        let line = format_credential_fetch_audit(&env_path, &dir, Some(""), "op", "KEY");
        let _ = fs::remove_dir_all(&root);

        // Empty project string is filtered; should fall back to "my-repo" (git root name).
        assert!(
            line.contains("project=my-repo"),
            "expected project=my-repo in: {line}"
        );
    }

    /// Verify that `log_credential_fetch_audit` actually emits a tracing event.
    /// A mutant that replaces the function body with `()` would emit nothing and fail this test.
    #[test]
    fn log_credential_fetch_audit_emits_audit_message() {
        use std::io::Write;
        use std::sync::{Arc, Mutex};

        let buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));

        struct BufWriter(Arc<Mutex<Vec<u8>>>);
        impl Write for BufWriter {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        struct BufMakeWriter(Arc<Mutex<Vec<u8>>>);
        impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for BufMakeWriter {
            type Writer = BufWriter;
            fn make_writer(&'a self) -> BufWriter {
                BufWriter(self.0.clone())
            }
        }

        let root = unique_subdir("audit-emit");
        let dir = root.join("service");
        let env_path = dir.join(".env");
        fs::create_dir_all(&dir).unwrap();

        let subscriber = tracing_subscriber::fmt()
            .with_writer(BufMakeWriter(buf.clone()))
            .with_max_level(tracing::Level::INFO)
            .finish();

        tracing::subscriber::with_default(subscriber, || {
            log_credential_fetch_audit(&env_path, &dir, Some("my-project"), "1Password", "API_KEY");
        });

        let _ = fs::remove_dir_all(&root);
        let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(
            output.contains("AUDIT credential_fetch"),
            "expected audit message in tracing output, got: {output:?}"
        );
    }

    /// Set up mock CLI binaries and an isolated approval store (via a temporary $HOME),
    /// then call `f`, returning its result. The `env_path` .env file must already exist.
    ///
    /// Used by tests that exercise `resolve_env_file` with resolvable entries so that
    /// `Config::ensure_secret_fetch_approved` does not block the test.
    #[cfg(unix)]
    fn with_approval_and_mock_binaries<T, F>(
        op_script: Option<&str>,
        bw_script: Option<&str>,
        gpg_script: Option<&str>,
        env_path: &std::path::Path,
        f: F,
    ) -> T
    where
        F: FnOnce() -> T,
    {
        use std::os::unix::fs::PermissionsExt;

        let _guard = crate::backend::MOCK_PATH_MUTEX
            .lock()
            .unwrap_or_else(|p| p.into_inner());

        // Redirect HOME so the approval store is written to a temp directory.
        let home_tmp = tempfile::TempDir::new().expect("should create temp HOME dir");
        let old_home = std::env::var_os("HOME");
        // SAFETY: serialised by MOCK_PATH_MUTEX — no concurrent HOME/PATH mutations.
        unsafe { std::env::set_var("HOME", home_tmp.path()) };

        // Pre-approve project-wide so ensure_secret_fetch_approved passes.
        crate::config::Config::approve_secret_fetch(
            env_path,
            crate::config::SecretFetchApprovalMode::ProjectWide,
        )
        .expect("should pre-approve env file in temp HOME");

        // Install mock binaries ahead of the real ones on PATH.
        let script_dir = tempfile::TempDir::new().expect("should create temp script dir");
        for (name, script) in [("op", op_script), ("bw", bw_script), ("gpg", gpg_script)] {
            if let Some(src) = script {
                let p = script_dir.path().join(name);
                fs::write(&p, src).expect("should write mock script");
                let mut perms = fs::metadata(&p).unwrap().permissions();
                perms.set_mode(0o755);
                fs::set_permissions(&p, perms).unwrap();
            }
        }

        let old_path = std::env::var_os("PATH").unwrap_or_default();
        let new_path = std::env::join_paths(
            std::iter::once(script_dir.path().to_path_buf())
                .chain(std::env::split_paths(&old_path)),
        )
        .expect("should construct new PATH");
        unsafe { std::env::set_var("PATH", &new_path) };

        let result = f();

        unsafe { std::env::set_var("PATH", &old_path) };
        match old_home {
            Some(h) => unsafe { std::env::set_var("HOME", h) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        result
    }

    /// The op:// reference in an entry must be forwarded to the backend's "read" command.
    /// A mutant that deletes the `EntryKind::OpReference(r)` match arm would make
    /// `reference` always `None`, causing the backend to fall back to key-based "item get"
    /// which the mock rejects — leaving API_KEY absent from the resolved map.
    #[cfg(unix)]
    #[test]
    fn resolve_env_file_passes_op_reference_to_backend() {
        let temp = unique_subdir("resolve-op-ref");
        let env_path = temp.join(".env");
        fs::create_dir_all(&temp).unwrap();
        fs::write(&env_path, "API_KEY=op://My Vault/my-item/field\n").unwrap();

        let op_script =
            "#!/bin/sh\nif [ \"$1\" = 'read' ]; then echo 'op-ref-secret'; else exit 1; fi\n";

        let env_file = crate::env_file::EnvFile::parse(&env_path).unwrap();
        let config = Config {
            defaults: crate::config::Defaults::default(),
            log: crate::config::LogConfig::default(),
            updates: crate::config::UpdateConfig::default(),
            projects: vec![],
        };

        let resolved =
            with_approval_and_mock_binaries(Some(op_script), None, None, &env_path, || {
                resolve_env_file(&env_file, &config, &temp).expect("should resolve env file")
            });

        let _ = fs::remove_dir_all(&temp);
        assert_eq!(
            resolved.get("API_KEY").map(String::as_str),
            Some("op-ref-secret"),
            "op:// reference should be forwarded to 'op read'"
        );
    }

    /// The bw:// reference in an entry must be forwarded to the backend's
    /// exact-name search and selected-item lookup path.
    ///
    /// This test covers two mutations:
    /// - `delete match arm EntryKind::BwReference(r)` → reference becomes None, the backend
    ///   falls back to searching for `BW_KEY` instead of `my-item`, which the mock rejects
    ///   → key absent.
    /// - `delete ! in !bw_entries.is_empty()` → the bw block only runs when bw_entries is
    ///   empty, so BW_KEY is never resolved → key absent.
    #[cfg(unix)]
    #[test]
    fn resolve_env_file_passes_bw_reference_to_backend() {
        let temp = unique_subdir("resolve-bw-ref");
        let env_path = temp.join(".env");
        fs::create_dir_all(&temp).unwrap();
        fs::write(&env_path, "BW_KEY=bw://my-item/password\n").unwrap();

        let bw_list_json = r#"[{"id":"item-1","name":"my-item","login":{"password":"bw-ref-secret"},"fields":[]}]"#;
        let bw_item_json =
            r#"{"type":1,"id":"item-1","name":"my-item","login":{"password":"bw-ref-secret"}}"#;
        // Match only the exact item name extracted from the bw:// reference ("my-item").
        // Without the BwReference arm the backend falls back to searching for "BW_KEY"
        // (the env key name), which does NOT match "my-item", so the mock exits with an
        // error and the key stays absent from the resolved map.
        let bw_script = format!(
            "#!/bin/sh\nif [ \"$1\" = 'status' ]; then echo '{{\"status\":\"unlocked\"}}'; elif [ \"$1\" = 'list' ] && [ \"$2\" = 'items' ] && [ \"$4\" = 'my-item' ]; then echo '{}'; elif [ \"$1\" = 'get' ] && [ \"$2\" = 'item' ] && [ \"$3\" = 'item-1' ]; then echo '{}'; else exit 1; fi\n",
            bw_list_json, bw_item_json
        );

        let env_file = crate::env_file::EnvFile::parse(&env_path).unwrap();
        let config = Config {
            defaults: crate::config::Defaults::default(),
            log: crate::config::LogConfig::default(),
            updates: crate::config::UpdateConfig::default(),
            projects: vec![],
        };

        let resolved =
            with_approval_and_mock_binaries(None, Some(&bw_script), None, &env_path, || {
                resolve_env_file(&env_file, &config, &temp).expect("should resolve env file")
            });

        let _ = fs::remove_dir_all(&temp);
        assert_eq!(
            resolved.get("BW_KEY").map(String::as_str),
            Some("bw-ref-secret"),
            "bw:// reference should be resolved via exact-name search and selected-item lookup"
        );
    }

    /// Empty-value entries must be resolved via the configured non-GPG default backend.
    ///
    /// This test covers two mutations:
    /// - `delete ! in !default_entries.is_empty()` → the default block only runs when the
    ///   list is empty, so DEFAULT_KEY is never resolved → key absent.
    /// - `replace == with != in default_backend_name == "gpg"` → the GPG code path is taken
    ///   even though the backend is "op"; GpgBackend::resolve_all fails (no .env.gpg file)
    ///   → key absent.
    #[cfg(unix)]
    #[test]
    fn resolve_env_file_resolves_default_entries_with_non_gpg_backend() {
        let temp = unique_subdir("resolve-default");
        let env_path = temp.join(".env");
        fs::create_dir_all(&temp).unwrap();
        fs::write(&env_path, "DEFAULT_KEY=\n").unwrap();

        // Any op invocation returns "default-op-secret".
        let op_script = "#!/bin/sh\necho 'default-op-secret'\n";

        let env_file = crate::env_file::EnvFile::parse(&env_path).unwrap();
        let config = Config {
            defaults: crate::config::Defaults::default(), // default backend = "op"
            log: crate::config::LogConfig::default(),
            updates: crate::config::UpdateConfig::default(),
            projects: vec![],
        };

        let resolved =
            with_approval_and_mock_binaries(Some(op_script), None, None, &env_path, || {
                resolve_env_file(&env_file, &config, &temp).expect("should resolve env file")
            });

        let _ = fs::remove_dir_all(&temp);
        assert_eq!(
            resolved.get("DEFAULT_KEY").map(String::as_str),
            Some("default-op-secret"),
            "empty-value entries should be resolved via the configured non-gpg backend"
        );
    }

    #[cfg(unix)]
    #[test]
    fn resolve_env_file_batches_default_bitwarden_entries() {
        let temp = unique_subdir("resolve-default-bw-batch");
        let env_path = temp.join(".env");
        let call_log = temp.join("bw-calls.log");
        fs::create_dir_all(&temp).unwrap();
        fs::write(&env_path, "API_KEY=\nDB_PASS=\n").unwrap();

        let items_json = r#"[{"name":"API_KEY","folderId":"folder-abc","login":{"password":"api-secret"},"fields":[]},{"name":"DB_PASS","folderId":"folder-abc","login":{"password":"db-secret"},"fields":[]}]"#;
        let bw_script = format!(
            "#!/bin/sh\necho \"$@\" >> '{}'\nif [ \"$1\" = 'status' ]; then\n  echo '{{\"status\":\"unlocked\"}}'\nelif [ \"$1\" = 'sync' ]; then\n  exit 0\nelif [ \"$1\" = 'list' ] && [ \"$2\" = 'folders' ] && [ \"$4\" = 'Secrets' ]; then\n  echo '[{{\"name\":\"Secrets\",\"id\":\"folder-abc\"}}]'\nelif [ \"$1\" = 'list' ] && [ \"$2\" = 'items' ] && [ $# -eq 2 ]; then\n  echo '{}'\nelse\n  exit 1\nfi\n",
            call_log.display(),
            items_json
        );

        let env_file = crate::env_file::EnvFile::parse(&env_path).unwrap();
        let config = Config {
            defaults: crate::config::Defaults {
                backend: "bw".to_string(),
                bw: crate::config::BwConfig {
                    folder: Some("Secrets".to_string()),
                    ..crate::config::BwConfig::default()
                },
                ..crate::config::Defaults::default()
            },
            log: crate::config::LogConfig::default(),
            updates: crate::config::UpdateConfig::default(),
            projects: vec![],
        };

        let resolved =
            with_approval_and_mock_binaries(None, Some(&bw_script), None, &env_path, || {
                resolve_env_file(&env_file, &config, &temp).expect("should resolve env file")
            });

        let log = fs::read_to_string(&call_log).unwrap();

        let _ = fs::remove_dir_all(&temp);
        assert_eq!(
            resolved.get("API_KEY").map(String::as_str),
            Some("api-secret")
        );
        assert_eq!(
            resolved.get("DB_PASS").map(String::as_str),
            Some("db-secret")
        );
        assert_eq!(
            log.lines().filter(|line| *line == "list items").count(),
            1,
            "expected a single full item list fetch for default Bitwarden entries"
        );
        assert_eq!(
            log.lines()
                .filter(|line| line.starts_with("list items --search"))
                .count(),
            0,
            "default Bitwarden entries should not fall back to per-key search lookups"
        );
    }

    #[cfg(unix)]
    #[test]
    fn resolve_env_file_uses_secret_cache_for_op_entries() {
        let _lock = crate::cache::keyring_test_lock()
            .lock()
            .expect("keyring test mutex poisoned");
        let temp = unique_subdir("resolve-op-cache");
        let env_path = temp.join(".env");
        let call_log = temp.join("op-calls.log");
        fs::create_dir_all(&temp).unwrap();
        fs::write(&env_path, "API_KEY=op://My Vault/my-item/field\n").unwrap();

        let op_script = format!(
            "#!/bin/sh\necho \"$@\" >> '{}'\nif [ \"$1\" = 'read' ]; then echo 'op-cache-secret'; else exit 1; fi\n",
            call_log.display()
        );

        let env_file = crate::env_file::EnvFile::parse(&env_path).unwrap();
        let config = Config {
            defaults: crate::config::Defaults::default(),
            log: crate::config::LogConfig::default(),
            updates: crate::config::UpdateConfig::default(),
            projects: vec![],
        };

        crate::cache::set_test_secret_cache_index_path(Some(
            temp.join("pw-env").join("resolved-secret-cache.json"),
        ));
        crate::cache::set_test_keyring_available(true);

        let (first, second) =
            with_approval_and_mock_binaries(Some(&op_script), None, None, &env_path, || {
                let first = resolve_env_file(&env_file, &config, &temp).unwrap();
                let second = resolve_env_file(&env_file, &config, &temp).unwrap();
                (first, second)
            });

        crate::cache::reset_test_keyring();
        crate::cache::set_test_secret_cache_index_path(None);

        let log = fs::read_to_string(&call_log).unwrap();
        let _ = fs::remove_dir_all(&temp);

        assert_eq!(
            first.get("API_KEY").map(String::as_str),
            Some("op-cache-secret")
        );
        assert_eq!(
            second.get("API_KEY").map(String::as_str),
            Some("op-cache-secret")
        );
        assert_eq!(log.lines().count(), 1, "expected exactly one op invocation");
    }

    #[cfg(unix)]
    #[test]
    fn resolve_env_file_uses_secret_cache_for_bitwarden_entries() {
        let _lock = crate::cache::keyring_test_lock()
            .lock()
            .expect("keyring test mutex poisoned");
        let temp = unique_subdir("resolve-bw-cache");
        let env_path = temp.join(".env");
        let call_log = temp.join("bw-calls.log");
        fs::create_dir_all(&temp).unwrap();
        fs::write(&env_path, "API_KEY=\n").unwrap();

        let items_json = r#"[{"name":"API_KEY","folderId":"folder-abc","login":{"password":"bw-cache-secret"},"fields":[]}]"#;
        let bw_script = format!(
            "#!/bin/sh\necho \"$@\" >> '{}'\nif [ \"$1\" = 'status' ]; then\n  echo '{{\"status\":\"unlocked\"}}'\nelif [ \"$1\" = 'sync' ]; then\n  exit 0\nelif [ \"$1\" = 'list' ] && [ \"$2\" = 'folders' ] && [ \"$4\" = 'Secrets' ]; then\n  echo '[{{\"name\":\"Secrets\",\"id\":\"folder-abc\"}}]'\nelif [ \"$1\" = 'list' ] && [ \"$2\" = 'items' ] && [ $# -eq 2 ]; then\n  echo '{}'\nelse\n  exit 1\nfi\n",
            call_log.display(),
            items_json
        );

        let env_file = crate::env_file::EnvFile::parse(&env_path).unwrap();
        let config = Config {
            defaults: crate::config::Defaults {
                backend: "bw".to_string(),
                bw: crate::config::BwConfig {
                    folder: Some("Secrets".to_string()),
                    ..crate::config::BwConfig::default()
                },
                ..crate::config::Defaults::default()
            },
            log: crate::config::LogConfig::default(),
            updates: crate::config::UpdateConfig::default(),
            projects: vec![],
        };

        crate::cache::set_test_secret_cache_index_path(Some(
            temp.join("pw-env").join("resolved-secret-cache.json"),
        ));
        crate::cache::set_test_keyring_available(true);

        let (first, second) =
            with_approval_and_mock_binaries(None, Some(&bw_script), None, &env_path, || {
                let first = resolve_env_file(&env_file, &config, &temp).unwrap();
                let second = resolve_env_file(&env_file, &config, &temp).unwrap();
                (first, second)
            });

        crate::cache::reset_test_keyring();
        crate::cache::set_test_secret_cache_index_path(None);

        let log = fs::read_to_string(&call_log).unwrap();
        let _ = fs::remove_dir_all(&temp);

        assert_eq!(
            first.get("API_KEY").map(String::as_str),
            Some("bw-cache-secret")
        );
        assert_eq!(
            second.get("API_KEY").map(String::as_str),
            Some("bw-cache-secret")
        );
        assert_eq!(
            log.lines().filter(|line| *line == "list items").count(),
            1,
            "expected the Bitwarden item list to be fetched only once"
        );
    }

    #[cfg(unix)]
    #[test]
    fn resolve_env_file_uses_secret_cache_for_gpg_entries() {
        let _lock = crate::cache::keyring_test_lock()
            .lock()
            .expect("keyring test mutex poisoned");
        let temp = unique_subdir("resolve-gpg-cache");
        let env_path = temp.join(".env");
        let encrypted_path = temp.join(".env.gpg");
        let call_log = temp.join("gpg-calls.log");
        fs::create_dir_all(&temp).unwrap();
        fs::write(&env_path, "GPG_KEY=\n").unwrap();
        fs::write(&encrypted_path, "placeholder").unwrap();

        let gpg_script = format!(
            "#!/bin/sh\necho \"$@\" >> '{}'\nif [ \"$1\" = '--decrypt' ]; then\n  printf 'GPG_KEY=gpg-cache-secret\\n'\n  exit 0\nfi\nexit 1\n",
            call_log.display()
        );

        let env_file = crate::env_file::EnvFile::parse(&env_path).unwrap();
        let config = Config {
            defaults: crate::config::Defaults {
                backend: "gpg".to_string(),
                ..crate::config::Defaults::default()
            },
            log: crate::config::LogConfig::default(),
            updates: crate::config::UpdateConfig::default(),
            projects: vec![],
        };

        crate::cache::set_test_secret_cache_index_path(Some(
            temp.join("pw-env").join("resolved-secret-cache.json"),
        ));
        crate::cache::set_test_keyring_available(true);

        let (first, second) =
            with_approval_and_mock_binaries(None, None, Some(&gpg_script), &env_path, || {
                let first = resolve_env_file(&env_file, &config, &temp).unwrap();
                let second = resolve_env_file(&env_file, &config, &temp).unwrap();
                (first, second)
            });

        crate::cache::reset_test_keyring();
        crate::cache::set_test_secret_cache_index_path(None);

        let log = fs::read_to_string(&call_log).unwrap();
        let _ = fs::remove_dir_all(&temp);

        assert_eq!(
            first.get("GPG_KEY").map(String::as_str),
            Some("gpg-cache-secret")
        );
        assert_eq!(
            second.get("GPG_KEY").map(String::as_str),
            Some("gpg-cache-secret")
        );
        assert_eq!(
            log.lines()
                .filter(|line| line.starts_with("--decrypt"))
                .count(),
            1,
            "expected a single gpg decrypt across cached resolves"
        );
    }

    #[cfg(unix)]
    #[test]
    fn resolve_env_file_falls_back_when_keyring_is_unavailable() {
        let _lock = crate::cache::keyring_test_lock()
            .lock()
            .expect("keyring test mutex poisoned");
        let temp = unique_subdir("resolve-cache-unavailable");
        let env_path = temp.join(".env");
        let call_log = temp.join("op-calls.log");
        fs::create_dir_all(&temp).unwrap();
        fs::write(&env_path, "API_KEY=op://My Vault/my-item/field\n").unwrap();

        let op_script = format!(
            "#!/bin/sh\necho \"$@\" >> '{}'\nif [ \"$1\" = 'read' ]; then echo 'op-fallback-secret'; else exit 1; fi\n",
            call_log.display()
        );

        let env_file = crate::env_file::EnvFile::parse(&env_path).unwrap();
        let config = Config {
            defaults: crate::config::Defaults::default(),
            log: crate::config::LogConfig::default(),
            updates: crate::config::UpdateConfig::default(),
            projects: vec![],
        };

        crate::cache::set_test_secret_cache_index_path(Some(
            temp.join("pw-env").join("resolved-secret-cache.json"),
        ));
        crate::cache::set_test_keyring_available(false);

        let (first, second) =
            with_approval_and_mock_binaries(Some(&op_script), None, None, &env_path, || {
                let first = resolve_env_file(&env_file, &config, &temp).unwrap();
                let second = resolve_env_file(&env_file, &config, &temp).unwrap();
                (first, second)
            });

        crate::cache::reset_test_keyring();
        crate::cache::set_test_secret_cache_index_path(None);

        let log = fs::read_to_string(&call_log).unwrap();
        let _ = fs::remove_dir_all(&temp);

        assert_eq!(
            first.get("API_KEY").map(String::as_str),
            Some("op-fallback-secret")
        );
        assert_eq!(
            second.get("API_KEY").map(String::as_str),
            Some("op-fallback-secret")
        );
        assert_eq!(
            log.lines().count(),
            2,
            "expected backend resolution to continue when the keyring is unavailable"
        );
    }
}
