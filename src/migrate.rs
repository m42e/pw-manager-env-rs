use anyhow::{Context, Result};
use dialoguer::{MultiSelect, theme::ColorfulTheme};
use std::collections::BTreeSet;
use std::io::IsTerminal;
use std::path::Path;
use tracing::{info, warn};

use crate::backend::{self, ResolveContext, StoreContext};
use crate::config::Config;
use crate::env_file::EnvFile;
use crate::resolve;

#[cfg(test)]
thread_local! {
    static MOCK_INTERACTIVE: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
    static MOCK_PROMPT_RESULT: std::cell::RefCell<Option<BTreeSet<usize>>> = const { std::cell::RefCell::new(None) };
}

/// Run the migration process: detect plaintext secrets in .env, offer to store them
/// in the configured password backend, then rewrite .env to clear them.
pub fn migrate(dir: &Path, config: &Config, backend_override: Option<&str>) -> Result<()> {
    let effective_config = config_for_migration(config, dir, backend_override);
    let env_path =
        EnvFile::find_with_parents(dir, effective_config.effective_search_parent_env(dir))
            .ok_or_else(|| anyhow::anyhow!("No .env file found in {}", dir.display()))?;
    let env_file = EnvFile::parse(&env_path)?;
    let plaintext_entries = env_file.plaintext_entries();

    if plaintext_entries.is_empty() {
        eprintln!("No plaintext values found in .env that require migration.");
        return Ok(());
    }

    let likely_secret_count = plaintext_entries
        .iter()
        .filter(|entry| entry.is_likely_secret())
        .count();

    eprintln!(
        "Found {} plaintext value(s) in {}:",
        plaintext_entries.len(),
        env_path.display()
    );
    if likely_secret_count > 0 {
        eprintln!(
            "{} of them look like secrets based on key names or secret-like values.",
            likely_secret_count
        );
    }
    for entry in &plaintext_entries {
        // Show key and a masked value (first 3 chars + ***)
        let masked = mask_value(&entry.raw_value);
        let label = if entry.is_likely_secret() {
            "  [likely secret]"
        } else {
            ""
        };
        eprintln!("  {} = {}{}", entry.key, masked, label);
    }
    eprintln!();

    let backend_name = effective_config.effective_backend(dir);
    eprintln!("These will be stored in the '{}' backend.", backend_name);

    if !is_interactive() {
        anyhow::bail!("pw-env migrate requires an interactive terminal to select entries");
    }

    let selected_indexes = prompt_for_entries(&plaintext_entries, backend_name)?;
    if selected_indexes.is_empty() {
        eprintln!("No entries selected for migration.");
    }

    let selected_fingerprints = plaintext_entries
        .iter()
        .enumerate()
        .filter(|(index, _)| selected_indexes.contains(index))
        .filter_map(|(_, entry)| entry.review_fingerprint())
        .collect::<Vec<_>>();
    let skipped_fingerprints = plaintext_entries
        .iter()
        .enumerate()
        .filter(|(index, _)| !selected_indexes.contains(index))
        .filter_map(|(_, entry)| entry.review_fingerprint())
        .collect::<Vec<_>>();

    Config::forget_reviewed_migration_entries(&env_path, selected_fingerprints)?;

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
    let mut migrated_keys: Vec<&str> = Vec::new();

    for (index, entry) in plaintext_entries.iter().enumerate() {
        if !selected_indexes.contains(&index) {
            eprintln!("  Kept in .env: {}", entry.key);
            continue;
        }

        let value = strip_quotes(&entry.raw_value);
        info!("Storing '{}' in {}", entry.key, backend.name());

        match backend.store(&entry.key, &value, &store_ctx) {
            Ok(()) => match backend.has(&entry.key, &resolve_ctx) {
                Ok(true) => {
                    eprintln!("  Stored and verified: {}", entry.key);
                    migrated_keys.push(&entry.key);
                }
                Ok(false) => {
                    warn!(
                        "Stored '{}' but verification failed — keeping in .env",
                        entry.key
                    );
                    eprintln!(
                        "  Warning: stored '{}' but could not verify. Keeping in .env.",
                        entry.key
                    );
                }
                Err(e) => {
                    warn!("Verification error for '{}': {e}", entry.key);
                    eprintln!(
                        "  Warning: verification error for '{}': {e}. Keeping in .env.",
                        entry.key
                    );
                }
            },
            Err(e) => {
                warn!("Failed to store '{}': {e}", entry.key);
                eprintln!("  Error storing '{}': {e}", entry.key);
            }
        }
    }

    Config::remember_reviewed_migration_entries(&env_path, skipped_fingerprints)?;

    if !migrated_keys.is_empty() {
        eprintln!();
        eprintln!(
            "Clearing {} migrated value(s) from .env...",
            migrated_keys.len()
        );
        env_file.rewrite_with_cleared_keys(&migrated_keys)?;
        info!("Cleared {} migrated values from .env", migrated_keys.len());
        eprintln!("Done. Migrated values have been removed from .env.");
    }

    Ok(())
}

