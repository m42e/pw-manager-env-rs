# Shell integration

pw-env supports one-off exports and persistent shell hooks.

The persistent hook has two modes:

1. Default mode exports resolved secrets for the whole directory when you enter it.
2. Command-scoped mode installs transient wrappers for configured commands and keeps secrets out of the parent shell
   environment.

## One-off loading

::: code-group

```console [bash]
eval "$(pw-env export . --shell bash)"
```

```console [zsh]
eval "$(pw-env export . --shell zsh)"
```

```console [fish]
pw-env export . --shell fish | source
```

```console [powershell]
Invoke-Expression (& pw-env export . --shell powershell)
```

:::

If the current directory does not contain a `.env` file, `pw-env export` returns nothing.

## Automatic loading on directory change

Install the generated hook into your shell startup file.

::: code-group

```console [bash]
eval "$(pw-env init bash)"
```

```console [zsh]
eval "$(pw-env init zsh)"
```

```console [fish]
pw-env init fish | source
```

```console [powershell]
Invoke-Expression (& pw-env init powershell)
```

:::

Add the same command to your startup file (`~/.bashrc`, `~/.zshrc`, `~/.config/fish/config.fish`, or `$PROFILE` in
PowerShell).

## What the generated hook does

1. Tracks which directory last loaded pw-env shell state.
2. Keeps that state active while you move through nested subdirectories under the same directory.
3. Replaces the state when you enter another directory that has its own `.env` file.
4. Clears the state when you leave the owning directory tree entirely.

Warnings from pw-env are written to stderr, so they remain visible when the hook is running automatically.

## Command-scoped mode

Add a `commands` list to a matching `[[projects]]` entry or to `.pw-env.toml`:

```toml
commands = ["cargo*", "npm", "terraform"]
```

In that mode, pw-env does not export secrets into the parent shell when you enter the directory. Instead it resolves the
configured command names and glob patterns against executable names on `PATH`, installs wrappers for the matches, and
runs those commands through `pw-env exec` so the resolved secrets exist only in the child process.

Command-scoped mode matches exact command names and shell-style glob patterns against executable names.

## Per-shell behavior

| Shell | Hook strategy |
| --- | --- |
| `bash` | Wraps `cd`, `pushd`, and `popd` |
| `zsh` | Registers a `chpwd` hook |
| `fish` | Uses a `PWD` variable event |
| `powershell` | Wraps `prompt` and reacts to location changes |

PowerShell hook behavior runs on prompt render and detects directory changes by comparing the current location to the
previous one. `cmd.exe` does not support equivalent automatic hooks for directory changes or command execution, so use
`pw-env exec` directly there.

## Debugging shell behavior

When automatic loading does not look right, verify the project directly before changing your shell config:

```console
pw-env load .
pw-env export . --shell bash
pw-env exec --dir . -- env | grep YOUR_KEY
```

If you expect a backend lookup but `pw-env export` prints nothing, check the `.env` file classification rules in
[Resolution model](../concepts/resolution-model.md).
