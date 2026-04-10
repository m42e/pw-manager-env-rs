use anyhow::{Context, Result, bail};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::process::Command;
use tracing::debug;

use super::{
    Backend, CREATED_WITH_FIELD_NAME, MIGRATED_FROM_FIELD_NAME, PROJECT_FIELD_NAME, ResolveContext,
    StoreContext,
};

pub struct GpgBackend;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct StoredSecret {
    value: String,
    project: Option<String>,
    migrated_from: Option<String>,
    created_with: Option<String>,
}

impl GpgBackend {
    fn metadata_comment(name: &str, value: &str) -> String {
        format!("# pw-env: {name}={value}")
    }

    /// Decrypt a GPG file and return its contents entirely in memory.
    fn decrypt_file(path: &PathBuf) -> Result<String> {
        debug!("Decrypting GPG file: {}", path.display());
        let output = Command::new("gpg")
            .args(["--decrypt", "--batch", "--quiet"])
            .arg(path)
            .stdin(std::process::Stdio::null())
            .output()
            .context("Failed to execute `gpg`. Is GnuPG installed?")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("gpg decrypt failed: {stderr}");
        }
        String::from_utf8(output.stdout).context("GPG decrypted content was not valid UTF-8")
    }

    /// Parse KEY=VALUE lines from decrypted content (like a .env file).
    fn parse_env_content(content: &str) -> HashMap<String, String> {
        Self::parse_stored_secrets(content)
            .into_iter()
            .map(|(key, secret)| (key, secret.value))
            .collect()
    }

    fn parse_stored_secrets(content: &str) -> BTreeMap<String, StoredSecret> {
        let mut map = BTreeMap::new();
        let mut pending_project: Option<String> = None;
        let mut pending_migrated_from: Option<String> = None;
        let mut pending_created_with: Option<String> = None;

        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                if let Some(metadata) = line.strip_prefix("# pw-env: ")
                    && let Some((name, value)) = metadata.split_once('=')
                {
                    match name.trim() {
                        PROJECT_FIELD_NAME => pending_project = Some(value.trim().to_string()),
                        MIGRATED_FROM_FIELD_NAME => {
                            pending_migrated_from = Some(value.trim().to_string())
                        }
                        CREATED_WITH_FIELD_NAME => {
                            pending_created_with = Some(value.trim().to_string())
                        }
                        _ => {}
                    }
                }
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                let key = key.trim();
                let value = value.trim();
                // Strip surrounding quotes
                let value = if (value.starts_with('"') && value.ends_with('"'))
                    || (value.starts_with('\'') && value.ends_with('\''))
                {
                    let mut chars = value.chars();
                    chars.next();
                    chars.next_back();
                    chars.as_str()
                } else {
                    value
                };
                if !key.is_empty() {
                    map.insert(
                        key.to_string(),
                        StoredSecret {
                            value: value.to_string(),
                            project: pending_project.take(),
                            migrated_from: pending_migrated_from.take(),
                            created_with: pending_created_with.take(),
                        },
                    );
                }
            } else {
                pending_project = None;
                pending_migrated_from = None;
                pending_created_with = None;
            }
        }
        map
    }

    /// Find the GPG file path for the given directory and config.
    /// Validates that file_pattern does not escape the project directory.
    fn gpg_file_path(ctx_dir: &std::path::Path, config: &crate::config::Config) -> PathBuf {
        let gpg_config = config.effective_gpg(ctx_dir);
        let joined = ctx_dir.join(&gpg_config.file_pattern);
        // Normalize the path and verify it stays within the project directory.
        // If canonicalize fails (file doesn't exist yet), fall through to the
        // component-based check.
        if let Ok(canonical) = joined.canonicalize() {
            if let Ok(canonical_dir) = ctx_dir.canonicalize()
                && !canonical.starts_with(&canonical_dir)
            {
                tracing::warn!(
                    "GPG file_pattern '{}' escapes the project directory; falling back to default",
                    gpg_config.file_pattern
                );
                return ctx_dir.join(".env.gpg");
            }
        } else {
            // File doesn't exist yet — check for path traversal components
            if gpg_config.file_pattern.contains("..") {
                tracing::warn!(
                    "GPG file_pattern '{}' contains path traversal; falling back to default",
                    gpg_config.file_pattern
                );
                return ctx_dir.join(".env.gpg");
            }
        }
        joined
    }

    /// Decrypt and parse the GPG env file, returning all key-value pairs.
    fn load_all(ctx: &ResolveContext) -> Result<HashMap<String, String>> {
        let path = Self::gpg_file_path(ctx.dir, ctx.config);
        if !path.exists() {
            bail!("GPG encrypted file not found: {}", path.display());
        }
        let content = Self::decrypt_file(&path)?;
        Ok(Self::parse_env_content(&content))
    }

    fn load_all_stored_secrets(ctx: &ResolveContext) -> Result<BTreeMap<String, StoredSecret>> {
        let path = Self::gpg_file_path(ctx.dir, ctx.config);
        if !path.exists() {
            bail!("GPG encrypted file not found: {}", path.display());
        }
        let content = Self::decrypt_file(&path)?;
        Ok(Self::parse_stored_secrets(&content))
    }

    fn serialize_stored_secrets(secrets: &BTreeMap<String, StoredSecret>) -> String {
        let mut content = String::new();
        for (key, secret) in secrets {
            if let Some(project) = secret.project.as_deref() {
                content.push_str(&Self::metadata_comment(PROJECT_FIELD_NAME, project));
                content.push('\n');
            }
            if let Some(migrated_from) = secret.migrated_from.as_deref() {
                content.push_str(&Self::metadata_comment(
                    MIGRATED_FROM_FIELD_NAME,
                    migrated_from,
                ));
                content.push('\n');
            }
            if let Some(created_with) = secret.created_with.as_deref() {
                content.push_str(&Self::metadata_comment(
                    CREATED_WITH_FIELD_NAME,
                    created_with,
                ));
                content.push('\n');
            }
            content.push_str(&format!("{key}={}\n", secret.value));
        }
        content
    }

    /// Encrypt content and write to the GPG file.
    fn encrypt_to_file(content: &str, path: &PathBuf, recipient: &str) -> Result<()> {
        debug!("Encrypting content to GPG file: {}", path.display());
        let mut cmd = Command::new("gpg");
        cmd.args([
            "--encrypt",
            "--batch",
            "--yes",
            "--recipient",
            recipient,
            "--output",
        ]);
        cmd.arg(path);
        cmd.stdin(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        let mut child = cmd
            .spawn()
            .context("Failed to execute `gpg` for encryption")?;
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            stdin.write_all(content.as_bytes())?;
        }
        let output = child.wait_with_output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("gpg encrypt failed: {stderr}");
        }
        Ok(())
    }
}

