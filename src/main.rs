mod backend;
mod config;
mod env_file;
mod migrate;
mod output;
mod resolve;
mod shell;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use tracing::{debug, error, info};

#[derive(Parser)]
#[command(
    name = "pw-env",
    version,
    about = "Securely load environment variables from password managers",
    long_about = "pw-env resolves .env file entries from 1Password, Bitwarden, or GPG-encrypted files.\n\
                  Secrets never touch disk — they flow from the password manager through stdout into your shell.",
    arg_required_else_help = true
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Generate shell hook code for automatic .env loading on cd
    Init {
        /// Shell to generate hook for: bash, zsh, or fish
        shell: String,
    },
    /// Export resolved environment variables (for shell eval)
    Export {
        /// Directory to look for .env file in (defaults to current directory)
        dir: Option<PathBuf>,
        /// Shell syntax to use: bash, zsh, or fish
        #[arg(long, default_value = "bash")]
        shell: String,
    },
    /// Load and display resolved environment variables (human-readable)
    Load {
        /// Directory to look for .env file in (defaults to current directory)
        dir: Option<PathBuf>,
    },
    /// Migrate plaintext secrets from .env into the password manager
    Migrate {
        /// Directory containing the .env file (defaults to current directory)
        dir: Option<PathBuf>,
    },
    /// Check availability of password manager backends
    Check,
    /// Manage approved project-local override hashes
    Approvals {
        #[command(subcommand)]
        command: ApprovalCommands,
    },
    /// Print the default configuration file template
    ConfigTemplate,
}

#[derive(Subcommand)]
enum ApprovalCommands {
    /// List approved project override files and hashes
    List,
    /// Approve the current contents of a project override file or directory
    Approve {
        /// Path to a .pw-env.toml file or a directory containing one
        path: PathBuf,
    },
    /// Show the approval status for a project override file or directory
    Show {
        /// Path to a .pw-env.toml file or a directory containing one
        path: Option<PathBuf>,
    },
    /// Remove the approved hash for a project override file or directory
    Revoke {
        /// Path to a .pw-env.toml file or a directory containing one
        path: PathBuf,
    },
}

fn main() {
    let cli = Cli::parse();

    // Load config early (before logging setup) to get log settings
    let config = match config::Config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error loading config: {e}");
            std::process::exit(1);
        }
    };

    // Set up logging
    setup_logging(&config);

    if let Err(e) = run(cli, config) {
        error!("{e:#}");
        eprintln!("Error: {e:#}");
        std::process::exit(1);
    }
}

fn run(cli: Cli, _config: config::Config) -> Result<()> {
    match cli.command {
        Commands::Init { shell } => {
            print!("{}", shell::generate_hook(&shell));
            Ok(())
        }

        Commands::Export { dir, shell } => {
            let dir = resolve_dir(dir)?;
            let config = config::Config::load_for_dir(&dir)?;
            let shell_syntax = match shell.as_str() {
                "fish" => output::ShellSyntax::Fish,
                _ => output::ShellSyntax::Posix,
            };

            let env_path = match env_file::EnvFile::find(&dir) {
                Some(p) => p,
                None => {
                    debug!("No .env file in {}", dir.display());
                    return Ok(());
                }
            };

            let env_file = env_file::EnvFile::parse(&env_path)?;

            let likely_secrets = env_file.likely_secret_entries();
            if !likely_secrets.is_empty() {
                eprintln!(
                    "pw-env: warning: likely plaintext secrets found in .env: {}. Run `pw-env migrate` to secure them.",
                    summarize_entry_keys(&likely_secrets)
                );
            }

            let resolved = resolve::resolve_env_file(&env_file, &config, &dir)?;

            if resolved.is_empty() {
                debug!("No variables resolved");
                return Ok(());
            }

            // Output the export statements
            print!("{}", output::format_exports(&resolved, shell_syntax));

            // Output key tracking for the shell hook (to enable unloading on dir change)
            let keys: Vec<String> = resolved.keys().cloned().collect();
            let tracking = output::format_key_tracking(&keys);
            match shell_syntax {
                output::ShellSyntax::Posix => {
                    println!("__pw_env_previous_keys=\"{tracking}\"");
                }
                output::ShellSyntax::Fish => {
                    println!("set -g __pw_env_previous_keys \"{tracking}\"");
                }
            }

            info!("Exported {} variables from {}", resolved.len(), env_path.display());
            Ok(())
        }

        Commands::Load { dir } => {
            let dir = resolve_dir(dir)?;
            let config = config::Config::load_for_dir(&dir)?;
            let env_path = env_file::EnvFile::find(&dir)
                .ok_or_else(|| anyhow::anyhow!("No .env file found in {}", dir.display()))?;
            let env_file = env_file::EnvFile::parse(&env_path)?;

            eprintln!("Loading environment from {}", env_path.display());
            eprintln!("Backend: {}", config.effective_backend(&dir));
            eprintln!();

            let likely_secrets = env_file.likely_secret_entries();
            if !likely_secrets.is_empty() {
                eprintln!(
                    "Warning: likely plaintext secrets found in .env: {}",
                    summarize_entry_keys(&likely_secrets)
                );
                eprintln!();
            }

            // Show entry classification
            for entry in env_file.entries() {
                let status = match &entry.kind {
                    env_file::EntryKind::Empty => "  (resolve from backend)".to_string(),
                    env_file::EntryKind::OpReference(r) => format!("  (1Password: {r})"),
                    env_file::EntryKind::BwReference(r) => format!("  (Bitwarden: {r})"),
                    env_file::EntryKind::Plaintext(_) if entry.is_likely_secret() => {
                        "  ⚠ PLAINTEXT SECRET (run `pw-env migrate`)".to_string()
                    }
                    env_file::EntryKind::Plaintext(_) => "  (plaintext value)".to_string(),
                };
                eprintln!("  {}{}", entry.key, status);
            }
            eprintln!();

            // Resolve
            let resolved = resolve::resolve_env_file(&env_file, &config, &dir)?;
            eprintln!("Resolved {}/{} entries:", resolved.len(), env_file.resolvable_entries().len());
            for key in resolved.keys() {
                eprintln!("  {} = ****", key);
            }

            // Also print as export statements for piping
            print!("{}", output::format_exports(&resolved, output::ShellSyntax::Posix));

            Ok(())
        }

        Commands::Migrate { dir } => {
            let dir = resolve_dir(dir)?;
            let config = config::Config::load_for_dir(&dir)?;
            migrate::migrate(&dir, &config)
        }

        Commands::Check => {
            let dir = resolve_dir(None)?;
            let config = config::Config::load_for_dir(&dir)?;
            check_backends();
            eprintln!();
            check_config(&config, &dir);
            Ok(())
        }

        Commands::Approvals { command } => handle_approvals(command),

        Commands::ConfigTemplate => {
            print!("{}", config_template());
            Ok(())
        }
    }
}

