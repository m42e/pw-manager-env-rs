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
        for (name, script) in [("op", op_script), ("bw", bw_script)] {
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

        let resolved = with_approval_and_mock_binaries(Some(op_script), None, &env_path, || {
            resolve_env_file(&env_file, &config, &temp).expect("should resolve env file")
        });

        let _ = fs::remove_dir_all(&temp);
        assert_eq!(
            resolved.get("API_KEY").map(String::as_str),
            Some("op-ref-secret"),
            "op:// reference should be forwarded to 'op read'"
        );
    }

    /// The bw:// reference in an entry must be forwarded to the backend's "get item" command.
    ///
    /// This test covers two mutations:
    /// - `delete match arm EntryKind::BwReference(r)` → reference becomes None, the backend
    ///   falls back to "get password BW_KEY" which the mock rejects → key absent.
    /// - `delete ! in !bw_entries.is_empty()` → the bw block only runs when bw_entries is
    ///   empty, so BW_KEY is never resolved → key absent.
    #[cfg(unix)]
    #[test]
    fn resolve_env_file_passes_bw_reference_to_backend() {
        let temp = unique_subdir("resolve-bw-ref");
        let env_path = temp.join(".env");
        fs::create_dir_all(&temp).unwrap();
        fs::write(&env_path, "BW_KEY=bw://my-item/password\n").unwrap();

        let bw_item_json = r#"{"type":1,"name":"my-item","login":{"password":"bw-ref-secret"}}"#;
        // Match only the exact item name extracted from the bw:// reference ("my-item").
        // Without the BwReference arm the backend falls back to "get item BW_KEY" (the env
        // key name), which does NOT match "my-item", so the mock exits with an error and
        // the key stays absent from the resolved map.
        let bw_script = format!(
            "#!/bin/sh\nif [ \"$1\" = 'status' ]; then echo '{{\"status\":\"unlocked\"}}'; elif [ \"$1\" = 'get' ] && [ \"$2\" = 'item' ] && [ \"$3\" = 'my-item' ]; then echo '{}'; else exit 1; fi\n",
            bw_item_json
        );

        let env_file = crate::env_file::EnvFile::parse(&env_path).unwrap();
        let config = Config {
            defaults: crate::config::Defaults::default(),
            log: crate::config::LogConfig::default(),
            updates: crate::config::UpdateConfig::default(),
            projects: vec![],
        };

        let resolved = with_approval_and_mock_binaries(None, Some(&bw_script), &env_path, || {
            resolve_env_file(&env_file, &config, &temp).expect("should resolve env file")
        });

        let _ = fs::remove_dir_all(&temp);
        assert_eq!(
            resolved.get("BW_KEY").map(String::as_str),
            Some("bw-ref-secret"),
            "bw:// reference should be forwarded to 'bw get item'"
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

        let resolved = with_approval_and_mock_binaries(Some(op_script), None, &env_path, || {
            resolve_env_file(&env_file, &config, &temp).expect("should resolve env file")
        });

        let _ = fs::remove_dir_all(&temp);
        assert_eq!(
            resolved.get("DEFAULT_KEY").map(String::as_str),
            Some("default-op-secret"),
            "empty-value entries should be resolved via the configured non-gpg backend"
        );
    }
}
