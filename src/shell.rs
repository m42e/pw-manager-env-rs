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
__pw_env_previous_commands=""
__pw_env_saved_aliases=""

__pw_env_clear_state() {
    if [ -n "$__pw_env_previous_keys" ]; then
        for key in $__pw_env_previous_keys; do
            unset "$key"
        done
        __pw_env_previous_keys=""
    fi

    if [ -n "$__pw_env_previous_commands" ]; then
        for cmd in $__pw_env_previous_commands; do
            if alias "$cmd" >/dev/null 2>&1; then
                unalias "$cmd"
            fi
        done
        __pw_env_previous_commands=""
    fi

    if [ -n "$__pw_env_saved_aliases" ]; then
        eval "$__pw_env_saved_aliases"
        __pw_env_saved_aliases=""
    fi
}

__pw_env_define_command_wrapper() {
    local cmd="$1"
    local existing

    if alias "$cmd" >/dev/null 2>&1; then
        existing="$(alias "$cmd")"
        __pw_env_saved_aliases="${__pw_env_saved_aliases}${existing}
"
    fi

    alias "$cmd"="pw-env exec --dir \"\$PWD\" -- $cmd"
}

__pw_env_hook() {
    __pw_env_clear_state

    if [ -f ".env" ]; then
        local _pw_env_output
        _pw_env_output="$(pw-env hook "$PWD" --shell bash)"
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
typeset -g __pw_env_previous_commands=""
typeset -g __pw_env_saved_aliases=""

__pw_env_clear_state() {
    if [[ -n "$__pw_env_previous_keys" ]]; then
        for key in ${=__pw_env_previous_keys}; do
            unset "$key"
        done
        __pw_env_previous_keys=""
    fi

    if [[ -n "$__pw_env_previous_commands" ]]; then
        for cmd in ${=__pw_env_previous_commands}; do
            if alias "$cmd" >/dev/null 2>&1; then
                unalias "$cmd"
            fi
        done
        __pw_env_previous_commands=""
    fi

    if [[ -n "$__pw_env_saved_aliases" ]]; then
        eval "$__pw_env_saved_aliases"
        __pw_env_saved_aliases=""
    fi
}

__pw_env_define_command_wrapper() {
    local cmd="$1"
    local existing

    if alias "$cmd" >/dev/null 2>&1; then
        existing="$(alias "$cmd")"
        __pw_env_saved_aliases="${__pw_env_saved_aliases}${existing}
"
    fi

    alias "$cmd"="pw-env exec --dir \"\$PWD\" -- $cmd"
}

__pw_env_hook() {
    __pw_env_clear_state

    if [[ -f ".env" ]]; then
        local _pw_env_output
        _pw_env_output="$(pw-env hook "$PWD" --shell zsh)"
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
set -g __pw_env_previous_commands

function __pw_env_saved_function_name --argument-names cmd
    string replace -ra '[^A-Za-z0-9_]' '_' -- "__pw_env_saved_$cmd"
end

function __pw_env_clear_state
    if test -n "$__pw_env_previous_keys"
        for key in (string split " " $__pw_env_previous_keys)
            set -e $key
        end
        set -g __pw_env_previous_keys ""
    end

    for cmd in $__pw_env_previous_commands
        if functions -q $cmd
            functions -e $cmd
        end

        set -l saved (__pw_env_saved_function_name $cmd)
        if functions -q $saved
            functions -c $saved $cmd
            functions -e $saved
        end
    end

    set -g __pw_env_previous_commands
end

function __pw_env_define_command_wrapper --argument-names cmd
    set -l saved (__pw_env_saved_function_name $cmd)

    if functions -q $saved
        functions -e $saved
    end

    if functions -q $cmd
        functions -c $cmd $saved
    end

    eval "function $cmd --wraps $cmd
    pw-env exec --dir \$PWD -- $cmd \$argv
end"
end

function __pw_env_hook --on-variable PWD
    __pw_env_clear_state

    if test -f ".env"
        set -l _pw_env_output (pw-env hook $PWD --shell fish)
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
        assert!(hook.contains("pw-env hook"));
        assert!(hook.contains("pw-env exec"));
        assert!(hook.contains("__pw_env_hook"));
    }

    #[test]
    fn test_zsh_hook_contains_chpwd() {
        let hook = generate_hook("zsh");
        assert!(hook.contains("chpwd"));
        assert!(hook.contains("pw-env hook"));
        assert!(hook.contains("pw-env exec"));
    }

    #[test]
    fn test_fish_hook_contains_on_variable() {
        let hook = generate_hook("fish");
        assert!(hook.contains("--on-variable PWD"));
        assert!(hook.contains("pw-env hook"));
        assert!(hook.contains("pw-env exec"));
    }

    #[test]
    fn test_unsupported_shell() {
        let hook = generate_hook("powershell");
        assert!(hook.contains("Unsupported shell"));
    }
}