fn summarize_entry_keys(entries: &[&env_file::EnvEntry]) -> String {
    const MAX_VISIBLE_KEYS: usize = 3;

    let mut keys: Vec<&str> = entries.iter().map(|entry| entry.key.as_str()).collect();
    keys.sort_unstable();

    let visible = keys
        .iter()
        .take(MAX_VISIBLE_KEYS)
        .copied()
        .collect::<Vec<_>>()
        .join(", ");

    if keys.len() > MAX_VISIBLE_KEYS {
        format!("{} (+{} more)", visible, keys.len() - MAX_VISIBLE_KEYS)
    } else {
        visible
    }
}

fn resolve_dir(dir: Option<PathBuf>) -> Result<PathBuf> {
    match dir {
        Some(d) => {
            let canonical = d.canonicalize()
                .with_context(|| format!("Directory not found: {}", d.display()))?;
            Ok(canonical)
        }
        None => {
            let dir = std::env::current_dir().context("Failed to determine current directory")?;
            dir.canonicalize()
                .with_context(|| format!("Directory not found: {}", dir.display()))
        }
    }
}

fn check_backends() {
    eprintln!("Checking password manager backends:");
    eprintln!();

    // 1Password
    match std::process::Command::new("op")
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .output()
    {
        Ok(output) if output.status.success() => {
            let version = String::from_utf8_lossy(&output.stdout);
            eprintln!("  1Password CLI (op): ✓ {}", version.trim());
        }
        _ => {
            eprintln!("  1Password CLI (op): ✗ not found");
        }
    }

    // Bitwarden
    match std::process::Command::new("bw")
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .output()
    {
        Ok(output) if output.status.success() => {
            let version = String::from_utf8_lossy(&output.stdout);
            eprintln!("  Bitwarden CLI (bw): ✓ {}", version.trim());
        }
        _ => {
            eprintln!("  Bitwarden CLI (bw): ✗ not found");
        }
    }

    // GPG
    match std::process::Command::new("gpg")
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .output()
    {
        Ok(output) if output.status.success() => {
            let version = String::from_utf8_lossy(&output.stdout);
            let first_line = version.lines().next().unwrap_or("unknown");
            eprintln!("  GnuPG (gpg): ✓ {}", first_line);
        }
        _ => {
            eprintln!("  GnuPG (gpg): ✗ not found");
        }
    }
}

fn check_config(config: &config::Config, dir: &std::path::Path) {
    let config_path = config::Config::config_path();
    if config_path.exists() {
        eprintln!("Configuration: {}", config_path.display());
        eprintln!("  Default backend: {}", config.defaults.backend);
        if let Some(ref vault) = config.defaults.op.vault {
            eprintln!("  1Password vault: {vault}");
        }
        if let Some(ref folder) = config.defaults.bw.folder {
            eprintln!("  Bitwarden folder: {folder}");
        }
        eprintln!("  GPG file pattern: {}", config.defaults.gpg.file_pattern);
        eprintln!("  Projects configured: {}", config.projects.len());
        if let Some(local_override) = config::Config::project_override_path(dir) {
            eprintln!(
                "  Project override file: {}",
                local_override.display()
            );
        }
    } else {
        eprintln!("Configuration: not found (using defaults)");
        eprintln!("  Create one with: pw-env config-template > {}", config_path.display());
    }
}