fn config_for_migration(config: &Config, dir: &Path, backend_override: Option<&str>) -> Config {
    config.with_backend_override_for_dir(dir, backend_override)
}

fn is_interactive() -> bool {
    #[cfg(test)]
    if let Some(val) = MOCK_INTERACTIVE.with(|c| c.get()) {
        return val;
    }
    is_interactive_check(
        cfg!(not(test)),
        std::io::stdin().is_terminal(),
        std::io::stderr().is_terminal(),
    )
}

fn is_interactive_check(not_test: bool, stdin_terminal: bool, stderr_terminal: bool) -> bool {
    not_test && stdin_terminal && stderr_terminal
}

fn mask_value(value: &str) -> String {
    let v = strip_quotes(value);
    let char_count = v.chars().count();
    if char_count <= 3 {
        "***".to_string()
    } else {
        let prefix: String = v.chars().take(3).collect();
        format!("{prefix}***")
    }
}

fn strip_quotes(value: &str) -> String {
    let v = value.trim();
    if (v.starts_with('"') && v.ends_with('"')) || (v.starts_with('\'') && v.ends_with('\'')) {
        let mut chars = v.chars();
        chars.next(); // skip opening quote
        chars.next_back(); // skip closing quote
        chars.collect()
    } else {
        v.to_string()
    }
}

