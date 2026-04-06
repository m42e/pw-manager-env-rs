# Configuration file

The main config file lives at `~/.config/pw-env/config.toml` unless `XDG_CONFIG_HOME` overrides that location.

## Default template

```toml [~/.config/pw-env/config.toml]
# pw-env configuration
# Place this file at ~/.config/pw-env/config.toml

[defaults]
# Default backend: "op" (1Password), "bw" (Bitwarden), or "gpg" (GPG encrypted file)
backend = "op"

[defaults.op]
# Default 1Password vault to search in
# vault = "Development"
# 1Password account shorthand (for multiple accounts)
# account = "my-team"
# Default item name - if set, keys are resolved as fields on this item
# item = "project-env"

[defaults.bw]
# Default Bitwarden folder to search in
# folder = "env-secrets"
# Default Bitwarden organization
# organization = ""
# Default item name - if set, keys are resolved as custom fields on this item
# item = "project-env"

[defaults.gpg]
# Default encrypted file name to look for
file_pattern = ".env.gpg"
# GPG recipient for encrypting (required for `pw-env migrate` with GPG backend)
# recipient = "your-email@example.com"

[log]
# Log level: trace, debug, info, warn, error
level = "info"
# Log file path (optional, defaults to ~/.local/state/pw-env/pw-env.log)
# Successful credential fetches are also written here as AUDIT lines without secret values.
# file = "~/.local/state/pw-env/pw-env.log"

[updates]
# Automatically check GitHub releases for a newer pw-env version.
enabled = true
# Minimum time between automatic checks.
check_interval_hours = 24

# Per-project overrides
# Matched by directory path prefix - most specific match wins
#
# [[projects]]
# path = "/home/user/work/api-server"
# backend = "op"
# item = "api-server-env"
# commands = ["cargo", "npm"]
#
# [projects.op]
# vault = "Work"
#
# [[projects]]
# path = "/home/user/personal/blog"
# backend = "gpg"
#
# [projects.gpg]
# file_pattern = ".secrets.gpg"
# recipient = "personal@example.com"
```

## Section reference

| Section | Keys | Notes |
| --- | --- | --- |
| `[defaults]` | `backend` | Selects the default backend for empty `.env` values |
| `[defaults.op]` | `vault`, `account`, `item` | Default 1Password lookup settings |
| `[defaults.bw]` | `folder`, `organization`, `item` | Default Bitwarden lookup settings |
| `[defaults.gpg]` | `file_pattern`, `recipient` | GPG file matching and encryption settings |
| `[log]` | `level`, `file` | Logging configuration and audit-log destination |
| `[updates]` | `enabled`, `check_interval_hours` | Automatic GitHub release checks |
| `[[projects]]` | `path`, `backend`, `item`, `commands` | Per-path overrides; most specific path prefix wins |
| `[projects.op]`, `[projects.bw]`, `[projects.gpg]` | backend-specific keys | Extra settings for the most recent `[[projects]]` block |

## Valid backend values

`backend` accepts `op`, `bw`, or `gpg`.

## Project-local override file

For a repository-specific override, create `.pw-env.toml` in the project root or a parent directory inside the repository.

```toml [.pw-env.toml]
backend = "op"
item = "api-server-env"
commands = ["cargo", "npm"]

[op]
vault = "Work"
```

`.pw-env.toml` uses the same backend-specific keys as the global config, but it does not use `[[projects]]` because the file itself already scopes the override to the current project.

pw-env loads the local override only after the current file contents are approved.

## Command-scoped shell integration

Set `commands` on a project entry or in `.pw-env.toml` to opt that project into transient command wrappers.

```toml
[[projects]]
path = "/home/user/work/api-server"
backend = "op"
item = "api-server-env"
commands = ["cargo", "npm", "terraform"]
```

When `commands` is set, the generated shell hook stops exporting resolved secrets into the parent shell for that project. Instead it installs wrappers for the listed command names, and those wrappers run the command through `pw-env exec`.

`commands` accepts exact command names and shell-style glob patterns that are matched against executable names on `PATH`. Values must still resolve to safe shell command tokens such as `cargo`, `npm`, `docker-compose`, or patterns such as `cargo*`.