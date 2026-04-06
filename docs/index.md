---
layout: home

hero:
  name: Keep .env in your project.
  text: Keep secrets out of it.
  tagline: pw-env resolves empty env keys from 1Password, Bitwarden, or GPG-backed files, then streams the results straight into your shell.
  image:
    src: /assets/images/Logo-pw-env@3x.png
    alt: pw-env
  actions:
    - theme: brand
      text: Install pw-env
      link: /getting-started/installation
    - theme: alt
      text: Build your first project flow
      link: /getting-started/first-project

features:
  - title: Secrets stay in the backend
    details: Empty values and explicit references are resolved at runtime. pw-env exports resolved keys to stdout instead of writing a generated .env file back to disk.
  - title: Works with the shell you already use
    details: Use pw-env export for one-off loading, or install a shell hook with pw-env init bash, pw-env init zsh, pw-env init fish, or pw-env init powershell.
  - title: Built for mixed env files
    details: Secret-like plaintext values can be migrated into the backend, while safe local values can stay in the file with # no-migrate.
  - title: Trust is explicit
    details: Project-local overrides in .pw-env.toml and credential fetching from .env are approved separately and re-checked when the file contents change.
---

<div class="home-strip">
  <span>Rust CLI</span>
  <span>1Password, Bitwarden, and GPG</span>
  <span>Automatic activation</span>
</div>

<div class="home-intro-grid">
  <div class="home-callout home-callout--accent">
    <strong>Why it exists</strong>
    <p>pw-env keeps the project-facing ergonomics of a normal .env file while moving secret resolution to the edge of your shell session.</p>
  </div>
  <div class="home-callout">
    <strong>What changes in practice</strong>
    <p>Developers keep checked-in env shape, approvals stay explicit, and secret values stop drifting into repositories and local plaintext copies.</p>
  </div>
</div>

<div class="example-panel">

## Example

```console [Load the current directory into bash]
$ eval "$(pw-env export . --shell bash)"
```

```dotenv [.env]
DATABASE_URL=
API_KEY=op://Development/my-app/api_key
LOG_LEVEL=debug # no-migrate
```

```bash [Environment]
DATABASE_URL=sqlite:///example.db
API_KEY=XdASdf923.....
LOG_LEVEL=debug # no-migrate
```

</div>

## Fast path

<div class="fast-path-grid">
  <div class="fast-path-step">
    <span>01</span>
    <strong>Shape the project env</strong>
    <p>Leave secret keys empty or point them at a specific backend reference.</p>
  </div>
  <div class="fast-path-step">
    <span>02</span>
    <strong>Pick a default backend</strong>
    <p>Resolve empty values through 1Password, Bitwarden, or a GPG-backed env file.</p>
  </div>
  <div class="fast-path-step">
    <span>03</span>
    <strong>Load on demand or on cd</strong>
    <p>Export once for a shell session or install a hook that follows your working directory.</p>
  </div>
</div>

## Install

::: code-group

```console [Standalone installer]
$ curl -fsSL https://m42e.de/pw-env/install.sh | bash
```

```powershell [Standalone installer (PowerShell)]
PS> & ([scriptblock]::Create((irm https://m42e.de/pw-env/install.ps1)))
```

```console [Specific release]
$ curl -fsSL https://m42e.de/pw-env/install.sh | bash -s -- --version v0.2.8
```

```console [Build from source]
$ cargo build --release
$ ./target/release/pw-env --help
```

:::

## Learn the flow

<div class="manual-grid">
  <a class="manual-card" href="./getting-started/installation">
    <strong>Installation</strong>
    <span>Install the binary, check supported targets, and preview the manual locally.</span>
  </a>
  <a class="manual-card" href="./getting-started/first-project">
    <strong>First project</strong>
    <span>Set up a .env, choose a backend, and run your first export.</span>
  </a>
  <a class="manual-card" href="./guides/migrate-secrets">
    <strong>Migrate plaintext secrets</strong>
    <span>Move existing values into 1Password, Bitwarden, or GPG without rewriting safe local settings.</span>
  </a>
  <a class="manual-card" href="./guides/approvals">
    <strong>Approvals and trust</strong>
    <span>Understand .pw-env.toml approvals, .env hash approvals, and project-wide fetch grants.</span>
  </a>
  <a class="manual-card" href="./concepts/resolution-model">
    <strong>Resolution model</strong>
    <span>See how pw-env classifies entries and routes them to the correct backend.</span>
  </a>
  <a class="manual-card" href="./reference/cli">
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

```dotenv [.env]
DATABASE_URL=
API_KEY=bw://env-secrets/my-service/api_key
LOG_LEVEL=debug # no-migrate
```

Use a global config for defaults, and add .pw-env.toml only when a project needs a local override. The local override is discovered by walking upward from the current directory until the repository root.