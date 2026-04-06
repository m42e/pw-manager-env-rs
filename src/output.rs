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
            ShellSyntax::PowerShell => {
                // $env:KEY = 'value' with PowerShell single-quote escaping: ' -> ''
                let escaped = powershell_escape_single_quote(value);
                output.push_str(&format!("$env:{key} = '{escaped}'\n"));
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

/// Format configured command wrappers for the shell hook.
pub fn format_command_wrappers(commands: &[String], shell: ShellSyntax) -> String {
    let valid_commands = commands
        .iter()
        .filter(|command| is_safe_command_name(command))
        .cloned()
        .collect::<Vec<_>>();

    let mut output = String::new();
    for command in &valid_commands {
        match shell {
            ShellSyntax::Posix => {
                output.push_str(&format!(
                    "__pw_env_define_command_wrapper {command}\n"
                ));
            }
            ShellSyntax::Fish => {
                output.push_str(&format!(
                    "__pw_env_define_command_wrapper {command}\n"
                ));
            }
            ShellSyntax::PowerShell => {
                output.push_str(&format!(
                    "__pw_env_define_command_wrapper '{}'\n",
                    powershell_escape_single_quote(command)
                ));
            }
        }
    }

    match shell {
        ShellSyntax::Posix => {
            output.push_str("__pw_env_previous_keys=\"\"\n");
            output.push_str(&format!(
                "__pw_env_previous_commands=\"{}\"\n",
                format_command_tracking(&valid_commands)
            ));
        }
        ShellSyntax::Fish => {
            output.push_str("set -g __pw_env_previous_keys \"\"\n");
            if valid_commands.is_empty() {
                output.push_str("set -g __pw_env_previous_commands\n");
            } else {
                output.push_str(&format!(
                    "set -g __pw_env_previous_commands {}\n",
                    valid_commands.join(" ")
                ));
            }
        }
        ShellSyntax::PowerShell => {
            output.push_str("$global:__pw_env_previous_keys = @()\n");
            output.push_str(&format!(
                "$global:__pw_env_previous_commands = {}\n",
                format_powershell_array(&valid_commands)
            ));
        }
    }

    output
}

/// Format the list of wrapped commands as a space-separated string for tracking.
pub fn format_command_tracking(commands: &[String]) -> String {
    commands
        .iter()
        .filter(|command| is_safe_command_name(command))
        .cloned()
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Debug, Clone, Copy)]
pub enum ShellSyntax {
    Posix, // bash, zsh
    Fish,
    PowerShell,
}

/// Escape a value for safe embedding in single quotes.
/// The only character that needs escaping in single quotes is the single quote itself.
/// We replace ' with '\'' (end quote, escaped quote, start quote).
fn shell_escape_single_quote(value: &str) -> String {
    value.replace('\'', "'\\''")
}

/// Escape a value for safe embedding in PowerShell single quotes.
/// In PowerShell, single quote escaping is done by doubling it.
fn powershell_escape_single_quote(value: &str) -> String {
    value.replace('\'', "''")
}

fn format_powershell_array(values: &[String]) -> String {
    if values.is_empty() {
        "@()".to_string()
    } else {
        let escaped = values
            .iter()
            .map(|value| format!("'{}'", powershell_escape_single_quote(value)))
            .collect::<Vec<_>>()
            .join(", ");
        format!("@({escaped})")
    }
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

/// Validate that a configured command name is safe to wrap in shell aliases/functions.
pub fn is_safe_command_name(command: &str) -> bool {
    if command.is_empty() {
        return false;
    }

    command.chars().all(|ch| {
        ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | ':')
    })
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
    fn test_format_exports_powershell() {
        let mut vars = BTreeMap::new();
        vars.insert("DB_PASS".to_string(), "sec'ret".to_string());
        let output = format_exports(&vars, ShellSyntax::PowerShell);
        assert!(output.contains("$env:DB_PASS = 'sec''ret'\n"));
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

    #[test]
    fn test_safe_command_name() {
        assert!(is_safe_command_name("cargo"));
        assert!(is_safe_command_name("docker-compose"));
        assert!(is_safe_command_name("npm"));
        assert!(!is_safe_command_name("cargo test"));
        assert!(!is_safe_command_name("cargo;rm"));
    }

    #[test]
    fn test_format_key_tracking_returns_space_separated_keys() {
        let keys = vec!["API_KEY".to_string(), "DB_PASSWORD".to_string()];
        assert_eq!(format_key_tracking(&keys), "API_KEY DB_PASSWORD");
    }

    #[test]
    fn test_format_key_tracking_filters_invalid_keys() {
        let keys = vec!["VALID_KEY".to_string(), "bad key".to_string()];
        assert_eq!(format_key_tracking(&keys), "VALID_KEY");
    }

    #[test]
    fn test_format_key_tracking_empty_input_returns_empty() {
        assert_eq!(format_key_tracking(&[]), "");
    }

    #[test]
    fn test_format_command_wrappers_posix() {
        let output = format_command_wrappers(
            &["cargo".to_string(), "npm".to_string()],
            ShellSyntax::Posix,
        );

        assert!(output.contains("__pw_env_define_command_wrapper cargo\n"));
        assert!(output.contains("__pw_env_define_command_wrapper npm\n"));
        assert!(output.contains("__pw_env_previous_commands=\"cargo npm\"\n"));
    }

    #[test]
    fn test_format_command_wrappers_fish() {
        let output = format_command_wrappers(&["cargo".to_string()], ShellSyntax::Fish);

        assert!(output.contains("__pw_env_define_command_wrapper cargo\n"));
        assert!(output.contains("set -g __pw_env_previous_commands cargo\n"));
    }

    #[test]
    fn test_format_command_wrappers_powershell() {
        let output = format_command_wrappers(&["cargo".to_string()], ShellSyntax::PowerShell);

        assert!(output.contains("__pw_env_define_command_wrapper 'cargo'\n"));
        assert!(output.contains("$global:__pw_env_previous_keys = @()\n"));
        assert!(output.contains("$global:__pw_env_previous_commands = @('cargo')\n"));
    }
}
