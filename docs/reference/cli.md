# CLI reference

`pw-env` is a command-oriented CLI with a small top-level surface.

## Top-level commands

| Command | Purpose |
| --- | --- |
| `init` | Generate shell hook code for `bash`, `zsh`, `fish`, or `powershell` |
| `exec` | Run a command with resolved secrets only in the child process |
| `export` | Print shell exports for the current project |
| `load` | Show a human-readable view of the current resolution state |
| `add` | Store a secret in the effective backend, or a backend chosen by flag, and ensure `.env` contains `KEY=` |
| `migrate` | Move plaintext values into the effective backend, or a backend chosen by flag |
| `check` | Verify backend binaries and config discovery |
| `approvals` | Manage local override and secret-fetch approvals |
| `cache` | Clear local metadata caches and the resolved-secret cache index |
| `update` | Replace the current binary with a GitHub release build |
| `config-template` | Print the default config template |

## `init`

```console
pw-env init <SHELL>
```

Generate shell hook code for automatic loading on directory change. Supported shells are `bash`, `zsh`, `fish`, and
`powershell`.

If the active project config defines `commands`, the generated hook installs transient wrappers for the matching
executable names instead of exporting resolved secrets into the parent shell.

## `exec`

```console
pw-env exec [--dir <DIR>] -- <COMMAND> [ARGS...]
```

Resolve the current `.env` file, inject the resolved variables only into the child process, and then replace `pw-env`
with the target command. This keeps the parent shell environment clean.

Use this directly when you want explicit transient loading, or let the generated shell hook call it for configured
project commands.

## `export`

```console
pw-env export [--shell <SHELL>] [DIR]
```

Resolve the current `.env` file and print export statements for `bash`, `zsh`, `fish`, or `powershell`. If the directory
and its parents up to the git workspace root have no `.env` file, the command prints nothing.

`cmd.exe` does not expose robust per-directory or per-command hook points, so automatic shell hooks are not supported
there.

This command is still useful for one-off shell exports. Projects that configure `commands` use transient wrappers
through the generated hook instead of directory-wide exports.

## `load`

```console
pw-env load [--reveal] [DIR]
```

Print a human-readable summary of how each `.env` entry was classified, then print masked export output that shows only
a short prefix of each resolved value. Pass `--reveal` when you intentionally need the full resolved content. Use this
when you need to debug what pw-env would do without wiring it into a shell.

## `add`

```console
pw-env add [--dir <DIR>] [--backend <BACKEND>] <KEY> [VALUE]
```

Store a secret in the effective backend for the selected directory, then ensure the active `.env` file contains an empty
entry for that key so future resolution can load it through the default backend. With parent search enabled, pw-env
updates the first `.env` it discovers while walking upward.

Pass `--backend op`, `--backend bw`, or `--backend gpg` to store the secret in a specific backend for this command
without changing the configured default backend for later resolution.

Pass `VALUE` directly when that is acceptable for your shell history and process list. If you omit it, `pw-env` reads
the value from an interactive hidden prompt or from stdin when piped.

If the key already exists in `.env` with a plaintext value or an explicit backend reference, the command stops instead
of silently rewriting that line.

## `migrate`

```console
pw-env migrate [--backend <op|bw|gpg>] [DIR]
```

Scan plaintext values, open an interactive selection prompt, store the chosen entries in the effective backend or a
backend chosen for that invocation, and clear only the entries that were stored and verified.

## `check`

```console
pw-env check
```

Check whether `op`, `bw`, and `gpg` are available on `PATH`, then report the discovered config file and effective
defaults.

## `approvals`

```console
pw-env approvals <SUBCOMMAND>
```

### Project override approvals

| Subcommand | Usage | Purpose |
| --- | --- | --- |
| `list` | `pw-env approvals list` | List approved `.pw-env.toml` hashes |
| `approve` | `pw-env approvals approve <PATH>` | Approve the current contents of a `.pw-env.toml` file or project directory |
| `show` | `pw-env approvals show [PATH]` | Show the current and approved hash for a local override |
| `revoke` | `pw-env approvals revoke <PATH>` | Remove the stored approval for a local override |

### Secret-fetch approvals

| Subcommand | Usage | Purpose |
| --- | --- | --- |
| `list-fetch` | `pw-env approvals list-fetch` | List approved projects and `.env` hashes |
| `approve-fetch` | `pw-env approvals approve-fetch <PATH>` | Approve secret fetching for the current `.env` hash |
| `approve-fetch --project-wide` | `pw-env approvals approve-fetch <PATH> --project-wide` | Approve future `.env` changes in the same project |
| `show-fetch` | `pw-env approvals show-fetch [PATH]` | Show the current approval status for secret fetching |
| `revoke-fetch` | `pw-env approvals revoke-fetch <PATH>` | Remove secret-fetch approvals for a project |

## `cache`

```console
pw-env cache clear
```

Clear persisted Bitwarden folder and sync metadata, remove the resolved-secret cache index under the pw-env state
directory, and best-effort delete the corresponding OS-keyring cache entries.

If the OS keyring is unavailable at clear time, pw-env still removes the local index so future resolutions miss the
cache and fetch directly from the configured backend.

## `update`

```console
pw-env update [--version <VERSION>]
```

Download the release asset that matches the current platform and replace the running executable. Omit `--version` to
install the latest release.

## `config-template`

```console
pw-env config-template
```

Print the default `config.toml` template to stdout.

The template includes `[defaults.cache]`, where `enabled = true` and `ttl_hours = 4` control the resolved-secret
cache.

## Help and version

```console
pw-env --help
pw-env --version
```

Use `pw-env help <command>` or `pw-env <command> --help` when you want the built-in usage text for a specific command.
