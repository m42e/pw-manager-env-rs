use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
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
    pub trailing_comment: Option<String>,
    pub no_migrate: bool,
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
    /// Rejects symlinks to prevent symlink-based attacks where an attacker
    /// replaces .env with a symlink to a controlled or sensitive file.
    pub fn find(dir: &Path) -> Option<PathBuf> {
        Self::find_with_parents(dir, false)
    }

    /// Find the active .env file for the given directory.
    ///
    /// When `search_parents` is enabled, pw-env walks upward until it finds
    /// the first `.env` file or reaches the enclosing git workspace root. If
    /// nested git markers are present, such as submodules, the search continues
    /// until the highest ancestor git root.
    pub fn find_with_parents(dir: &Path, search_parents: bool) -> Option<PathBuf> {
        let stop_dir = if search_parents {
            topmost_git_root(dir)
        } else {
            Some(dir.to_path_buf())
        };
        let mut current = dir.to_path_buf();

        loop {
            let env_path = current.join(".env");
            if env_path.exists() {
                if env_path.is_symlink() {
                    eprintln!(
                        "pw-env: refusing to follow .env symlink at {}. Use a regular file.",
                        env_path.display()
                    );
                    return None;
                }
                return Some(env_path);
            }

            if stop_dir.as_ref().is_some_and(|stop| stop == &current) {
                return None;
            }

            if !current.pop() {
                return None;
            }
        }
    }

    /// Parse a .env file, classifying each entry.
    pub fn parse(path: &Path) -> Result<Self> {
        if path.is_symlink() {
            anyhow::bail!(
                "Refusing to read .env symlink at {}. Use a regular file.",
                path.display()
            );
        }
        debug!("Parsing .env file: {}", path.display());
        let file = std::fs::File::open(path)
            .with_context(|| format!("Failed to open .env file: {}", path.display()))?;
        let reader = BufReader::new(file);
        let mut lines = Vec::new();
        let mut pending_no_migrate = false;

        for line_result in reader.lines() {
            let line = line_result.context("Failed to read line from .env file")?;
            let trimmed = line.trim();

            if trimmed.is_empty() {
                pending_no_migrate = false;
                lines.push(EnvLine::Comment(line));
                continue;
            }

            if trimmed.starts_with('#') {
                pending_no_migrate = pending_no_migrate || comment_has_no_migrate_marker(trimmed);
                lines.push(EnvLine::Comment(line));
                continue;
            }

            if let Some((key, value, trailing_comment)) = parse_entry_line(&line) {
                let no_migrate = pending_no_migrate
                    || trailing_comment
                        .as_deref()
                        .is_some_and(comment_has_no_migrate_marker);
                pending_no_migrate = false;

                // Strip surrounding quotes for classification, but keep raw_value as-is
                let unquoted = strip_quotes(&value);
                let kind = classify_value(&unquoted);

                lines.push(EnvLine::Entry(EnvEntry {
                    key,
                    raw_value: value,
                    trailing_comment,
                    no_migrate,
                    kind,
                }));
            } else {
                pending_no_migrate = false;
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
            .filter(|e| matches!(e.kind, EntryKind::Plaintext(_)) && !e.no_migrate)
            .collect()
    }

    /// Get plaintext entries that are likely to contain secrets.
    pub fn likely_secret_entries(&self) -> Vec<&EnvEntry> {
        self.entries()
            .into_iter()
            .filter(|e| e.is_likely_secret() && !e.no_migrate)
            .collect()
    }

    pub fn likely_secret_entries_unreviewed<'a>(
        &'a self,
        reviewed_fingerprints: &std::collections::BTreeSet<String>,
    ) -> Vec<&'a EnvEntry> {
        self.likely_secret_entries()
            .into_iter()
            .filter(|entry| match &entry.kind {
                EntryKind::Plaintext(value) => {
                    let fingerprint = review_fingerprint(&entry.key, value);
                    !reviewed_fingerprints.contains(&fingerprint)
                }
                _ => true,
            })
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
                        output.push_str(&format_entry_line(
                            &entry.key,
                            "",
                            entry.trailing_comment.as_deref(),
                        ));
                        output.push('\n');
                    } else {
                        output.push_str(&format_entry_line(
                            &entry.key,
                            &entry.raw_value,
                            entry.trailing_comment.as_deref(),
                        ));
                        output.push('\n');
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

fn topmost_git_root(dir: &Path) -> Option<PathBuf> {
    let mut current = dir.to_path_buf();
    let mut topmost = None;

    loop {
        if current.join(".git").exists() {
            topmost = Some(current.clone());
        }

        if !current.pop() {
            return topmost;
        }
    }
}

impl EnvEntry {
    pub fn is_likely_secret(&self) -> bool {
        match &self.kind {
            EntryKind::Plaintext(value) => is_likely_secret(&self.key, value),
            _ => false,
        }
    }

    pub fn review_fingerprint(&self) -> Option<String> {
        match &self.kind {
            EntryKind::Plaintext(value) => Some(review_fingerprint(&self.key, value)),
            _ => None,
        }
    }
}

fn parse_entry_line(line: &str) -> Option<(String, String, Option<String>)> {
    let trimmed = line.trim();
    let trimmed = trimmed.strip_prefix("export ").unwrap_or(trimmed);
    let (key, raw_value) = trimmed.split_once('=')?;
    let key = key.trim();
    if key.is_empty() {
        return None;
    }

    let (value, trailing_comment) = split_value_and_comment(raw_value);
    Some((key.to_string(), value.trim().to_string(), trailing_comment))
}

fn split_value_and_comment(value: &str) -> (String, Option<String>) {
    let mut in_single_quotes = false;
    let mut in_double_quotes = false;

    for (index, ch) in value.char_indices() {
        match ch {
            '\'' if !in_double_quotes => in_single_quotes = !in_single_quotes,
            '"' if !in_single_quotes => in_double_quotes = !in_double_quotes,
            '#' if !in_single_quotes && !in_double_quotes => {
                let raw_value = value[..index].trim_end().to_string();
                let comment = value[index..].trim_start().to_string();
                return (raw_value, Some(comment));
            }
            _ => {}
        }
    }

    (value.to_string(), None)
}

fn comment_has_no_migrate_marker(comment: &str) -> bool {
    let normalized = comment.trim().trim_start_matches('#').trim();
    let Some((prefix, suffix)) = normalized.split_once(':') else {
        return false;
    };

    if suffix.contains(':') {
        return false;
    }

    prefix.trim().eq_ignore_ascii_case("pw-env") && suffix.trim().eq_ignore_ascii_case("ignore")
}

fn format_entry_line(key: &str, raw_value: &str, trailing_comment: Option<&str>) -> String {
    let mut line = format!("{key}={raw_value}");
    if let Some(comment) = trailing_comment {
        line.push(' ');
        line.push_str(comment);
    }
    line
}

fn strip_quotes(value: &str) -> String {
    if (value.starts_with('"') && value.ends_with('"'))
        || (value.starts_with('\'') && value.ends_with('\''))
    {
        let mut chars = value.chars();
        chars.next();
        chars.next_back();
        chars.collect()
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

    // "auth_token", "client_secret", and "refresh_token" are already matched by the
    // "token" and "secret" direct patterns above, so those segment pairs are omitted.
    contains_segment_pair(&segments, "api", "key")
        || contains_segment_pair(&segments, "auth", "key")
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
    // Reject strings that are too short to satisfy either entropy condition (both
    // require len >= 20).  Whitespace and URL characters are caught below by the
    // alphanumeric-only check, so no redundant early returns are needed.
    if trimmed.len() < 20 {
        return false;
    }

    if !trimmed
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '/' | '+' | '=' | '-' | '_' | '.'))
    {
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

fn review_fingerprint(key: &str, value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(key.as_bytes());
    hasher.update([0]);
    hasher.update(value.trim().as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
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
        assert_eq!(classify_value(val), EntryKind::OpReference(val.to_string()));
    }

    #[test]
    fn test_classify_bw_reference() {
        let val = "bw://env-secrets/myapp/password";
        assert_eq!(classify_value(val), EntryKind::BwReference(val.to_string()));
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
            trailing_comment: None,
            no_migrate: false,
            kind: EntryKind::Plaintext("dev-value".to_string()),
        };

        assert!(entry.is_likely_secret());
    }

    #[test]
    fn test_detects_secret_by_entropy() {
        let entry = EnvEntry {
            key: "SESSION".to_string(),
            raw_value: "Qx9Lpm7_aB2Nz8VwK4rTy1Hu".to_string(),
            trailing_comment: None,
            no_migrate: false,
            kind: EntryKind::Plaintext("Qx9Lpm7_aB2Nz8VwK4rTy1Hu".to_string()),
        };

        assert!(entry.is_likely_secret());
    }

    #[test]
    fn test_detects_secret_in_url_credentials() {
        let entry = EnvEntry {
            key: "DATABASE_URL".to_string(),
            raw_value: "postgres://app:s3cr3t@db.internal/app".to_string(),
            trailing_comment: None,
            no_migrate: false,
            kind: EntryKind::Plaintext("postgres://app:s3cr3t@db.internal/app".to_string()),
        };

        assert!(entry.is_likely_secret());
    }

    #[test]
    fn test_ignores_plaintext_non_secret_setting() {
        let entry = EnvEntry {
            key: "LOG_LEVEL".to_string(),
            raw_value: "debug".to_string(),
            trailing_comment: None,
            no_migrate: false,
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
                    trailing_comment: None,
                    no_migrate: false,
                    kind: EntryKind::Plaintext("dev-value".to_string()),
                }),
                EnvLine::Entry(EnvEntry {
                    key: "LOG_LEVEL".to_string(),
                    raw_value: "debug".to_string(),
                    trailing_comment: None,
                    no_migrate: false,
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

    #[test]
    fn test_filters_reviewed_likely_secret_entries() {
        let env_file = EnvFile {
            path: PathBuf::from(".env"),
            lines: vec![
                EnvLine::Entry(EnvEntry {
                    key: "API_KEY".to_string(),
                    raw_value: "dev-value".to_string(),
                    trailing_comment: None,
                    no_migrate: false,
                    kind: EntryKind::Plaintext("dev-value".to_string()),
                }),
                EnvLine::Entry(EnvEntry {
                    key: "SESSION".to_string(),
                    raw_value: "Qx9Lpm7_aB2Nz8VwK4rTy1Hu".to_string(),
                    trailing_comment: None,
                    no_migrate: false,
                    kind: EntryKind::Plaintext("Qx9Lpm7_aB2Nz8VwK4rTy1Hu".to_string()),
                }),
            ],
        };

        let reviewed = std::collections::BTreeSet::from([match &env_file.lines[0] {
            EnvLine::Entry(entry) => entry.review_fingerprint().unwrap(),
            EnvLine::Comment(_) => panic!("expected env entry"),
        }]);

        let detected: Vec<&str> = env_file
            .likely_secret_entries_unreviewed(&reviewed)
            .into_iter()
            .map(|entry| entry.key.as_str())
            .collect();

        assert_eq!(detected, vec!["SESSION"]);
    }

    #[test]
    fn test_parse_entry_line_splits_inline_comment() {
        let (key, value, trailing_comment) =
            parse_entry_line("API_KEY=secret-value # pw-env:ignore").expect("entry should parse");

        assert_eq!(key, "API_KEY");
        assert_eq!(value, "secret-value");
        assert_eq!(trailing_comment.as_deref(), Some("# pw-env:ignore"));
    }

    #[test]
    fn test_parse_entry_line_keeps_hash_inside_quotes() {
        let (_, value, trailing_comment) =
            parse_entry_line("API_KEY=\"secret#value\" # pw-env:ignore")
                .expect("entry should parse");

        assert_eq!(value, "\"secret#value\"");
        assert_eq!(trailing_comment.as_deref(), Some("# pw-env:ignore"));
    }

    #[test]
    fn test_preceding_no_migrate_comment_marks_next_entry_only() {
        let path =
            write_test_env("# pw-env:ignore\nAPI_KEY=secret-value\nOTHER_KEY=second-value\n");
        let env_file = EnvFile::parse(&path).expect("parse should succeed");
        std::fs::remove_file(&path).expect("temp file should be removable");

        let entries = env_file.entries();
        assert!(entries[0].no_migrate);
        assert!(!entries[1].no_migrate);

        let plaintext_keys: Vec<&str> = env_file
            .plaintext_entries()
            .into_iter()
            .map(|entry| entry.key.as_str())
            .collect();
        assert_eq!(plaintext_keys, vec!["OTHER_KEY"]);
    }

    #[test]
    fn test_blank_line_clears_pending_no_migrate_marker() {
        let path = write_test_env("# pw-env:ignore\n\nAPI_KEY=secret-value\n");
        let env_file = EnvFile::parse(&path).expect("parse should succeed");
        std::fs::remove_file(&path).expect("temp file should be removable");

        let entry = env_file.entries()[0];
        assert!(!entry.no_migrate);
    }

    #[test]
    fn test_rewrite_preserves_inline_comments() {
        let path = write_test_env("KEEP_ME=value # pw-env:ignore\nCLEAR_ME=secret\n");
        let env_file = EnvFile::parse(&path).expect("parse should succeed");
        env_file
            .rewrite_with_cleared_keys(&["CLEAR_ME"])
            .expect("rewrite should succeed");

        let rewritten = std::fs::read_to_string(&path).expect("rewritten file should be readable");
        std::fs::remove_file(&path).expect("temp file should be removable");

        assert_eq!(rewritten, "KEEP_ME=value # pw-env:ignore\nCLEAR_ME=\n");
    }

    #[test]
    fn split_value_and_comment_keeps_hash_inside_single_quotes() {
        let (value, comment) = split_value_and_comment("'secret#value'");
        assert_eq!(value, "'secret#value'");
        assert_eq!(comment, None);
    }

    #[test]
    fn split_value_and_comment_splits_hash_after_single_quoted_value() {
        let (value, comment) = split_value_and_comment("'secret' # annotation");
        assert_eq!(value, "'secret'");
        assert_eq!(comment.as_deref(), Some("# annotation"));
    }

    #[test]
    fn split_value_and_comment_ignores_single_quote_inside_double_quotes() {
        // A single quote inside double quotes must not toggle in_single_quotes.
        // If it did, the closing double-quote would fail its guard and the `#` would
        // not be recognized as the start of a comment.
        let (value, comment) = split_value_and_comment("\"hello'world\" # comment");
        assert_eq!(value, "\"hello'world\"");
        assert_eq!(comment.as_deref(), Some("# comment"));
    }

    #[test]
    fn split_value_and_comment_ignores_double_quote_inside_single_quotes() {
        // A double quote inside single quotes must not toggle in_double_quotes.
        let (value, comment) = split_value_and_comment("'hello\"world' # comment");
        assert_eq!(value, "'hello\"world'");
        assert_eq!(comment.as_deref(), Some("# comment"));
    }

    #[test]
    fn comment_has_no_migrate_marker_returns_false_for_regular_comment() {
        assert!(!comment_has_no_migrate_marker("# some comment"));
        assert!(!comment_has_no_migrate_marker(
            "# description of the variable"
        ));
        assert!(!comment_has_no_migrate_marker("# skip"));
    }

    #[test]
    fn comment_has_no_migrate_marker_returns_true_for_marker() {
        assert!(comment_has_no_migrate_marker("# pw-env:ignore"));
        assert!(comment_has_no_migrate_marker("# PW-ENV:IGNORE"));
        assert!(comment_has_no_migrate_marker("#   pw-env   :   ignore   "));
        assert!(comment_has_no_migrate_marker("  # pw-env:ignore "));
    }

    #[test]
    fn strip_quotes_does_not_strip_mismatched_quotes() {
        assert_eq!(strip_quotes("\"hello'"), "\"hello'");
        assert_eq!(strip_quotes("'hello\""), "'hello\"");
    }

    #[test]
    fn looks_like_secret_name_detects_auth_key_segment_pair() {
        // "auth_key" is not among the direct patterns, so segment pair logic is required.
        assert!(looks_like_secret_name("SERVICE_AUTH_KEY"));
        assert!(looks_like_secret_name("AUTH_KEY_VALUE"));
    }

    #[test]
    fn looks_like_secret_name_detects_api_key_segment_pair_with_double_dash() {
        // "X--API--KEY" normalises to "x__api__key": "api_key" (single underscore) is
        // not a substring, so only the segment-pair check can return true.
        assert!(looks_like_secret_name("X--API--KEY"));
    }

    #[test]
    fn looks_like_secret_name_detects_private_key_segment_pair_with_double_dash() {
        // Same reasoning: "x__private__key" does not contain the "private_key" substring.
        assert!(looks_like_secret_name("X--PRIVATE--KEY"));
    }

    #[test]
    fn looks_like_secret_name_detects_access_key_segment_pair_with_double_dash() {
        assert!(looks_like_secret_name("X--ACCESS--KEY"));
    }

    #[test]
    fn looks_like_secret_name_does_not_match_non_secret_key_names() {
        assert!(!looks_like_secret_name("AUTH_URL"));
        assert!(!looks_like_secret_name("LOG_LEVEL"));
        assert!(!looks_like_secret_name("DATABASE_HOST"));
    }

    #[test]
    fn contains_embedded_credentials_empty_username_returns_false() {
        assert!(!contains_embedded_credentials(
            "postgres://:secret@db.example.com/app"
        ));
    }

    #[test]
    fn contains_embedded_credentials_empty_password_returns_false() {
        assert!(!contains_embedded_credentials(
            "postgres://user:@db.example.com/app"
        ));
    }

    #[test]
    fn contains_embedded_credentials_both_present_returns_true() {
        assert!(contains_embedded_credentials(
            "postgres://user:secret@db.example.com/app"
        ));
    }

    #[test]
    fn has_common_secret_prefix_detects_each_known_prefix() {
        assert!(has_common_secret_prefix("ghp_xxxx"));
        assert!(has_common_secret_prefix("github_pat_xxxx"));
        assert!(has_common_secret_prefix("glpat-xxxx"));
        assert!(has_common_secret_prefix("xoxb-xxxx"));
        assert!(has_common_secret_prefix("xoxp-xxxx"));
        assert!(has_common_secret_prefix("xoxs-xxxx"));
        assert!(has_common_secret_prefix("sk-xxxx"));
        assert!(has_common_secret_prefix("AKIAxxxx"));
    }

    #[test]
    fn has_common_secret_prefix_returns_false_for_plain_value() {
        assert!(!has_common_secret_prefix("normalvalue"));
        assert!(!has_common_secret_prefix("debug"));
    }

    #[test]
    fn looks_like_high_entropy_secret_low_entropy_is_not_secret() {
        // 20 identical chars: entropy = 0 — should not be flagged even though len >= 20.
        assert!(!looks_like_high_entropy_secret("aaaaaaaaaaaaaaaaaaaa"));
    }

    #[test]
    fn looks_like_high_entropy_secret_medium_entropy_short_is_not_secret() {
        // 24 chars, 12 unique chars each appearing twice → entropy ≈ log2(12) ≈ 3.58.
        // len 24 < 32, so the second condition (len≥32 && entropy≥3.4) fails.
        // The first condition also fails (entropy < 3.8).
        assert!(!looks_like_high_entropy_secret("aabbccddeeffgghhiijjkkll"));
    }

    #[test]
    fn looks_like_high_entropy_secret_medium_entropy_long_is_secret() {
        // 33 chars, 11 unique chars each appearing 3 times → entropy ≈ log2(11) ≈ 3.46.
        // Satisfies the second condition: len≥32 && entropy≥3.4.
        assert!(looks_like_high_entropy_secret(
            "abcdefghijkabcdefghijkabcdefghijk"
        ));
    }

    #[test]
    fn looks_like_high_entropy_secret_exactly_minimum_length_qualifies() {
        // 20 unique ASCII letters → entropy = log2(20) ≈ 4.32 ≥ 3.8, len == 20.
        // With the `< 20` guard mutated to `== 20` or `<= 20` this would be rejected early.
        assert!(looks_like_high_entropy_secret("abcdefghijklmnopqrst"));
    }

    fn write_test_env(contents: &str) -> PathBuf {
        use std::time::{SystemTime, UNIX_EPOCH};

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after unix epoch")
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("pw-env-test-{}-{}.env", std::process::id(), unique));

        std::fs::write(&path, contents).expect("temp env file should be writable");
        path
    }

    #[test]
    fn find_returns_some_for_plain_env_file() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let env_path = temp_dir.path().join(".env");
        std::fs::write(&env_path, "KEY=value\n").unwrap();

        let found = EnvFile::find(temp_dir.path());
        assert_eq!(found, Some(env_path));
    }

    #[cfg(unix)]
    #[test]
    fn find_rejects_symlinked_env_file() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let real_env = temp_dir.path().join("real.env");
        std::fs::write(&real_env, "KEY=value\n").unwrap();
        std::os::unix::fs::symlink(&real_env, temp_dir.path().join(".env")).unwrap();

        assert!(
            EnvFile::find(temp_dir.path()).is_none(),
            "find() should reject a .env symlink"
        );
    }

    #[test]
    fn find_with_parents_returns_ancestor_env_before_git_root() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let repo_dir = temp_dir.path().join("repo");
        let nested_dir = repo_dir.join("services/api");

        std::fs::create_dir_all(repo_dir.join(".git")).unwrap();
        std::fs::create_dir_all(&nested_dir).unwrap();
        std::fs::write(repo_dir.join(".env"), "API_KEY=\n").unwrap();

        let found = EnvFile::find_with_parents(&nested_dir, true);
        assert_eq!(found, Some(repo_dir.join(".env")));
    }

    #[test]
    fn find_with_parents_stops_at_git_root_without_env() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let repo_dir = temp_dir.path().join("repo");
        let nested_dir = repo_dir.join("services/api");

        std::fs::create_dir_all(repo_dir.join(".git")).unwrap();
        std::fs::create_dir_all(&nested_dir).unwrap();
        std::fs::write(temp_dir.path().join(".env"), "OUTSIDE_REPO=\n").unwrap();

        let found = EnvFile::find_with_parents(&nested_dir, true);
        assert!(
            found.is_none(),
            "search should stop at the git workspace root"
        );
    }

    #[test]
    fn find_with_parents_does_not_stop_at_submodule_root() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let repo_dir = temp_dir.path().join("repo");
        let submodule_dir = repo_dir.join("submodule");
        let nested_dir = submodule_dir.join("src");

        std::fs::create_dir_all(repo_dir.join(".git")).unwrap();
        std::fs::create_dir_all(&nested_dir).unwrap();
        std::fs::write(
            submodule_dir.join(".git"),
            "gitdir: ../.git/modules/submodule\n",
        )
        .unwrap();
        std::fs::write(repo_dir.join(".env"), "ROOT_SECRET=\n").unwrap();

        let found = EnvFile::find_with_parents(&nested_dir, true);
        assert_eq!(found, Some(repo_dir.join(".env")));
    }

    #[cfg(unix)]
    #[test]
    fn parse_rejects_symlinked_env_file() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let real_env = temp_dir.path().join("real.env");
        std::fs::write(&real_env, "KEY=value\n").unwrap();
        let symlink_path = temp_dir.path().join(".env");
        std::os::unix::fs::symlink(&real_env, &symlink_path).unwrap();

        let result = EnvFile::parse(&symlink_path);
        assert!(result.is_err(), "parse() should reject a .env symlink");
    }

    #[test]
    fn strip_quotes_handles_multibyte_utf8() {
        assert_eq!(strip_quotes("\"héllo\""), "héllo");
        assert_eq!(strip_quotes("'日本語'"), "日本語");
    }

    #[test]
    fn test_parse_entry_line_strips_export_prefix() {
        let (key, value, _) =
            parse_entry_line("export API_KEY=secret-value").expect("entry should parse");
        assert_eq!(key, "API_KEY");
        assert_eq!(value, "secret-value");
    }

    #[test]
    fn test_export_prefix_in_full_parse() {
        let path =
            write_test_env("export DB_HOST=localhost\nexport DB_PASS=op://vault/item/pass\n");
        let env_file = EnvFile::parse(&path).expect("parse should succeed");
        std::fs::remove_file(&path).expect("temp file should be removable");

        let entries = env_file.entries();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].key, "DB_HOST");
        assert_eq!(entries[1].key, "DB_PASS");
        assert!(matches!(entries[1].kind, EntryKind::OpReference(_)));
    }
}
