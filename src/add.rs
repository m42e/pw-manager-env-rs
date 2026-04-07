use anyhow::{Context, Result, bail};
use dialoguer::Password;
use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};
use tracing::info;

use crate::backend::{self, ResolveContext, StoreContext};
use crate::config::Config;
use crate::env_file::{self, EntryKind};
use crate::output;
use crate::resolve;

pub fn add_entry(
    dir: &Path,
    config: &Config,
    key: &str,
    provided_value: Option<String>,
    backend_override: Option<&str>,
) -> Result<()> {
    validate_key(key)?;

    let env_update = plan_env_entry_update(dir, key)?;
    let value = read_secret_value(key, provided_value)?;
    let effective_config = config.with_backend_override_for_dir(dir, backend_override);
    let backend_name = effective_config.effective_backend(dir);
    let backend = backend::create_backend(backend_name)?;
    let project = resolve::detect_project_name(dir);
    let repository = resolve::detect_repository_remote(dir);
    let store_ctx = StoreContext {
        dir,
        config: &effective_config,
        project: project.clone(),
        repository: repository.clone(),
    };
    let resolve_ctx = ResolveContext {
        dir,
        config: &effective_config,
        project,
        repository,
    };

    info!(key, backend = backend.name(), "Adding managed secret");
    backend
        .store(key, &value, &store_ctx)
        .with_context(|| format!("Failed to store '{key}' in {}", backend.name()))?;

    if !backend.has(key, &resolve_ctx).with_context(|| {
        format!(
            "Failed to verify '{key}' after storing it in {}",
            backend.name()
        )
    })? {
        bail!(
            "Stored '{key}' in {} but verification failed",
            backend.name()
        );
    }

    let env_message = apply_env_entry_update(env_update, key)?;

    eprintln!("Stored {key} in {}.", backend.name());
    eprintln!("{env_message}");

    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum EnvEntryUpdate {
    Create(PathBuf),
    Append(PathBuf),
    AlreadyManaged(PathBuf),
}

fn validate_key(key: &str) -> Result<()> {
    if output::is_valid_env_key(key) {
        return Ok(());
    }

    bail!("Invalid environment variable name '{key}'. Expected [A-Za-z_][A-Za-z0-9_]*")
}

fn read_secret_value(key: &str, provided_value: Option<String>) -> Result<String> {
    let value = match provided_value {
        Some(value) => value,
        None if std::io::stdin().is_terminal() => Password::new()
            .with_prompt(format!("Value for {key}"))
            .with_confirmation("Confirm value", "Values do not match")
            .interact()
            .context("Failed to read secret value")?,
        None => {
            let mut buffer = String::new();
            std::io::stdin()
                .read_to_string(&mut buffer)
                .context("Failed to read secret value from stdin")?;
            trim_single_trailing_newline(buffer)
        }
    };

    if value.is_empty() {
        bail!("Secret value cannot be empty")
    }

    Ok(value)
}

fn trim_single_trailing_newline(mut value: String) -> String {
    if value.ends_with("\r\n") {
        value.truncate(value.len() - 2);
        return value;
    }

    if value.ends_with('\n') {
        value.pop();
    }

    value
}

fn plan_env_entry_update(dir: &Path, key: &str) -> Result<EnvEntryUpdate> {
    let env_path = dir.join(".env");
    if !env_path.exists() {
        return Ok(EnvEntryUpdate::Create(env_path));
    }

    if env_path.is_symlink() {
        bail!(
            "Refusing to update .env symlink at {}. Use a regular file.",
            env_path.display()
        );
    }

    let env_file = env_file::EnvFile::parse(&env_path)?;
    if let Some(existing_entry) = env_file
        .entries()
        .into_iter()
        .find(|entry| entry.key == key)
    {
        match &existing_entry.kind {
            EntryKind::Empty => return Ok(EnvEntryUpdate::AlreadyManaged(env_path)),
            EntryKind::OpReference(_) | EntryKind::BwReference(_) => {
                bail!(
                    "{key} already exists in {} as an explicit backend reference. Clear that value first if you want pw-env add to manage it via the default backend.",
                    env_path.display()
                )
            }
            EntryKind::Plaintext(_) => {
                bail!(
                    "{key} already exists in {} with a plaintext value. Clear it first or run pw-env migrate instead.",
                    env_path.display()
                )
            }
        }
    }

    Ok(EnvEntryUpdate::Append(env_path))
}

fn apply_env_entry_update(update: EnvEntryUpdate, key: &str) -> Result<String> {
    match update {
        EnvEntryUpdate::Create(path) => {
            std::fs::write(&path, format!("{key}=\n"))
                .with_context(|| format!("Failed to create {}", path.display()))?;
            Ok(format!(
                "Created {} with an empty managed entry for {key}.",
                path.display()
            ))
        }
        EnvEntryUpdate::Append(path) => {
            let mut contents = std::fs::read_to_string(&path)
                .with_context(|| format!("Failed to read {}", path.display()))?;
            if !contents.is_empty() && !contents.ends_with('\n') {
                contents.push('\n');
            }
            contents.push_str(&format!("{key}=\n"));
            std::fs::write(&path, contents)
                .with_context(|| format!("Failed to update {}", path.display()))?;
            Ok(format!(
                "Added an empty managed entry for {key} to {}.",
                path.display()
            ))
        }
        EnvEntryUpdate::AlreadyManaged(path) => Ok(format!(
            "{} already contains an empty managed entry for {key}.",
            path.display()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, Defaults, GpgConfig, LogConfig, OpConfig, UpdateConfig};
    use tempfile::TempDir;

    #[test]
    fn trim_single_trailing_newline_removes_lf() {
        assert_eq!(
            trim_single_trailing_newline("secret\n".to_string()),
            "secret"
        );
    }

    #[test]
    fn trim_single_trailing_newline_removes_crlf() {
        assert_eq!(
            trim_single_trailing_newline("secret\r\n".to_string()),
            "secret"
        );
    }

    #[test]
    fn trim_single_trailing_newline_keeps_embedded_newlines() {
        assert_eq!(
            trim_single_trailing_newline("first\nsecond".to_string()),
            "first\nsecond"
        );
    }

    #[test]
    fn plan_env_entry_update_creates_new_env_when_missing() {
        let temp_dir = TempDir::new().unwrap();
        let result = plan_env_entry_update(temp_dir.path(), "API_KEY").unwrap();
        assert_eq!(result, EnvEntryUpdate::Create(temp_dir.path().join(".env")));
    }

    #[test]
    fn plan_env_entry_update_marks_existing_empty_entry_as_managed() {
        let temp_dir = TempDir::new().unwrap();
        let env_path = temp_dir.path().join(".env");
        std::fs::write(&env_path, "API_KEY=\n").unwrap();

        let result = plan_env_entry_update(temp_dir.path(), "API_KEY").unwrap();
        assert_eq!(result, EnvEntryUpdate::AlreadyManaged(env_path));
    }

    #[test]
    fn plan_env_entry_update_rejects_plaintext_entry() {
        let temp_dir = TempDir::new().unwrap();
        std::fs::write(temp_dir.path().join(".env"), "API_KEY=plaintext\n").unwrap();

        let result = plan_env_entry_update(temp_dir.path(), "API_KEY");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("plaintext value"));
    }

    #[test]
    fn apply_env_entry_update_appends_missing_newline() {
        let temp_dir = TempDir::new().unwrap();
        let env_path = temp_dir.path().join(".env");
        std::fs::write(&env_path, "EXISTING_KEY=").unwrap();

        apply_env_entry_update(EnvEntryUpdate::Append(env_path.clone()), "API_KEY").unwrap();

        let contents = std::fs::read_to_string(env_path).unwrap();
        assert_eq!(contents, "EXISTING_KEY=\nAPI_KEY=\n");
    }

    #[test]
    #[cfg(unix)]
    fn add_entry_stores_secret_and_creates_empty_env_entry_for_gpg_backend() {
        let _guard = crate::backend::MOCK_PATH_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp_dir = TempDir::new().unwrap();
        let bin_dir = TempDir::new().unwrap();
        let gpg_path = bin_dir.path().join("gpg");
        let gpg_output_path = temp_dir.path().join("captured-gpg-output.txt");
        let script = format!(
            "#!/bin/sh\nif [ \"$1\" = \"--decrypt\" ]; then\n  cat '{}'\n  exit 0\nfi\nout=''\nprev=''\nfor arg in \"$@\"; do\n  if [ \"$prev\" = \"--output\" ]; then\n    out=\"$arg\"\n    break\n  fi\n  prev=\"$arg\"\ndone\ncat > \"$out\"\ncp \"$out\" '{}'\nexit 0\n",
            gpg_output_path.display(),
            gpg_output_path.display()
        );

        std::fs::write(&gpg_path, script).unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&gpg_path).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&gpg_path, permissions).unwrap();
        }

        let old_path = std::env::var_os("PATH").unwrap_or_default();
        let new_path = std::env::join_paths(
            std::iter::once(bin_dir.path().to_path_buf()).chain(std::env::split_paths(&old_path)),
        )
        .unwrap();
        unsafe { std::env::set_var("PATH", &new_path) };

        let config = Config {
            defaults: Defaults {
                backend: "gpg".to_string(),
                gpg: GpgConfig {
                    file_pattern: ".env.gpg".to_string(),
                    recipient: Some("test@example.com".to_string()),
                },
                ..Defaults::default()
            },
            log: LogConfig::default(),
            updates: UpdateConfig::default(),
            projects: vec![],
        };

        let result = add_entry(
            temp_dir.path(),
            &config,
            "API_KEY",
            Some("super-secret".to_string()),
            None,
        );

        unsafe { std::env::set_var("PATH", &old_path) };

        assert!(result.is_ok(), "expected Ok, got: {result:?}");
        assert_eq!(
            std::fs::read_to_string(temp_dir.path().join(".env")).unwrap(),
            "API_KEY=\n"
        );
        assert!(temp_dir.path().join(".env.gpg").exists());

        let encrypted_payload = std::fs::read_to_string(gpg_output_path).unwrap();
        assert!(encrypted_payload.contains("API_KEY=super-secret"));
    }

    #[test]
    #[cfg(unix)]
    fn add_entry_backend_override_uses_matching_backend_item_config() {
        let _guard = crate::backend::MOCK_PATH_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp_dir = TempDir::new().unwrap();
        let bin_dir = TempDir::new().unwrap();
        let op_path = bin_dir.path().join("op");
        let op_log_path = temp_dir.path().join("op-invocation.txt");
        let script = format!(
            "#!/bin/sh\nprintf '%s\n' \"$*\" > '{}'\nif [ \"$1\" = \"item\" ] && [ \"$2\" = \"edit\" ] && [ \"$3\" = \"shared-item\" ]; then\n  exit 0\nfi\nif [ \"$1\" = \"item\" ] && [ \"$2\" = \"get\" ] && [ \"$3\" = \"shared-item\" ] && [ \"$4\" = \"--fields\" ] && [ \"$5\" = \"label=API_KEY\" ] && [ \"$6\" = \"--reveal\" ]; then\n  printf 'super-secret'\n  exit 0\nfi\nprintf 'unexpected args: %s\n' \"$*\" >&2\nexit 1\n",
            op_log_path.display()
        );

        std::fs::write(&op_path, script).unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&op_path).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&op_path, permissions).unwrap();
        }

        let old_path = std::env::var_os("PATH").unwrap_or_default();
        let new_path = std::env::join_paths(
            std::iter::once(bin_dir.path().to_path_buf()).chain(std::env::split_paths(&old_path)),
        )
        .unwrap();
        unsafe { std::env::set_var("PATH", &new_path) };

        let config = Config {
            defaults: Defaults {
                backend: "bw".to_string(),
                op: OpConfig {
                    item: Some("shared-item".to_string()),
                    ..OpConfig::default()
                },
                ..Defaults::default()
            },
            log: LogConfig::default(),
            updates: UpdateConfig::default(),
            projects: vec![],
        };

        let result = add_entry(
            temp_dir.path(),
            &config,
            "API_KEY",
            Some("super-secret".to_string()),
            Some("op"),
        );

        unsafe { std::env::set_var("PATH", &old_path) };

        assert!(result.is_ok(), "expected Ok, got: {result:?}");
        assert_eq!(
            std::fs::read_to_string(temp_dir.path().join(".env")).unwrap(),
            "API_KEY=\n"
        );
        let logged_args = std::fs::read_to_string(op_log_path).unwrap();
        assert!(
            logged_args.starts_with("item get shared-item --fields label=API_KEY --reveal"),
            "unexpected op args: {logged_args}"
        );
    }
}