impl Backend for GpgBackend {
    fn resolve(&self, key: &str, _reference: Option<&str>, ctx: &ResolveContext) -> Result<String> {
        let all = Self::load_all(ctx)?;
        all.get(key)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Key '{key}' not found in GPG encrypted env file"))
    }

    fn store(&self, key: &str, value: &str, ctx: &StoreContext) -> Result<()> {
        let gpg_config = ctx.config.effective_gpg(ctx.dir);
        let recipient = gpg_config
            .recipient
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("GPG recipient must be configured to store secrets"))?;

        let path = Self::gpg_file_path(ctx.dir, ctx.config);

        // Load existing content (or start empty)
        let resolve_ctx = ResolveContext {
            dir: ctx.dir,
            config: ctx.config,
            project: None,
            repository: None,
        };
        let mut existing = if path.exists() {
            Self::load_all_stored_secrets(&resolve_ctx).unwrap_or_default()
        } else {
            BTreeMap::new()
        };

        existing.insert(
            key.to_string(),
            StoredSecret {
                value: value.to_string(),
                project: ctx.project.clone(),
                migrated_from: Some(ctx.migrated_from()),
                created_with: Some(ctx.created_with()),
            },
        );

        let content = Self::serialize_stored_secrets(&existing);

        Self::encrypt_to_file(&content, &path, recipient)?;
        Ok(())
    }

    fn has(&self, key: &str, ctx: &ResolveContext) -> Result<bool> {
        let all = Self::load_all(ctx)?;
        Ok(all.contains_key(key))
    }

    fn name(&self) -> &str {
        "GPG"
    }
}

