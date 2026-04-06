# Configuration model

pw-env layers defaults, project overrides, and optional project-local overrides to determine how a directory should resolve secrets.

## Global config path

By default, pw-env reads the global config from `~/.config/pw-env/config.toml`.

If `XDG_CONFIG_HOME` is set, pw-env uses that directory instead of `~/.config`.

## Configuration layers

From broadest to narrowest, the effective configuration is built like this:

1. Built-in defaults.
2. Global config sections such as `[defaults]`, `[log]`, and `[updates]`.
3. Matching `[[projects]]` entries in the global config, where the most specific path prefix wins.
4. An approved `.pw-env.toml` discovered by walking upward from the current directory to the repository root.

## Backend sections

| Section | Purpose |
| --- | --- |
| `[defaults]` | Select the default backend for empty values |
| `[defaults.op]` | 1Password defaults such as `vault`, `account`, and `item` |
| `[defaults.bw]` | Bitwarden defaults such as `folder`, `organization`, and `item` |
| `[defaults.gpg]` | GPG defaults such as `file_pattern` and `recipient` |

The same backend-specific sections can also appear under `[[projects]]`, and inside a project-local `.pw-env.toml`.

## Item resolution

`item` can be set at the project level or inside the backend-specific defaults for 1Password and Bitwarden. A project-level `item` wins over the backend-specific default item.

## Command-scoped shell behavior

Projects can also declare `commands`, a list of exact command names or executable-name glob patterns that should receive secrets transiently through the generated shell hook.

That setting is resolved with the same precedence as the rest of the project configuration: the most specific matching `[[projects]]` entry wins, and an approved `.pw-env.toml` can override it for the current repository.

When `commands` is present, pw-env keeps using the project config to resolve secrets, but the shell integration changes behavior: it installs wrappers for those commands instead of exporting resolved variables into the parent shell session.

## Local override discovery

`.pw-env.toml` is not searched across the whole filesystem. pw-env only walks upward from the current directory until it reaches the repository root, then stops.

That keeps the override boundary inside the current project.

## Logging and update checks

`[log]` controls the log level and optional log file destination.

`[updates]` controls whether automatic GitHub release checks are enabled and how often they may run.

For the exact TOML shape, see [Configuration file](../reference/configuration-file.md).