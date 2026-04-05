/// Generate shell hook code for the given shell.
/// The hook wraps `cd` (or uses chpwd/fish events) to automatically run
/// `pw-env export` when entering a directory with a .env file.
pub fn generate_hook(shell: &str) -> String {
    match shell {
        "bash" => generate_bash_hook(),
        "zsh" => generate_zsh_hook(),
        "fish" => generate_fish_hook(),
        other => format!("# Unsupported shell: {other}\n# Supported shells: bash, zsh, fish\n"),
    }
}

fn generate_bash_hook() -> String {
    r#"# pw-env shell hook for bash
# Add to ~/.bashrc: eval "$(pw-env init bash)"

__pw_env_previous_keys=""

__pw_env_hook() {
    # Unset previously exported variables
    if [ -n "$__pw_env_previous_keys" ]; then
        for key in $__pw_env_previous_keys; do
            unset "$key"
        done
        __pw_env_previous_keys=""
    fi

    # Check if there's a .env file in the current directory
    if [ -f ".env" ]; then
        local _pw_env_output
        _pw_env_output="$(pw-env export "$PWD" --shell bash)"
        if [ -n "$_pw_env_output" ]; then
            eval "$_pw_env_output"
        fi
    fi
}

# Wrap cd to trigger the hook
cd() {
    builtin cd "$@" && __pw_env_hook
}

# Also hook into pushd and popd
pushd() {
    builtin pushd "$@" && __pw_env_hook
}

popd() {
    builtin popd "$@" && __pw_env_hook
}

# Run on shell init for the current directory
__pw_env_hook
"#
    .to_string()
}

fn generate_zsh_hook() -> String {
    r#"# pw-env shell hook for zsh
# Add to ~/.zshrc: eval "$(pw-env init zsh)"

typeset -g __pw_env_previous_keys=""

__pw_env_hook() {
    # Unset previously exported variables
    if [[ -n "$__pw_env_previous_keys" ]]; then
        for key in ${=__pw_env_previous_keys}; do
            unset "$key"
        done
        __pw_env_previous_keys=""
    fi

    # Check if there's a .env file in the current directory
    if [[ -f ".env" ]]; then
        local _pw_env_output
        _pw_env_output="$(pw-env export "$PWD" --shell zsh)"
        if [[ -n "$_pw_env_output" ]]; then
            eval "$_pw_env_output"
        fi
    fi
}

# Use zsh's chpwd hook
autoload -U add-zsh-hook
add-zsh-hook chpwd __pw_env_hook

# Run on shell init for the current directory
__pw_env_hook
"#
    .to_string()
}

fn generate_fish_hook() -> String {
    r#"# pw-env shell hook for fish
# Add to ~/.config/fish/config.fish: pw-env init fish | source

set -g __pw_env_previous_keys ""

function __pw_env_hook --on-variable PWD
    # Unset previously exported variables
    if test -n "$__pw_env_previous_keys"
        for key in (string split " " $__pw_env_previous_keys)
            set -e $key
        end
        set -g __pw_env_previous_keys ""
    end

    # Check if there's a .env file in the current directory
    if test -f ".env"
        set -l _pw_env_output (pw-env export $PWD --shell fish)
        if test -n "$_pw_env_output"
            eval $_pw_env_output
        end
    end
end

# Run on shell init for the current directory
__pw_env_hook
"#
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bash_hook_contains_cd() {
        let hook = generate_hook("bash");
        assert!(hook.contains("cd()"));
        assert!(hook.contains("pw-env export"));
        assert!(hook.contains("__pw_env_hook"));
        assert!(!hook.contains("2>/dev/null"));
    }

    #[test]
    fn test_zsh_hook_contains_chpwd() {
        let hook = generate_hook("zsh");
        assert!(hook.contains("chpwd"));
        assert!(hook.contains("pw-env export"));
        assert!(!hook.contains("2>/dev/null"));
    }

    #[test]
    fn test_fish_hook_contains_on_variable() {
        let hook = generate_hook("fish");
        assert!(hook.contains("--on-variable PWD"));
        assert!(hook.contains("pw-env export"));
        assert!(!hook.contains("2>/dev/null"));
    }

    #[test]
    fn test_unsupported_shell() {
        let hook = generate_hook("powershell");
        assert!(hook.contains("Unsupported shell"));
    }
}
