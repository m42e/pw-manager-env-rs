use std::collections::BTreeMap;

/// Show only a short prefix of a secret-like value for inspection output.
pub fn obfuscate_value(value: &str) -> String {
    let visible: String = value.chars().take(3).collect();
    let total_chars = value.chars().count();

    if total_chars <= 3 {
        "***".to_string()
    } else {
        format!("{visible}***")
    }
}

/// Format resolved key-value pairs as shell export statements.
/// Uses single-quote escaping to prevent shell injection.
pub fn format_exports(vars: &BTreeMap<String, String>, shell: ShellSyntax) -> String {
    let mut output = String::new();
    for (key, value) in vars {
        if !is_valid_env_key(key) {
            continue;
        }
        match shell {
            ShellSyntax::Posix => {
                // export KEY='value' with single-quote escaping: ' -> '\''
                let escaped = shell_escape_single_quote(value);
                output.push_str(&format!("export {key}='{escaped}'\n"));
            }
            ShellSyntax::Fish => {
                // set -gx KEY 'value'
                let escaped = shell_escape_single_quote(value);
                output.push_str(&format!("set -gx {key} '{escaped}'\n"));
            }
        }
    }
    output
}

/// Format the list of exported keys as a space-separated string for tracking.
pub fn format_key_tracking(keys: &[String]) -> String {
    keys.iter()
        .filter(|k| is_valid_env_key(k))
        .cloned()
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Debug, Clone, Copy)]
pub enum ShellSyntax {
    Posix, // bash, zsh
    Fish,
}

/// Escape a value for safe embedding in single quotes.
/// The only character that needs escaping in single quotes is the single quote itself.
/// We replace ' with '\'' (end quote, escaped quote, start quote).
fn shell_escape_single_quote(value: &str) -> String {
    value.replace('\'', "'\\''")
}

/// Validate that a key is a safe environment variable name.
/// Must match [A-Za-z_][A-Za-z0-9_]* to prevent shell injection.
fn is_valid_env_key(key: &str) -> bool {
    if key.is_empty() {
        return false;
    }
    let mut chars = key.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_alphabetic() && first != '_' {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shell_escape_simple() {
        assert_eq!(shell_escape_single_quote("hello"), "hello");
    }

    #[test]
    fn test_shell_escape_with_single_quote() {
        assert_eq!(shell_escape_single_quote("it's"), "it'\\''s");
    }

    #[test]
    fn test_shell_escape_with_spaces() {
        assert_eq!(shell_escape_single_quote("hello world"), "hello world");
    }

    #[test]
    fn test_shell_escape_with_double_quotes() {
        assert_eq!(shell_escape_single_quote("say \"hello\""), "say \"hello\"");
    }

    #[test]
    fn test_format_exports_posix() {
        let mut vars = BTreeMap::new();
        vars.insert("DB_HOST".to_string(), "localhost".to_string());
        vars.insert("DB_PASS".to_string(), "sec'ret".to_string());
        let output = format_exports(&vars, ShellSyntax::Posix);
        assert!(output.contains("export DB_HOST='localhost'\n"));
        assert!(output.contains("export DB_PASS='sec'\\''ret'\n"));
    }

    #[test]
    fn test_format_exports_fish() {
        let mut vars = BTreeMap::new();
        vars.insert("API_KEY".to_string(), "abc123".to_string());
        let output = format_exports(&vars, ShellSyntax::Fish);
        assert!(output.contains("set -gx API_KEY 'abc123'\n"));
    }

    #[test]
    fn test_obfuscate_value_long() {
        assert_eq!(obfuscate_value("abcdef"), "abc***");
    }

    #[test]
    fn test_obfuscate_value_short() {
        assert_eq!(obfuscate_value("abc"), "***");
    }

    #[test]
    fn test_obfuscate_value_unicode_safe() {
        assert_eq!(obfuscate_value("a😀bcdef"), "a😀b***");
    }

    #[test]
    fn test_valid_env_key() {
        assert!(is_valid_env_key("DATABASE_URL"));
        assert!(is_valid_env_key("_PRIVATE"));
        assert!(is_valid_env_key("a"));
        assert!(!is_valid_env_key(""));
        assert!(!is_valid_env_key("1ABC"));
        assert!(!is_valid_env_key("KEY WITH SPACE"));
        assert!(!is_valid_env_key("KEY;INJECTION"));
    }

    #[test]
    fn test_rejects_injection_key() {
        let mut vars = BTreeMap::new();
        vars.insert("GOOD_KEY".to_string(), "value".to_string());
        vars.insert("BAD;KEY".to_string(), "pwned".to_string());
        let output = format_exports(&vars, ShellSyntax::Posix);
        assert!(output.contains("GOOD_KEY"));
        assert!(!output.contains("BAD;KEY"));
    }
}
