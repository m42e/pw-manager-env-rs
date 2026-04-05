use anyhow::{Context, Result};
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use tracing::debug;

/// Classification of a .env entry's value.
#[derive(Debug, Clone, PartialEq)]
pub enum EntryKind {
    /// Value is empty or a placeholder — resolve from default backend by key name.
    Empty,
    /// Value is a 1Password reference (op://vault/item/field).
    OpReference(String),
    /// Value is a Bitwarden reference (bw://[folder/]item/field).
    BwReference(String),
    /// Value is plaintext — candidate for migration.
    Plaintext(String),
}

/// A parsed .env entry.
#[derive(Debug, Clone)]
pub struct EnvEntry {
    pub key: String,
    pub raw_value: String,
    pub kind: EntryKind,
}

/// All lines from the .env file, preserving comments and blanks for rewriting.
#[derive(Debug, Clone)]
pub enum EnvLine {
    /// A comment or blank line (preserved as-is).
    Comment(String),
    /// A key=value entry.
    Entry(EnvEntry),
}

/// Parsed .env file.
pub struct EnvFile {
    pub path: PathBuf,
    pub lines: Vec<EnvLine>,
}

impl EnvFile {
    /// Find the .env file in the given directory.
    pub fn find(dir: &Path) -> Option<PathBuf> {
        let env_path = dir.join(".env");
        if env_path.exists() {
            Some(env_path)
        } else {
            None
        }
    }

    /// Parse a .env file, classifying each entry.
    pub fn parse(path: &Path) -> Result<Self> {
        debug!("Parsing .env file: {}", path.display());
        let file = std::fs::File::open(path)
            .with_context(|| format!("Failed to open .env file: {}", path.display()))?;
        let reader = BufReader::new(file);
        let mut lines = Vec::new();

        for line_result in reader.lines() {
            let line = line_result.context("Failed to read line from .env file")?;
            let trimmed = line.trim();

            if trimmed.is_empty() || trimmed.starts_with('#') {
                lines.push(EnvLine::Comment(line));
                continue;
            }

            if let Some((key, value)) = trimmed.split_once('=') {
                let key = key.trim().to_string();
                let value = value.trim().to_string();

                // Strip surrounding quotes for classification, but keep raw_value as-is
                let unquoted = strip_quotes(&value);
                let kind = classify_value(&unquoted);

                lines.push(EnvLine::Entry(EnvEntry {
                    key,
                    raw_value: value,
                    kind,
                }));
            } else {
                // Lines without '=' are treated as comments/passthrough
                lines.push(EnvLine::Comment(line));
            }
        }

        Ok(EnvFile {
            path: path.to_path_buf(),
            lines,
        })
    }

    /// Get all entries (filtering out comments).
    pub fn entries(&self) -> Vec<&EnvEntry> {
        self.lines
            .iter()
            .filter_map(|l| match l {
                EnvLine::Entry(e) => Some(e),
                _ => None,
            })
            .collect()
    }

    /// Get entries that have plaintext values (migration candidates).
    pub fn plaintext_entries(&self) -> Vec<&EnvEntry> {
        self.entries()
            .into_iter()
            .filter(|e| matches!(e.kind, EntryKind::Plaintext(_)))
            .collect()
    }

    /// Get plaintext entries that are likely to contain secrets.
    pub fn likely_secret_entries(&self) -> Vec<&EnvEntry> {
        self.entries()
            .into_iter()
            .filter(|e| e.is_likely_secret())
            .collect()
    }

    /// Get entries that need resolution (empty or reference values).
    pub fn resolvable_entries(&self) -> Vec<&EnvEntry> {
        self.entries()
            .into_iter()
            .filter(|e| !matches!(e.kind, EntryKind::Plaintext(_)))
            .collect()
    }

