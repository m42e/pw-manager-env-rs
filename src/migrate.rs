use anyhow::Result;
use std::io::{self, Write};
use std::path::Path;
use tracing::{info, warn};

use crate::backend::{self, ResolveContext, StoreContext};
use crate::config::Config;
use crate::env_file::EnvFile;

/// Run the migration process: detect plaintext secrets in .env, offer to store them
/// in the configured password backend, then rewrite .env to clear them.
pub fn migrate(dir: &Path, config: &Config) -> Result<()> {
    let env_path = EnvFile::find(dir)
        .ok_or_else(|| anyhow::anyhow!("No .env file found in {}", dir.display()))?;
    let env_file = EnvFile::parse(&env_path)?;
    let plaintext_entries = env_file.plaintext_entries();

    if plaintext_entries.is_empty() {
        eprintln!("No plaintext values found in .env — nothing to migrate.");
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
    eprintln!(
        "These will be stored in the '{}' backend.",
        backend_name
    );

    let backend = backend::create_backend(backend_name)?;
    let store_ctx = StoreContext { dir, config };
    let resolve_ctx = ResolveContext { dir, config, project: None };
    let mut migrated_keys: Vec<&str> = Vec::new();

    for entry in &plaintext_entries {
        eprint!(
            "Store '{}' in {}? [y/N/q] ",
            entry.key,
            backend.name()
        );
        io::stderr().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let answer = input.trim().to_lowercase();

        match answer.as_str() {
            "y" | "yes" => {
                // Extract the actual value (strip quotes)
                let value = strip_quotes(&entry.raw_value);
                info!("Storing '{}' in {}", entry.key, backend.name());

                match backend.store(&entry.key, &value, &store_ctx) {
                    Ok(()) => {
                        // Verify the value was stored
                        match backend.has(&entry.key, &resolve_ctx) {
                            Ok(true) => {
                                eprintln!("  Stored and verified: {}", entry.key);
                                migrated_keys.push(&entry.key);
                            }
                            Ok(false) => {
                                warn!("Stored '{}' but verification failed — keeping in .env", entry.key);
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
                        }
                    }
                    Err(e) => {
                        warn!("Failed to store '{}': {e}", entry.key);
                        eprintln!("  Error storing '{}': {e}", entry.key);
                    }
                }
            }
            "q" | "quit" => {
                eprintln!("Migration aborted.");
                break;
            }
            _ => {
                eprintln!("  Skipped: {}", entry.key);
            }
        }
    }

    if !migrated_keys.is_empty() {
        eprintln!();
        eprintln!(
            "Clearing {} migrated value(s) from .env...",
            migrated_keys.len()
        );
        env_file.rewrite_with_cleared_keys(&migrated_keys)?;
        info!(
            "Cleared {} migrated values from .env",
            migrated_keys.len()
        );
        eprintln!("Done. Migrated values have been removed from .env.");
    }

    Ok(())
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