/// Special method: resolve ALL keys from the GPG file at once (more efficient than one-by-one).
impl GpgBackend {
    pub fn resolve_all(ctx: &ResolveContext) -> Result<HashMap<String, String>> {
        Self::load_all(ctx)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::config::{Config, Defaults, LogConfig, UpdateConfig};

    #[test]
    fn test_parse_stored_secrets_with_line_without_equals() {
        // A line with no '=' should clear pending metadata
        let content = "# pw-env: project=service-a\nNOT_A_KV_PAIR\nVALID=value\n";
        let stored = GpgBackend::parse_stored_secrets(content);
        // Metadata should have been cleared by the non-KV line
        assert_eq!(stored.get("VALID").unwrap().project, None);
    }

    #[test]
    fn test_parse_stored_secrets_with_unknown_metadata_key() {
        let content = "# pw-env: unknown_key=some_value\nFOO=bar\n";
        let stored = GpgBackend::parse_stored_secrets(content);
        let foo = stored.get("FOO").unwrap();
        assert_eq!(foo.project, None);
        assert_eq!(foo.migrated_from, None);
        assert_eq!(foo.created_with, None);
    }

    #[test]
    fn test_parse_stored_secrets_with_double_quoted_value() {
        let content = "KEY=\"quoted value\"\n";
        let stored = GpgBackend::parse_stored_secrets(content);
        assert_eq!(stored.get("KEY").unwrap().value, "quoted value");
    }

    #[test]
    fn test_parse_stored_secrets_with_single_quoted_value() {
        let content = "KEY='single quoted'\n";
        let stored = GpgBackend::parse_stored_secrets(content);
        assert_eq!(stored.get("KEY").unwrap().value, "single quoted");
    }

    #[test]
    fn test_parse_stored_secrets_with_empty_key_ignored() {
        let content = "=value\n";
        let stored = GpgBackend::parse_stored_secrets(content);
        assert!(!stored.contains_key(""));
    }

    #[test]
    fn test_serialize_stored_secrets_with_no_metadata() {
        let mut stored = BTreeMap::new();
        stored.insert(
            "PLAIN_KEY".to_string(),
            StoredSecret {
                value: "plain_value".to_string(),
                project: None,
                migrated_from: None,
                created_with: None,
            },
        );
        let serialized = GpgBackend::serialize_stored_secrets(&stored);
        assert!(serialized.contains("PLAIN_KEY=plain_value"));
        assert!(!serialized.contains("# pw-env:"));
    }

    #[test]
    fn test_serialize_stored_secrets_empty() {
        let stored = BTreeMap::new();
        let serialized = GpgBackend::serialize_stored_secrets(&stored);
        assert_eq!(serialized, "");
    }

    #[test]
    fn test_metadata_comment_format() {
        let comment = GpgBackend::metadata_comment("project", "my-project");
        assert_eq!(comment, "# pw-env: project=my-project");
    }

    #[test]
    fn test_parse_env_content_empty() {
        let map = GpgBackend::parse_env_content("");
        assert!(map.is_empty());
    }

    #[test]
    fn test_gpg_file_path() {
        let config = Config {
            defaults: Defaults::default(),
            log: LogConfig::default(),
            updates: UpdateConfig::default(),
            projects: vec![],
        };
        let dir = std::path::Path::new("/some/dir");
        let path = GpgBackend::gpg_file_path(dir, &config);
        assert_eq!(path, PathBuf::from("/some/dir/.env.gpg"));
    }

    #[test]
    fn test_parse_env_content() {
        let content = r#"
# Database settings
DB_HOST=localhost
DB_PORT=5432
DB_PASSWORD="secret value"
API_KEY='my-api-key'
EMPTY=

# Comment line
SPACED_KEY = spaced_value
"#;
        let map = GpgBackend::parse_env_content(content);
        assert_eq!(map.get("DB_HOST").unwrap(), "localhost");
        assert_eq!(map.get("DB_PORT").unwrap(), "5432");
        assert_eq!(map.get("DB_PASSWORD").unwrap(), "secret value");
        assert_eq!(map.get("API_KEY").unwrap(), "my-api-key");
        assert_eq!(map.get("EMPTY").unwrap(), "");
        assert_eq!(map.get("SPACED_KEY").unwrap(), "spaced_value");
    }

    #[test]
    fn test_parse_stored_secrets_reads_metadata_comments() {
        let content = r#"
# pw-env: project=service-a
# pw-env: migrated_from=/tmp/work/service-a
# pw-env: created-with=pw-env (0.0.0)
API_KEY=secret
PLAIN=value
"#;

        let stored = GpgBackend::parse_stored_secrets(content);

        assert_eq!(
            stored.get("API_KEY").unwrap().project.as_deref(),
            Some("service-a")
        );
        assert_eq!(
            stored.get("API_KEY").unwrap().migrated_from.as_deref(),
            Some("/tmp/work/service-a")
        );
        assert_eq!(
            stored.get("API_KEY").unwrap().created_with.as_deref(),
            Some("pw-env (0.0.0)")
        );
        assert_eq!(stored.get("PLAIN").unwrap().project, None);
    }

    #[test]
    fn test_serialize_stored_secrets_emits_metadata_comments() {
        let mut stored = BTreeMap::new();
        stored.insert(
            "API_KEY".to_string(),
            StoredSecret {
                value: "secret".to_string(),
                project: Some("service-a".to_string()),
                migrated_from: Some("/tmp/work/service-a".to_string()),
                created_with: Some(format!("pw-env ({})", env!("CARGO_PKG_VERSION"))),
            },
        );

        let serialized = GpgBackend::serialize_stored_secrets(&stored);

        assert!(serialized.contains("# pw-env: project=service-a"));
        assert!(serialized.contains("# pw-env: migrated_from=/tmp/work/service-a"));
        assert!(serialized.contains(&format!(
            "# pw-env: created-with=pw-env ({})",
            env!("CARGO_PKG_VERSION")
        )));
        assert!(serialized.contains("API_KEY=secret"));
    }

    // ------- Error-path tests (no mock needed) -------

    fn make_gpg_resolve_context<'a>(
        config: &'a Config,
        dir: &'a std::path::Path,
    ) -> super::super::ResolveContext<'a> {
        super::super::ResolveContext {
            dir,
            config,
            project: None,
            repository: None,
        }
    }

    #[test]
    fn load_all_returns_err_when_gpg_file_missing() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let config = Config {
            defaults: Defaults::default(),
            log: LogConfig::default(),
            updates: UpdateConfig::default(),
            projects: vec![],
        };
        let ctx = make_gpg_resolve_context(&config, temp_dir.path());
        let result = GpgBackend::resolve_all(&ctx);
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("not found") || msg.contains(".env.gpg"));
    }

    #[test]
    fn backend_resolve_returns_err_when_gpg_file_missing() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let config = Config {
            defaults: Defaults::default(),
            log: LogConfig::default(),
            updates: UpdateConfig::default(),
            projects: vec![],
        };
        let ctx = make_gpg_resolve_context(&config, temp_dir.path());
        let backend = GpgBackend;
        let result = backend.resolve("ANY_KEY", None, &ctx);
        assert!(result.is_err());
    }

    #[test]
    fn backend_has_returns_err_when_gpg_file_missing() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let config = Config {
            defaults: Defaults::default(),
            log: LogConfig::default(),
            updates: UpdateConfig::default(),
            projects: vec![],
        };
        let ctx = make_gpg_resolve_context(&config, temp_dir.path());
        let backend = GpgBackend;
        let result = backend.has("ANY_KEY", &ctx);
        assert!(result.is_err());
    }

    // ------- Mock-gpg infrastructure -------

    fn with_mock_gpg<F: FnOnce()>(decrypt_output: &str, f: F) {
        let _guard = super::super::MOCK_PATH_MUTEX
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::TempDir::new().unwrap();
        let script_path = dir.path().join("gpg");
        // Script echoes the preset decrypt output when --decrypt is given; exits 0 for anything else
        let script = format!(
            "#!/bin/sh\nif [ \"$1\" = \"--decrypt\" ]; then\nprintf '%s' '{}'\nelse\ncat >/dev/null\nfi\nexit 0\n",
            decrypt_output.replace('\'', "'\\''")
        );
        std::fs::write(&script_path, &script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&script_path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&script_path, perms).unwrap();
        }
        let old_path = std::env::var_os("PATH").unwrap_or_default();
        let new_path = std::env::join_paths(
            std::iter::once(dir.path().to_path_buf()).chain(std::env::split_paths(&old_path)),
        )
        .unwrap();
        // SAFETY: guarded by MOCK_PATH_MUTEX, single-threaded access to PATH
        unsafe { std::env::set_var("PATH", &new_path) };
        f();
        unsafe { std::env::set_var("PATH", &old_path) };
    }

    #[test]
    fn load_all_returns_values_with_mock_gpg() {
        let gpg_content = "SECRET_KEY=top_secret\nDB_HOST=localhost\n";
        with_mock_gpg(gpg_content, || {
            let temp_dir = tempfile::TempDir::new().unwrap();
            // Create a placeholder .env.gpg file (mock ignores its content)
            let gpg_file = temp_dir.path().join(".env.gpg");
            std::fs::write(&gpg_file, "placeholder").unwrap();

            let config = Config {
                defaults: Defaults::default(),
                log: LogConfig::default(),
                updates: UpdateConfig::default(),
                projects: vec![],
            };
            let ctx = make_gpg_resolve_context(&config, temp_dir.path());
            let result = GpgBackend::resolve_all(&ctx).unwrap();
            assert_eq!(
                result.get("SECRET_KEY").map(|s| s.as_str()),
                Some("top_secret")
            );
            assert_eq!(result.get("DB_HOST").map(|s| s.as_str()), Some("localhost"));
        });
    }

    #[test]
    fn backend_resolve_returns_value_with_mock_gpg() {
        let gpg_content = "API_KEY=my_api_key\n";
        with_mock_gpg(gpg_content, || {
            let temp_dir = tempfile::TempDir::new().unwrap();
            let gpg_file = temp_dir.path().join(".env.gpg");
            std::fs::write(&gpg_file, "placeholder").unwrap();

            let config = Config {
                defaults: Defaults::default(),
                log: LogConfig::default(),
                updates: UpdateConfig::default(),
                projects: vec![],
            };
            let ctx = make_gpg_resolve_context(&config, temp_dir.path());
            let backend = GpgBackend;
            let result = backend.resolve("API_KEY", None, &ctx);
            assert_eq!(result.unwrap(), "my_api_key");
        });
    }

    #[test]
    fn backend_resolve_returns_err_for_missing_key_with_mock_gpg() {
        let gpg_content = "OTHER_KEY=some_value\n";
        with_mock_gpg(gpg_content, || {
            let temp_dir = tempfile::TempDir::new().unwrap();
            let gpg_file = temp_dir.path().join(".env.gpg");
            std::fs::write(&gpg_file, "placeholder").unwrap();

            let config = Config {
                defaults: Defaults::default(),
                log: LogConfig::default(),
                updates: UpdateConfig::default(),
                projects: vec![],
            };
            let ctx = make_gpg_resolve_context(&config, temp_dir.path());
            let backend = GpgBackend;
            let result = backend.resolve("MISSING_KEY", None, &ctx);
            assert!(result.is_err());
        });
    }

    #[test]
    fn backend_has_returns_true_with_mock_gpg() {
        let gpg_content = "PRESENT_KEY=value\n";
        with_mock_gpg(gpg_content, || {
            let temp_dir = tempfile::TempDir::new().unwrap();
            let gpg_file = temp_dir.path().join(".env.gpg");
            std::fs::write(&gpg_file, "placeholder").unwrap();

            let config = Config {
                defaults: Defaults::default(),
                log: LogConfig::default(),
                updates: UpdateConfig::default(),
                projects: vec![],
            };
            let ctx = make_gpg_resolve_context(&config, temp_dir.path());
            let backend = GpgBackend;
            assert_eq!(backend.has("PRESENT_KEY", &ctx).unwrap(), true);
        });
    }

    #[test]
    fn backend_has_returns_false_with_mock_gpg_when_key_absent() {
        let gpg_content = "OTHER_KEY=value\n";
        with_mock_gpg(gpg_content, || {
            let temp_dir = tempfile::TempDir::new().unwrap();
            let gpg_file = temp_dir.path().join(".env.gpg");
            std::fs::write(&gpg_file, "placeholder").unwrap();

            let config = Config {
                defaults: Defaults::default(),
                log: LogConfig::default(),
                updates: UpdateConfig::default(),
                projects: vec![],
            };
            let ctx = make_gpg_resolve_context(&config, temp_dir.path());
            let backend = GpgBackend;
            assert_eq!(backend.has("MISSING_KEY", &ctx).unwrap(), false);
        });
    }

    #[test]
    fn backend_name_is_gpg() {
        assert_eq!(GpgBackend.name(), "GPG");
    }

    #[test]
    fn store_returns_err_when_recipient_not_configured() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let config = Config {
            defaults: Defaults::default(), // no GPG recipient
            log: LogConfig::default(),
            updates: UpdateConfig::default(),
            projects: vec![],
        };
        let ctx = super::super::StoreContext {
            dir: temp_dir.path(),
            config: &config,
            project: None,
            repository: None,
        };
        let result = GpgBackend.store("MY_KEY", "my-value", &ctx);
        assert!(result.is_err());
        assert!(format!("{}", result.unwrap_err()).contains("GPG recipient must be configured"));
    }

    #[test]
    fn store_creates_new_file_with_mock_gpg() {
        with_mock_gpg("", || {
            let temp_dir = tempfile::TempDir::new().unwrap();
            let config = Config {
                defaults: Defaults {
                    gpg: crate::config::GpgConfig {
                        recipient: Some("test@example.com".to_string()),
                        file_pattern: ".env.gpg".to_string(),
                    },
                    ..Defaults::default()
                },
                log: LogConfig::default(),
                updates: UpdateConfig::default(),
                projects: vec![],
            };
            let ctx = super::super::StoreContext {
                dir: temp_dir.path(),
                config: &config,
                project: None,
                repository: None,
            };
            // No .env.gpg file exists yet — store should create it via mock gpg encrypt
            let result = GpgBackend.store("NEW_KEY", "new-value", &ctx);
            assert!(result.is_ok(), "store failed: {:?}", result);
        });
    }

    #[test]
    fn store_appends_to_existing_file_with_mock_gpg() {
        // The mock gpg script returns an existing secret when decrypting
        with_mock_gpg("EXISTING_KEY=existing-value\n", || {
            let temp_dir = tempfile::TempDir::new().unwrap();
            let gpg_file = temp_dir.path().join(".env.gpg");
            std::fs::write(&gpg_file, "placeholder").unwrap();
            let config = Config {
                defaults: Defaults {
                    gpg: crate::config::GpgConfig {
                        recipient: Some("test@example.com".to_string()),
                        file_pattern: ".env.gpg".to_string(),
                    },
                    ..Defaults::default()
                },
                log: LogConfig::default(),
                updates: UpdateConfig::default(),
                projects: vec![],
            };
            let ctx = super::super::StoreContext {
                dir: temp_dir.path(),
                config: &config,
                project: Some("my-project".to_string()),
                repository: None,
            };
            // File exists → load_all_stored_secrets is called (decrypt), then encrypt
            let result = GpgBackend.store("NEW_KEY", "new-value", &ctx);
            assert!(result.is_ok(), "store failed: {:?}", result);
        });
    }

    #[test]
    fn test_parse_stored_secrets_unmatched_leading_double_quote_not_stripped() {
        // Value starts with '"' but does not end with '"' — no stripping should occur.
        // This kills the && → || mutation at the double-quote condition.
        let content = "KEY=\"abc\n";
        let stored = GpgBackend::parse_stored_secrets(content);
        assert_eq!(stored.get("KEY").unwrap().value, "\"abc");
    }

    #[test]
    fn test_parse_stored_secrets_unmatched_leading_single_quote_not_stripped() {
        // Value starts with "'" but does not end with "'" — no stripping should occur.
        // This kills the && → || mutation at the single-quote condition.
        let content = "KEY='abc\n";
        let stored = GpgBackend::parse_stored_secrets(content);
        assert_eq!(stored.get("KEY").unwrap().value, "'abc");
    }

    #[test]
    fn test_gpg_file_path_rejects_traversal() {
        let config = Config {
            defaults: Defaults {
                gpg: crate::config::GpgConfig {
                    file_pattern: "../../etc/shadow.gpg".to_string(),
                    recipient: None,
                },
                ..Defaults::default()
            },
            log: LogConfig::default(),
            updates: UpdateConfig::default(),
            projects: vec![],
        };
        let dir = std::path::Path::new("/some/dir");
        let path = GpgBackend::gpg_file_path(dir, &config);
        // Should fall back to default rather than allowing traversal
        assert_eq!(path, PathBuf::from("/some/dir/.env.gpg"));
    }

    #[test]
    fn test_gpg_file_path_allows_safe_pattern() {
        let config = Config {
            defaults: Defaults {
                gpg: crate::config::GpgConfig {
                    file_pattern: "secrets.gpg".to_string(),
                    recipient: None,
                },
                ..Defaults::default()
            },
            log: LogConfig::default(),
            updates: UpdateConfig::default(),
            projects: vec![],
        };
        let dir = std::path::Path::new("/some/dir");
        let path = GpgBackend::gpg_file_path(dir, &config);
        assert_eq!(path, PathBuf::from("/some/dir/secrets.gpg"));
    }
}