    /// Rewrite the .env file, replacing specific keys' values.
    /// Used after migration to clear plaintext values.
    pub fn rewrite_with_cleared_keys(&self, keys_to_clear: &[&str]) -> Result<()> {
        let mut output = String::new();
        for line in &self.lines {
            match line {
                EnvLine::Comment(c) => {
                    output.push_str(c);
                    output.push('\n');
                }
                EnvLine::Entry(entry) => {
                    if keys_to_clear.contains(&entry.key.as_str()) {
                        // Write key with empty value
                        output.push_str(&format!("{}=\n", entry.key));
                    } else {
                        output.push_str(&format!("{}={}\n", entry.key, entry.raw_value));
                    }
                }
            }
        }
        std::fs::write(&self.path, output)
            .with_context(|| format!("Failed to rewrite .env file: {}", self.path.display()))?;
        debug!("Rewrote .env file: {}", self.path.display());
        Ok(())
    }

}

impl EnvEntry {
    pub fn is_likely_secret(&self) -> bool {
        match &self.kind {
            EntryKind::Plaintext(value) => is_likely_secret(&self.key, value),
            _ => false,
        }
    }
}

fn strip_quotes(value: &str) -> String {
    if (value.starts_with('"') && value.ends_with('"'))
        || (value.starts_with('\'') && value.ends_with('\''))
    {
        value[1..value.len() - 1].to_string()
    } else {
        value.to_string()
    }
}

fn classify_value(value: &str) -> EntryKind {
    if value.is_empty() {
        EntryKind::Empty
    } else if value.starts_with("op://") {
        EntryKind::OpReference(value.to_string())
    } else if value.starts_with("bw://") {
        EntryKind::BwReference(value.to_string())
    } else {
        EntryKind::Plaintext(value.to_string())
    }
}

fn is_likely_secret(key: &str, value: &str) -> bool {
    looks_like_secret_name(key)
        || contains_embedded_credentials(value)
        || has_common_secret_prefix(value)
        || looks_like_high_entropy_secret(value)
}

fn looks_like_secret_name(key: &str) -> bool {
    let normalized: String = key
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();

    let direct_patterns = [
        "password",
        "passwd",
        "passphrase",
        "secret",
        "token",
        "api_key",
        "apikey",
        "auth_token",
        "refresh_token",
        "client_secret",
        "secret_key",
        "private_key",
        "access_key",
        "jwt",
    ];

    if direct_patterns
        .iter()
        .any(|pattern| normalized.contains(pattern))
    {
        return true;
    }

    let segments: Vec<&str> = normalized
        .split('_')
        .filter(|segment| !segment.is_empty())
        .collect();

    contains_segment_pair(&segments, "api", "key")
        || contains_segment_pair(&segments, "auth", "key")
        || contains_segment_pair(&segments, "auth", "token")
        || contains_segment_pair(&segments, "client", "secret")
        || contains_segment_pair(&segments, "refresh", "token")
        || contains_segment_pair(&segments, "private", "key")
        || contains_segment_pair(&segments, "access", "key")
}

fn contains_segment_pair(segments: &[&str], first: &str, second: &str) -> bool {
    segments.windows(2).any(|window| window == [first, second])
}

fn contains_embedded_credentials(value: &str) -> bool {
    let trimmed = value.trim();
    let Some((_, remainder)) = trimmed.split_once("://") else {
        return false;
    };

    let authority = remainder.split('/').next().unwrap_or(remainder);
    let Some((userinfo, _)) = authority.rsplit_once('@') else {
        return false;
    };

    let Some((username, password)) = userinfo.split_once(':') else {
        return false;
    };

    !username.is_empty() && !password.is_empty()
}

fn has_common_secret_prefix(value: &str) -> bool {
    let lower = value.trim().to_ascii_lowercase();
    lower.starts_with("ghp_")
        || lower.starts_with("github_pat_")
        || lower.starts_with("glpat-")
        || lower.starts_with("xoxb-")
        || lower.starts_with("xoxp-")
        || lower.starts_with("xoxs-")
        || lower.starts_with("sk-")
        || value.trim().starts_with("AKIA")
}

