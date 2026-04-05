---
title: pw-env
hide:
  - toc
---

<div class="home-hero" markdown="1">
<div class="home-hero__copy" markdown="1">

<span class="home-hero__eyebrow">pw-env manual</span>

# Keep .env in your project. Keep secrets out of it.

pw-env resolves empty env keys from 1Password, Bitwarden, or GPG-backed files, then streams the results straight into your shell.

<div class="home-hero__actions">
  <a class="md-button md-button--primary" href="getting-started/installation/">Install pw-env</a>
  <a class="md-button" href="getting-started/first-project/">Build your first project flow</a>
</div>
</div>
<div class="home-hero__panel" markdown="1">
<h3>Example</h3>

```console title="Load the current directory into bash"
$ eval "$(pw-env export . --shell bash)"
```

```dotenv title=".env"
DATABASE_URL=
API_KEY=op://Development/my-app/api_key
LOG_LEVEL=debug # no-migrate
```

```bash title="Environment"
DATABASE_URL=sqlite:///example.db
API_KEY=XdASdf923.....
LOG_LEVEL=debug # no-migrate
```

</div>
</div>

## Highlights

<div class="card-grid" markdown="1">

<div class="card" markdown="1">
### Secrets stay in the backend

Empty values and explicit references are resolved at runtime. pw-env exports resolved keys to stdout instead of writing a generated `.env` file back to disk.
</div>

<div class="card" markdown="1">
### Works with the shell you already use

Use `pw-env export` for one-off loading, or install a shell hook with `pw-env init bash`, `pw-env init zsh`, or `pw-env init fish`.
</div>

<div class="card" markdown="1">
### Built for mixed env files

Secret-like plaintext values can be migrated into the backend, while safe local values can stay in the file with `# no-migrate`.
</div>

<div class="card" markdown="1">
### Trust is explicit

Project-local overrides in `.pw-env.toml` and credential fetching from `.env` are approved separately and re-checked when the file contents change.
</div>

</div>

## Install

=== "Standalone installer"

    ```console
    $ curl -fsSL https://m42e.de/pw-manager-env-rs/install.sh | bash
    ```

=== "Specific release"

    ```console
    $ curl -fsSL https://m42e.de/pw-manager-env-rs/install.sh | bash -s -- --version v0.2.8
    ```

=== "Build from source"

    ```console
    $ cargo build --release
    $ ./target/release/pw-env --help
    ```

## Learn the flow

<div class="manual-grid">
  <a class="manual-card" href="getting-started/installation/">
    <strong>Installation</strong>
    <span>Install the binary, check supported targets, and preview the manual locally.</span>
  </a>
  <a class="manual-card" href="getting-started/first-project/">
    <strong>First project</strong>
    <span>Set up a `.env`, choose a backend, and run your first export.</span>
  </a>
  <a class="manual-card" href="guides/migrate-secrets/">
    <strong>Migrate plaintext secrets</strong>
    <span>Move existing values into 1Password, Bitwarden, or GPG without rewriting safe local settings.</span>
  </a>
  <a class="manual-card" href="guides/approvals/">
    <strong>Approvals and trust</strong>
    <span>Understand `.pw-env.toml` approvals, `.env` hash approvals, and project-wide fetch grants.</span>
  </a>
  <a class="manual-card" href="concepts/resolution-model/">
    <strong>Resolution model</strong>
    <span>See how pw-env classifies entries and routes them to the correct backend.</span>
  </a>
  <a class="manual-card" href="reference/cli/">
    <strong>CLI reference</strong>
    <span>Browse the commands, usage forms, and the approvals subcommands in one place.</span>
  </a>
</div>

## What a project looks like

```text
my-service/
├── .env
├── .pw-env.toml
└── .git/
```

```dotenv title=".env"
DATABASE_URL=
API_KEY=bw://env-secrets/my-service/api_key
LOG_LEVEL=debug # no-migrate
```

Use a global config for defaults, and add `.pw-env.toml` only when a project needs a local override. The local override is discovered by walking upward from the current directory until the repository root.