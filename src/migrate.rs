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

/// Run the migration process: detect plaintext secrets in .env, offer to store them
/// in the configured password backend, then rewrite .env to clear them.
pub fn migrate(dir: &Path, config: &Config) -> Result<()> {
    let env_path = EnvFile::find(dir)
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

    let backend_name = config.effective_backend(dir);
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
    let store_ctx = StoreContext {
        dir,
        config,
        project: project.clone(),
    };
    let resolve_ctx = ResolveContext {
        dir,
        config,
        project,
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

fn is_interactive() -> bool {
    cfg!(not(test)) && std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

fn mask_value(value: &str) -> String {
    let v = strip_quotes(value);
    if v.len() <= 3 {
        "***".to_string()
    } else {
        format!("{}***", &v[..3])
    }
}

fn strip_quotes(value: &str) -> String {
    let v = value.trim();
    if (v.starts_with('"') && v.ends_with('"')) || (v.starts_with('\'') && v.ends_with('\'')) {
        v[1..v.len() - 1].to_string()
    } else {
        v.to_string()
    }
}

fn prompt_for_entries(
    entries: &[&crate::env_file::EnvEntry],
    backend_name: &str,
) -> Result<BTreeSet<usize>> {
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
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn migrate_returns_err_when_no_env_file() {
        let temp_dir = TempDir::new().unwrap();
        let config = crate::config::Config {
            defaults: crate::config::Defaults::default(),
            log: crate::config::LogConfig::default(),
            updates: crate::config::UpdateConfig::default(),
            projects: vec![],
        };
        let result = migrate(temp_dir.path(), &config);
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
        let result = migrate(temp_dir.path(), &config);
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
        let result = migrate(temp_dir.path(), &config);
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("interactive terminal"),
            "unexpected error: {msg}"
        );
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
}