fn looks_like_high_entropy_secret(value: &str) -> bool {
    let trimmed = value.trim();
    if trimmed.len() < 16 || trimmed.chars().any(char::is_whitespace) || trimmed.contains("://") {
        return false;
    }

    if !trimmed.chars().all(|ch| {
        ch.is_ascii_alphanumeric() || matches!(ch, '/' | '+' | '=' | '-' | '_' | '.')
    }) {
        return false;
    }

    let entropy = shannon_entropy(trimmed);
    (trimmed.len() >= 20 && entropy >= 3.8) || (trimmed.len() >= 32 && entropy >= 3.4)
}

fn shannon_entropy(value: &str) -> f64 {
    let mut counts: HashMap<u8, usize> = HashMap::new();
    for byte in value.bytes() {
        *counts.entry(byte).or_insert(0) += 1;
    }

    let len = value.len() as f64;
    counts
        .values()
        .map(|count| {
            let probability = *count as f64 / len;
            -probability * probability.log2()
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_empty() {
        assert_eq!(classify_value(""), EntryKind::Empty);
    }

    #[test]
    fn test_classify_op_reference() {
        let val = "op://Private/MyApp/api-key";
        assert_eq!(
            classify_value(val),
            EntryKind::OpReference(val.to_string())
        );
    }

    #[test]
    fn test_classify_bw_reference() {
        let val = "bw://env-secrets/myapp/password";
        assert_eq!(
            classify_value(val),
            EntryKind::BwReference(val.to_string())
        );
    }

    #[test]
    fn test_classify_plaintext() {
        assert_eq!(
            classify_value("my-secret-value"),
            EntryKind::Plaintext("my-secret-value".to_string())
        );
    }

    #[test]
    fn test_strip_quotes() {
        assert_eq!(strip_quotes("\"hello\""), "hello");
        assert_eq!(strip_quotes("'hello'"), "hello");
        assert_eq!(strip_quotes("hello"), "hello");
        assert_eq!(strip_quotes("\"hello"), "\"hello");
    }

    #[test]
    fn test_detects_secret_by_name() {
        let entry = EnvEntry {
            key: "API_KEY".to_string(),
            raw_value: "dev-value".to_string(),
            kind: EntryKind::Plaintext("dev-value".to_string()),
        };

        assert!(entry.is_likely_secret());
    }

    #[test]
    fn test_detects_secret_by_entropy() {
        let entry = EnvEntry {
            key: "SESSION".to_string(),
            raw_value: "Qx9Lpm7_aB2Nz8VwK4rTy1Hu".to_string(),
            kind: EntryKind::Plaintext("Qx9Lpm7_aB2Nz8VwK4rTy1Hu".to_string()),
        };

        assert!(entry.is_likely_secret());
    }

    #[test]
    fn test_detects_secret_in_url_credentials() {
        let entry = EnvEntry {
            key: "DATABASE_URL".to_string(),
            raw_value: "postgres://app:s3cr3t@db.internal/app".to_string(),
            kind: EntryKind::Plaintext("postgres://app:s3cr3t@db.internal/app".to_string()),
        };

        assert!(entry.is_likely_secret());
    }

    #[test]
    fn test_ignores_plaintext_non_secret_setting() {
        let entry = EnvEntry {
            key: "LOG_LEVEL".to_string(),
            raw_value: "debug".to_string(),
            kind: EntryKind::Plaintext("debug".to_string()),
        };

        assert!(!entry.is_likely_secret());
    }

    #[test]
    fn test_filters_likely_secret_entries() {
        let env_file = EnvFile {
            path: PathBuf::from(".env"),
            lines: vec![
                EnvLine::Entry(EnvEntry {
                    key: "API_KEY".to_string(),
                    raw_value: "dev-value".to_string(),
                    kind: EntryKind::Plaintext("dev-value".to_string()),
                }),
                EnvLine::Entry(EnvEntry {
                    key: "LOG_LEVEL".to_string(),
                    raw_value: "debug".to_string(),
                    kind: EntryKind::Plaintext("debug".to_string()),
                }),
            ],
        };

        let detected: Vec<&str> = env_file
            .likely_secret_entries()
            .into_iter()
            .map(|entry| entry.key.as_str())
            .collect();

        assert_eq!(detected, vec!["API_KEY"]);
    }
}
