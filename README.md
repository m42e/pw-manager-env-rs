# pw-env

`pw-env` is a Rust CLI that resolves `.env` entries from password managers instead of storing secrets in plaintext. It supports 1Password, Bitwarden, and GPG-encrypted env files, and can either print shell exports for `eval` or install a shell hook that automatically reloads secrets when you change directories.

The tool is designed for local development workflows where projects keep a checked-in or local `.env` shape, but secret values come from a secure backend.

## Documentation

The interactive manual is published with GitHub Pages at [m42e.de/pw-env](https://m42e.de/pw-env/).


## Features

- Resolves empty `.env` entries from a default backend by key name
- Supports explicit `op://...` and `bw://...` references per variable
- Supports GPG-backed secret files such as `.env.gpg`
- Generates shell hooks for `bash`, `zsh`, and `fish`
- Warns when likely plaintext secrets are still present in `.env`
- Migrates plaintext values out of `.env` into the configured backend
- Supports per-project backend overrides via config
- Supports trusted project-local overrides via `.pw-env.toml`
- Requires approval before a project `.env` can trigger secret fetches, keyed by project and `.env` hash
- Automatically checks GitHub releases for newer `pw-env` versions

## Install

Install the latest matching prebuilt release:

```bash
curl -fsSL https://m42e.de/pw-env/install.sh | bash
```

Install a specific release version:

```bash
curl -fsSL https://m42e.de/pw-env/install.sh | bash -s -- --version v0.1.0
```

Install into a custom directory:

```bash
curl -fsSL https://m42e.de/pw-env/install.sh | bash -s -- --dir "$HOME/.local/bin"
```

The installer currently supports these prebuilt targets:

- macOS Apple Silicon
- macOS Intel
- Linux x86_64
- Linux arm64
- Windows x86_64 when run from a POSIX shell environment such as Git Bash

To inspect what would be downloaded without installing:

```bash
./scripts/install.sh --dry-run
```

To update an existing installation in place:

```bash
pw-env update
```

To install a specific released version over the current binary:

```bash
pw-env update --version v0.2.8
```

Build from source with Cargo instead:

```bash
cargo build --release
```

The binary will be available at:

```bash
target/release/pw-env
```

For local development, you can also run it directly with Cargo:

```bash
cargo run -- --help
```

## Quick Start

### 1. Create a `.env` file

Use empty values for secrets you want to resolve from the default backend:

```dotenv
DATABASE_URL=
API_KEY=
```

Or mix in explicit references:

```dotenv
DATABASE_URL=op://Development/my-app/database_url
API_KEY=bw://env-secrets/my-app/api_key
LOG_LEVEL=debug
```

Value handling works like this:

- `KEY=` resolves `KEY` from the configured default backend
- `KEY=op://vault/item/field` resolves through 1Password
- `KEY=bw://[folder/]item/field` resolves through Bitwarden
- `KEY=plaintext` is treated as plaintext and left as-is until migrated

To keep a plaintext entry out of warnings and `pw-env migrate`, mark it with `no-migrate` either on the same line or on the comment line directly above it:

```dotenv
LOG_LEVEL=debug # no-migrate

# no-migrate
LOCAL_ONLY_TOKEN=dev-token
```

Warnings for plaintext secrets use a simple heuristic based on secret-like key names such as `API_KEY` or `PASSWORD`, embedded credentials in URLs, and high-entropy token-like values.

For the GPG backend, empty keys are resolved from an encrypted env file such as `.env.gpg`.

### 2. Create a config file

Generate the template:

```bash
pw-env config-template > ~/.config/pw-env/config.toml
```

Minimal example using 1Password:

```toml
[defaults]
backend = "op"

[defaults.op]
vault = "Development"
```

Minimal example using Bitwarden:

```toml
[defaults]
backend = "bw"

[defaults.bw]
folder = "env-secrets"
```

Minimal example using GPG:

```toml
[defaults]
backend = "gpg"

[defaults.gpg]
file_pattern = ".env.gpg"
recipient = "your-email@example.com"
```

### 3. Export variables into your shell

For one-off use:

```bash
eval "$(pw-env export . --shell bash)"
```

The first time a project `.env` would fetch credentials, `pw-env` asks for approval. By default the approval is tied to the current project and `.env` content hash, so changing `.env` requires approval again. In an interactive prompt you can also allow any future `.env` changes for that project explicitly.

For `fish`:

```fish
pw-env export . --shell fish | source
```

### 4. Install the shell hook

`bash`:

```bash
eval "$(pw-env init bash)"
```

Add that to your `~/.bashrc` to enable automatic loading on `cd`.

`zsh`:

```zsh
eval "$(pw-env init zsh)"
```

Add that to your `~/.zshrc`.

`fish`:

```fish
pw-env init fish | source
```

Add that to your fish config.

The generated hooks unset previously exported variables when you leave a directory and load new ones when entering a directory containing `.env`.
Warnings from `pw-env export` are written to stderr, so they remain visible when the shell hook auto-loads a directory.

## Backend Resolution Model

`pw-env` resolves variables in three groups:

1. `op://...` entries always use 1Password
2. `bw://...` entries always use Bitwarden
3. Empty values use the configured default backend

For 1Password and Bitwarden, empty keys can be resolved in two common ways:

- By item name matching the env key, usually reading the password field
- By reading a field from a configured shared item when `item = "project-env"` is set

For GPG, empty keys are looked up in the decrypted contents of the configured encrypted env file.

Project name detection uses the nearest Git repository root directory name when disambiguating duplicate entries in 1Password or Bitwarden.

## Configuration

Default config path:

```text
~/.config/pw-env/config.toml
```

Top-level sections:

- `[defaults]` selects the default backend
- `[defaults.op]` configures 1Password defaults such as `vault`, `account`, and `item`
- `[defaults.bw]` configures Bitwarden defaults such as `folder`, `organization`, and `item`
- `[defaults.gpg]` configures `file_pattern` and `recipient`
- `[log]` configures log level and optional log file path
- `[updates]` configures automatic release checks
- `[[projects]]` defines per-path overrides

Audit logging for credential fetches:

- Successful secret resolutions are written to the normal pw-env log file as `AUDIT credential_fetch ...` lines
- Audit entries include the detected project, project root, working folder, `.env` path, backend, and credential key name
- Secret values are never written to the audit log

Automatic release checks:

- Run on interactive commands except `pw-env export`
- Check the latest GitHub release at most once per configured interval
- Print a one-time notice per newer version until you upgrade or another release appears
- `pw-env update` downloads the matching release asset for the current platform and replaces the running binary in place

Example:

```toml
[updates]
enabled = true
check_interval_hours = 24
```

Project-local overrides:

- Put a `.pw-env.toml` file in the project directory or Git root
- `pw-env` asks for approval before loading it and stores the approved file hash in the user state directory
- If the file changes later, `pw-env` requires approval again before applying it

Secret-fetch approvals:

- `pw-env` blocks backend lookups until the current project and `.env` contents are approved
- Hash-specific approvals are stored per project, so a modified `.env` cannot fetch new credentials without another approval
- You can explicitly allow any `.env` changes for a project if you trust the whole project directory

Example with per-project overrides:

```toml
[defaults]
backend = "op"

[defaults.op]
vault = "Development"

[[projects]]
path = "~/work/company-api"
backend = "op"
item = "company-api-env"

[projects.op]
vault = "Work"

[[projects]]
path = "~/personal/site"
backend = "gpg"

[projects.gpg]
file_pattern = ".secrets.gpg"
recipient = "you@example.com"
```

Example project-local override file:

```toml
backend = "op"
item = "company-api-env"

[op]
vault = "Work"
```

The local file is applied as the most specific override for that project directory after you approve its current contents.

## Command Reference

Show all commands:

```bash
pw-env --help
```

Main subcommands:

- `pw-env init <bash|zsh|fish>` prints shell hook code
- `pw-env export [dir] --shell <bash|zsh|fish>` prints resolved exports for shell evaluation
- `pw-env load [dir]` prints a human-readable resolution summary and masked export statements; pass `--reveal` to show full values
- `pw-env migrate [dir]` interactively stores plaintext `.env` values in the configured backend and clears them from `.env`; entries marked with `no-migrate` are skipped
- `pw-env check` checks available backends and active configuration
- `pw-env approvals list` lists approved project-local override files and hashes
- `pw-env approvals approve <path>` stores the current hash for a `.pw-env.toml` file or project directory after validating the file
- `pw-env approvals show [path]` shows the current and approved hash for a `.pw-env.toml` file or project directory
- `pw-env approvals revoke <path>` removes a stored approval for a `.pw-env.toml` file or project directory
- `pw-env approvals list-fetch` lists approved secret-fetch projects and `.env` hashes
- `pw-env approvals approve-fetch <path>` approves secret fetching for the current `.env` hash
- `pw-env approvals approve-fetch <path> --project-wide` allows any future `.env` changes in that project
- `pw-env approvals show-fetch [path]` shows secret-fetch approval status for a `.env` file or project directory
- `pw-env approvals revoke-fetch <path>` removes stored secret-fetch approvals for that project
- `pw-env config-template` prints the default config template

## Migration Workflow

If `.env` still contains plaintext values:

```dotenv
DATABASE_PASSWORD=super-secret
API_KEY=abc123
```

Run:

```bash
pw-env migrate
```

The tool will:

- detect plaintext entries
- ignore entries marked with `# no-migrate` or a preceding `# no-migrate` comment
- open a TUI multi-select with likely secrets preselected so you can choose exactly which entries to store
- store `project` metadata using the Git root folder name and `migrated_from` metadata with the source directory path
- verify the value was stored successfully
- remember which remaining plaintext entries you reviewed so unchanged false positives are not suggested again later
- rewrite `.env` with migrated keys cleared

After migration, the file becomes:

```dotenv
DATABASE_PASSWORD=
API_KEY=
```

## Backend Prerequisites

Install and authenticate the backend you plan to use:

- 1Password: `op` CLI installed and authenticated
- Bitwarden: `bw` CLI installed, logged in, and unlocked as needed
- GPG: `gpg` installed, with a configured recipient for encryption during migration

You can verify local backend availability with:

```bash
pw-env check
```

## Security Notes

- Resolved values are printed as shell export statements, so use `eval` only in trusted local workflows
- Export formatting validates environment variable names and single-quote escapes values for shell safety
- Plaintext values in `.env` are not exported as secrets by the resolver; they are left in place and reported for migration
- GPG mode stores secrets in an encrypted file on disk rather than fetching them from an external password manager

## Development

Common commands:

```bash
cargo fmt
cargo test
cargo run -- check
```

To preview the manual locally:

```bash
npm install
npm run docs:dev
```

## Release

The repository includes a documented multi-stage release process. See `release-workflow.md` for the tagging, publishing, and optional macOS signing setup.