fn handle_approvals(command: ApprovalCommands) -> Result<()> {
    match command {
        ApprovalCommands::List => {
            if let Some(store_path) = config::Config::approval_store_path() {
                eprintln!("Approval store: {}", store_path.display());
            }

            let approvals = config::Config::approved_project_configs()?;
            if approvals.is_empty() {
                eprintln!("No approved project override files.");
                return Ok(());
            }

            eprintln!("Approved project override files:");
            for approval in approvals {
                eprintln!("  {}  {}", approval.hash, approval.path.display());
            }
            Ok(())
        }
        ApprovalCommands::Approve { path } => {
            let approval = config::Config::approve_project_override(&path)?;
            eprintln!("Approved project override: {}", approval.path.display());
            eprintln!("Stored hash: {}", approval.hash);
            Ok(())
        }
        ApprovalCommands::Show { path } => {
            let target = match path {
                Some(path) => path,
                None => resolve_dir(None)?,
            };
            let status = config::Config::project_override_approval_status(&target)?;

            eprintln!("Project override: {}", status.override_path.display());
            match status.current_hash.as_deref() {
                Some(hash) => eprintln!("Current hash: {hash}"),
                None => eprintln!("Current hash: unavailable"),
            }
            match status.approved_hash.as_deref() {
                Some(hash) => eprintln!("Approved hash: {hash}"),
                None => eprintln!("Approved hash: none"),
            }

            let state = match (&status.current_hash, &status.approved_hash) {
                (Some(current), Some(approved)) if current == approved => "approved",
                (Some(_), Some(_)) => "changed since approval",
                (Some(_), None) => "not approved",
                (None, Some(_)) => "approved file missing",
                (None, None) => "unknown",
            };
            eprintln!("Status: {state}");
            Ok(())
        }
        ApprovalCommands::Revoke { path } => {
            let revoked = config::Config::revoke_project_override_approval(&path)?;
            if revoked {
                eprintln!("Revoked approval for {}", path.display());
            } else {
                eprintln!("No approval entry found for {}", path.display());
            }
            Ok(())
        }
    }
}

fn config_template() -> String {
    r#"# pw-env configuration
# Place this file at ~/.config/pw-manager-env/config.toml

[defaults]
# Default backend: "op" (1Password), "bw" (Bitwarden), or "gpg" (GPG encrypted file)
backend = "op"

[defaults.op]
# Default 1Password vault to search in
# vault = "Development"
# 1Password account shorthand (for multiple accounts)
# account = "my-team"
# Default item name — if set, keys are resolved as fields on this item
# item = "project-env"

[defaults.bw]
# Default Bitwarden folder to search in
# folder = "env-secrets"
# Default Bitwarden organization
# organization = ""
# Default item name — if set, keys are resolved as custom fields on this item
# item = "project-env"

[defaults.gpg]
# Default encrypted file name to look for
file_pattern = ".env.gpg"
# GPG recipient for encrypting (required for `pw-env migrate` with GPG backend)
# recipient = "your-email@example.com"

[log]
# Log level: trace, debug, info, warn, error
level = "info"
# Log file path (optional, defaults to ~/.local/state/pw-manager-env/pw-env.log)
# file = "~/.local/state/pw-manager-env/pw-env.log"

# Per-project overrides
# Matched by directory path prefix — most specific match wins
#
# [[projects]]
# path = "/home/user/work/api-server"
# backend = "op"
# item = "api-server-env"
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

# Project-local override file
# Place this in a project directory as .pw-env.toml.
# pw-env will ask you to approve it the first time it sees the file,
# and again whenever its contents change.
#
# backend = "op"
# item = "api-server-env"
#
# [op]
# vault = "Work"
"#
    .to_string()
}

fn setup_logging(config: &config::Config) {
    use tracing_subscriber::EnvFilter;

    let log_level = &config.log.level;
    let filter = EnvFilter::try_new(log_level)
        .unwrap_or_else(|_| EnvFilter::new("info"));

    if let Some(ref log_file_path) = config.log.file {
        // Expand ~ in path
        let expanded = if let Some(rest) = log_file_path.strip_prefix("~/") {
            dirs::home_dir()
                .map(|h| h.join(rest))
                .unwrap_or_else(|| PathBuf::from(log_file_path))
        } else {
            PathBuf::from(log_file_path)
        };

        // Ensure parent directory exists
        if let Some(parent) = expanded.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        // Set up file-based logging
        if let Some(parent) = expanded.parent() {
            let filename = expanded
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let file_appender = tracing_appender::rolling::never(parent, filename);
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_writer(file_appender)
                .with_ansi(false)
                .with_target(false)
                .init();
            return;
        }
    }

    // Fallback: log to stderr
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();
}