fn prompt_for_entries(
    entries: &[&crate::env_file::EnvEntry],
    backend_name: &str,
) -> Result<BTreeSet<usize>> {
    #[cfg(test)]
    if let Some(result) = MOCK_PROMPT_RESULT.with(|r| r.borrow().clone()) {
        return Ok(result);
    }

    let items = entries
        .iter()
        .map(|entry| {
            let masked = mask_value(&entry.raw_value);
            let label = if entry.is_likely_secret() {
                " [likely secret]"
            } else {
                ""
            };
            format!("{} = {}{}", entry.key, masked, label)
        })
        .collect::<Vec<_>>();

    let defaults = entries
        .iter()
        .map(|entry| entry.is_likely_secret())
        .collect::<Vec<_>>();

    let selected = MultiSelect::with_theme(&ColorfulTheme::default())
        .with_prompt(format!(
            "Select the plaintext entries to store in the '{}' backend",
            backend_name
        ))
        .items(&items)
        .defaults(&defaults)
        .report(false)
        .interact()
        .context("Migration selection was interrupted")?;

    Ok(selected.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProjectOverride;
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;

    #[test]
    fn mask_value_returns_stars_for_short_values() {
        assert_eq!(mask_value(""), "***");
        assert_eq!(mask_value("ab"), "***");
        assert_eq!(mask_value("abc"), "***");
    }

    #[test]
    fn mask_value_shows_prefix_for_longer_values() {
        assert_eq!(mask_value("abcd"), "abc***");
        assert_eq!(mask_value("abcdef"), "abc***");
    }

    #[test]
    fn mask_value_strips_quotes_before_masking() {
        assert_eq!(mask_value("\"abcdef\""), "abc***");
        assert_eq!(mask_value("'abcdef'"), "abc***");
    }

    #[test]
    fn strip_quotes_removes_double_quotes() {
        assert_eq!(strip_quotes("\"hello\""), "hello");
    }

    #[test]
    fn strip_quotes_removes_single_quotes() {
        assert_eq!(strip_quotes("'hello'"), "hello");
    }

    #[test]
    fn strip_quotes_leaves_unquoted_value() {
        assert_eq!(strip_quotes("hello"), "hello");
    }

    #[test]
    fn strip_quotes_leaves_mismatched_quotes() {
        assert_eq!(strip_quotes("\"hello'"), "\"hello'");
        assert_eq!(strip_quotes("'hello\""), "'hello\"");
    }

    #[test]
    fn strip_quotes_trims_surrounding_whitespace() {
        assert_eq!(strip_quotes("  \"hello\"  "), "hello");
        assert_eq!(strip_quotes("  'hello'  "), "hello");
    }

    #[test]
    fn strip_quotes_preserves_inner_content_exactly() {
        // Verifies the slice bounds use subtraction (not division or addition).
        assert_eq!(strip_quotes("\"hello world\""), "hello world");
        assert_eq!(strip_quotes("'it\\'s here'"), "it\\'s here");
    }

    #[test]
    fn migrate_returns_err_when_no_env_file() {
        let temp_dir = TempDir::new().unwrap();
        let config = crate::config::Config {
            defaults: crate::config::Defaults::default(),
            log: crate::config::LogConfig::default(),
            updates: crate::config::UpdateConfig::default(),
            projects: vec![],
        };
        let result = migrate(temp_dir.path(), &config, None);
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("No .env file found"));
    }

    #[test]
    fn migrate_returns_ok_with_no_plaintext_entries() {
        let temp_dir = TempDir::new().unwrap();
        let env_path = temp_dir.path().join(".env");
        // An env file with only empty/op/bw entries — no plaintext
        fs::write(&env_path, "API_KEY=op://vault/item/field\nDB_URL=\n").unwrap();
        let config = crate::config::Config {
            defaults: crate::config::Defaults::default(),
            log: crate::config::LogConfig::default(),
            updates: crate::config::UpdateConfig::default(),
            projects: vec![],
        };
        let result = migrate(temp_dir.path(), &config, None);
        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
    }

    #[test]
    fn migrate_bails_with_plaintext_and_non_interactive_stdin() {
        let temp_dir = TempDir::new().unwrap();
        let env_path = temp_dir.path().join(".env");
        // Plaintext entry that looks like a secret
        fs::write(
            &env_path,
            "API_KEY=super_secret_value_that_is_long_enough\n",
        )
        .unwrap();
        let config = crate::config::Config {
            defaults: crate::config::Defaults::default(),
            log: crate::config::LogConfig::default(),
            updates: crate::config::UpdateConfig::default(),
            projects: vec![],
        };
        // In test env, stdin is not a terminal, so this should bail
        let result = migrate(temp_dir.path(), &config, None);
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("interactive terminal"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn config_for_migration_overrides_default_backend() {
        let dir = Path::new("/home/user/project");
        let config = crate::config::Config {
            defaults: crate::config::Defaults {
                backend: "op".to_string(),
                ..crate::config::Defaults::default()
            },
            log: crate::config::LogConfig::default(),
            updates: crate::config::UpdateConfig::default(),
            projects: vec![],
        };

        let effective_config = config_for_migration(&config, dir, Some("gpg"));

        assert_eq!(effective_config.effective_backend(dir), "gpg");
        assert_eq!(config.effective_backend(dir), "op");
    }

    #[test]
    fn config_for_migration_overrides_project_backend() {
        let dir = Path::new("/home/user/project");
        let config = crate::config::Config {
            defaults: crate::config::Defaults::default(),
            log: crate::config::LogConfig::default(),
            updates: crate::config::UpdateConfig::default(),
            projects: vec![ProjectOverride {
                path: dir.to_string_lossy().to_string(),
                backend: Some("bw".to_string()),
                ..ProjectOverride::default()
            }],
        };

        let effective_config = config_for_migration(&config, dir, Some("gpg"));

        assert_eq!(effective_config.effective_backend(dir), "gpg");
        assert_eq!(config.effective_backend(dir), "bw");
    }

    #[test]
    fn test_mask_value_short() {
        assert_eq!(mask_value("ab"), "***");
    }

    #[test]
    fn test_mask_value_exactly_three_chars() {
        assert_eq!(mask_value("abc"), "***");
    }

    #[test]
    fn test_mask_value_longer_than_three() {
        assert_eq!(mask_value("abcdef"), "abc***");
    }

    #[test]
    fn test_mask_value_quoted_double() {
        // Quotes are stripped before masking
        assert_eq!(mask_value("\"secretvalue\""), "sec***");
    }

    #[test]
    fn test_mask_value_quoted_single() {
        assert_eq!(mask_value("'mysecret'"), "mys***");
    }

    #[test]
    fn test_strip_quotes_double_quoted() {
        assert_eq!(strip_quotes("\"hello\""), "hello");
    }

    #[test]
    fn test_strip_quotes_single_quoted() {
        assert_eq!(strip_quotes("'hello'"), "hello");
    }

    #[test]
    fn test_strip_quotes_unquoted() {
        assert_eq!(strip_quotes("hello"), "hello");
    }

    #[test]
    fn test_strip_quotes_trims_surrounding_whitespace() {
        assert_eq!(strip_quotes("  hello  "), "hello");
    }

    #[test]
    fn test_strip_quotes_mismatched_not_stripped() {
        // Mismatched quotes: starts with " but ends with '
        assert_eq!(strip_quotes("\"hello'"), "\"hello'");
    }

    #[test]
    fn test_strip_quotes_single_char_between_quotes() {
        assert_eq!(strip_quotes("\"a\""), "a");
    }

    #[test]
    fn mask_value_handles_multibyte_utf8() {
        // Emoji are 4 bytes each — this must not panic on byte slicing
        assert_eq!(mask_value("😀😁😂😃"), "😀😁😂***");
    }

    #[test]
    fn strip_quotes_handles_multibyte_utf8() {
        assert_eq!(strip_quotes("\"héllo\""), "héllo");
        assert_eq!(strip_quotes("'日本語'"), "日本語");
    }

    #[test]
    fn mask_value_multibyte_short_returns_stars() {
        // A single emoji (4 bytes but 1 char) is ≤3 chars
        assert_eq!(mask_value("😀"), "***");
        // Three emoji (12 bytes but 3 chars) is ≤3 chars
        assert_eq!(mask_value("😀😁😂"), "***");
    }

    #[test]
    fn is_interactive_check_requires_all_true() {
        assert!(!is_interactive_check(false, true, true));
        assert!(!is_interactive_check(true, false, true));
        assert!(!is_interactive_check(true, true, false));
        assert!(is_interactive_check(true, true, true));
        assert!(!is_interactive_check(false, false, false));
    }

    fn set_mock_interactive(val: bool) {
        MOCK_INTERACTIVE.with(|c| c.set(Some(val)));
    }

    fn clear_mock_interactive() {
        MOCK_INTERACTIVE.with(|c| c.set(None));
    }

    fn set_mock_prompt(indexes: BTreeSet<usize>) {
        MOCK_PROMPT_RESULT.with(|r| *r.borrow_mut() = Some(indexes));
    }

    fn clear_mock_prompt() {
        MOCK_PROMPT_RESULT.with(|r| *r.borrow_mut() = None);
    }

    fn with_mock_op_backend<F: FnOnce()>(script: &str, f: F) {
        let _guard = crate::backend::MOCK_PATH_MUTEX
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let dir = TempDir::new().unwrap();
        let script_path = dir.path().join("op");
        fs::write(&script_path, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&script_path).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&script_path, perms).unwrap();
        }
        let old_path = std::env::var_os("PATH").unwrap_or_default();
        let new_path = std::env::join_paths(
            std::iter::once(dir.path().to_path_buf()).chain(std::env::split_paths(&old_path)),
        )
        .unwrap();
        unsafe { std::env::set_var("PATH", &new_path) };
        f();
        unsafe { std::env::set_var("PATH", &old_path) };
    }

    #[test]
    fn migrate_selects_and_stores_chosen_entries() {
        let temp_dir = TempDir::new().unwrap();
        let env_path = temp_dir.path().join(".env");
        // Two plaintext entries: one selected (index 0), one skipped (index 1)
        fs::write(
            &env_path,
            "SECRET_KEY=super_secret_long_value_here\nOTHER_VAL=another_long_plaintext_value\n",
        )
        .unwrap();

        let reviewed_dir = TempDir::new().unwrap();
        crate::config::set_test_reviewed_migrations_path(Some(
            reviewed_dir.path().join("reviewed-migrations.json"),
        ));

        let config = crate::config::Config {
            defaults: crate::config::Defaults {
                backend: "op".to_string(),
                op: crate::config::OpConfig {
                    vault: Some("TestVault".to_string()),
                    ..crate::config::OpConfig::default()
                },
                ..crate::config::Defaults::default()
            },
            log: crate::config::LogConfig::default(),
            updates: crate::config::UpdateConfig::default(),
            projects: vec![],
        };

        set_mock_interactive(true);
        // Select only the first entry (SECRET_KEY at index 0)
        set_mock_prompt(BTreeSet::from([0]));

        // Mock op that succeeds for all operations (create, get/verify)
        let script = r#"#!/bin/sh
# Handle any op command with success
echo "mock-value"
exit 0
"#;

        with_mock_op_backend(script, || {
            let result = migrate(temp_dir.path(), &config, None);
            assert!(result.is_ok(), "migration failed: {:?}", result);

            // The .env file should have SECRET_KEY cleared (migrated)
            // and OTHER_VAL preserved (skipped)
            let content = fs::read_to_string(&env_path).unwrap();
            assert!(
                content.contains("SECRET_KEY="),
                "SECRET_KEY should still be present as a key"
            );
            // The migrated entry should have its value cleared
            assert!(
                !content.contains("super_secret_long_value_here"),
                "migrated value should be cleared from .env"
            );
            // The skipped entry should be preserved
            assert!(
                content.contains("another_long_plaintext_value"),
                "skipped entry value should be preserved"
            );
        });

        clear_mock_interactive();
        clear_mock_prompt();
        crate::config::set_test_reviewed_migrations_path(None);
    }

    #[test]
    fn migrate_with_empty_selection_does_not_rewrite_env() {
        let temp_dir = TempDir::new().unwrap();
        let env_path = temp_dir.path().join(".env");
        let original_content = "PASSWORD=very_secret_long_value\n";
        fs::write(&env_path, original_content).unwrap();

        let reviewed_dir = TempDir::new().unwrap();
        crate::config::set_test_reviewed_migrations_path(Some(
            reviewed_dir.path().join("reviewed-migrations.json"),
        ));

        let config = crate::config::Config {
            defaults: crate::config::Defaults::default(),
            log: crate::config::LogConfig::default(),
            updates: crate::config::UpdateConfig::default(),
            projects: vec![],
        };

        set_mock_interactive(true);
        // Select nothing
        set_mock_prompt(BTreeSet::new());

        // No backend needed since nothing is selected
        let script = "#!/bin/sh\nexit 1\n";
        with_mock_op_backend(script, || {
            let result = migrate(temp_dir.path(), &config, None);
            assert!(result.is_ok(), "migration failed: {:?}", result);

            let content = fs::read_to_string(&env_path).unwrap();
            assert_eq!(content, original_content, ".env should not be modified");
        });

        clear_mock_interactive();
        clear_mock_prompt();
        crate::config::set_test_reviewed_migrations_path(None);
    }
}
