# First project

The fastest way to adopt pw-env is to keep the shape of your `.env` file, then let the CLI fill in secrets at runtime.

## 1. Create the project env file

Use empty values for variables that should be loaded from the default backend:

```dotenv [.env]
DATABASE_URL=
API_KEY=
```

Mix in explicit references when a key should always come from a specific backend:

```dotenv [.env]
DATABASE_URL=op://Development/my-app/database_url
API_KEY=bw://env-secrets/my-app/api_key
LOG_LEVEL=debug
```

Plaintext values are left alone until you migrate them. Add `# no-migrate` when a local value should never be treated as
a migration candidate.

## 2. Create the global config

Start from the built-in template:

```console
$ pw-env config-template > ~/.config/pw-env/config.toml
```

Pick a default backend for empty values.

::: code-group

```toml [1Password: ~/.config/pw-env/config.toml]
[defaults]
backend = "op"

[defaults.op]
vault = "Development"
```

```toml [Bitwarden: ~/.config/pw-env/config.toml]
[defaults]
backend = "bw"

[defaults.bw]
folder = "env-secrets"
```

```toml [GPG: ~/.config/pw-env/config.toml]
[defaults]
backend = "gpg"

[defaults.gpg]
file_pattern = ".env.gpg"
recipient = "your-email@example.com"
```

:::

## 3. Export the values into your shell

::: code-group

```console [bash or zsh]
eval "$(pw-env export . --shell bash)"
```

```console [fish]
pw-env export . --shell fish | source
```

```powershell [powershell]
Invoke-Expression (& pw-env export . --shell powershell)
```

:::

The first time a project `.env` would trigger secret fetching, pw-env asks you to approve it. The default approval is
tied to the current `.env` hash, so changing the file causes a new approval prompt.

## 4. Inspect what pw-env sees

```console
pw-env load .
pw-env check
```

`pw-env load` shows how each entry was classified before printing masked export output with only a short value prefix,
which makes it a good first debugging command. Add `--reveal` only when you intentionally need to inspect the full
resolved values.

## 5. Install automatic loading when you are ready

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

```powershell [powershell]
Invoke-Expression (& pw-env init powershell)
```

:::

Add the same command to your shell startup file so the hook is installed in every new session. For PowerShell, add it
to your `$PROFILE` file.

To enable tab completion for pw-env commands, generate the completion script once and source it from the same startup
file:

::: code-group

```console [bash]
eval "$(pw-env completions bash)"
```

```console [zsh]
eval "$(pw-env completions zsh)"
```

```console [fish]
pw-env completions fish | source
```

```powershell [powershell]
Invoke-Expression (& pw-env completions powershell)
```

:::

See [Shell integration](shell-integration.md) for the full hook behavior.

## Next steps

Move to [Shell integration](shell-integration.md) when you want automatic loading on `cd`, or to
[Migrating plaintext secrets](../guides/migrate-secrets.md) if your current `.env` file still contains credentials.
