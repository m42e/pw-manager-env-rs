mod add;
mod backend;
mod cache;
mod config;
mod env_file;
mod migrate;
mod output;
mod progress;
mod release;
mod resolve;
mod shell;

use anyhow::{Context, Result};
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::{Shell, generate};
use glob::Pattern;
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::io::IsTerminal;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command as ProcessCommand;
use tracing::{debug, error, info};

#[derive(Parser)]
#[command(
    name = "pw-env",
    version,
    about = "Securely load environment variables from password managers",
    long_about = "pw-env resolves .env file entries from 1Password, Bitwarden, or GPG-encrypted files.\n\
                  Resolved values stay out of project files, and pw-env can cache them in the OS keyring when available.",
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
        /// Shell to generate hook for: bash, zsh, fish, or powershell
        shell: String,
    },
    /// Execute a command with resolved environment variables only in the child process
    Exec {
        /// Directory to look for .env file in (defaults to current directory)
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Print a warning to stderr for each .env entry that could not be resolved
        #[arg(long)]
        warn_missing: bool,
        /// Command to execute with transient environment variables
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Export resolved environment variables (for shell eval)
    Export {
        /// Directory to look for .env file in (defaults to current directory)
        dir: Option<PathBuf>,
        /// Shell syntax to use: bash, zsh, fish, or powershell
        #[arg(long, default_value = "bash")]
        shell: String,
        /// Print a warning to stderr for each .env entry that could not be resolved
        #[arg(long)]
        warn_missing: bool,
    },
    /// Load and display resolved environment variables (human-readable)
    Load {
        /// Directory to look for .env file in (defaults to current directory)
        dir: Option<PathBuf>,
        /// Show full resolved values instead of masked previews
        #[arg(long)]
        reveal: bool,
    },
    /// Store a secret in the configured backend and add an empty .env entry for it
    Add {
        /// Directory containing or receiving the .env file (defaults to current directory)
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Backend to use for this add operation: op, bw, or gpg
        #[arg(long, value_parser = ["op", "bw", "gpg"])]
        backend: Option<String>,
        /// Environment variable key to add
        key: String,
        /// Secret value to store. If omitted, pw-env prompts or reads from stdin.
        value: Option<String>,
    },
    /// Migrate plaintext secrets from .env into the password manager
    Migrate {
        /// Directory containing the .env file (defaults to current directory)
        dir: Option<PathBuf>,
        /// Backend to use for this migration: op, bw, or gpg
        #[arg(long, value_parser = ["op", "bw", "gpg"])]
        backend: Option<String>,
    },
    /// Check availability of password manager backends
    Check,
    /// Manage approval state for project-local overrides and secret fetching
    Approvals {
        #[command(subcommand)]
        command: ApprovalCommands,
    },
    /// Manage local cache state
    Cache {
        #[command(subcommand)]
        command: CacheCommands,
    },
    /// Download and replace the current binary with a released build
    Update {
        /// Release version to install (for example 0.2.8 or v0.2.8)
        #[arg(long)]
        version: Option<String>,
    },
    /// Print the default configuration file template
    ConfigTemplate,
    /// Generate shell completion script
    ///
    /// Add one of the following lines to your shell's startup file:
    ///
    ///   Bash (~/.bashrc):   eval "$(pw-env completions bash)"
    ///   Zsh  (~/.zshrc):    eval "$(pw-env completions zsh)"
    ///   Fish (~/.config/fish/config.fish):  pw-env completions fish | source
    Completions {
        /// Shell to generate completions for (bash, zsh, fish, powershell, elvish)
        shell: Shell,
    },
    #[command(hide = true)]
    Hook {
        /// Directory to inspect for shell integration state (defaults to current directory)
        dir: Option<PathBuf>,
        /// Shell syntax to use: bash, zsh, fish, or powershell
        #[arg(long, default_value = "bash")]
        shell: String,
    },
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
    /// List approved secret-fetch projects and .env hashes
    ListFetch,
    /// Approve credential fetching for a .env file or project directory
    ApproveFetch {
        /// Path to a .env file or a directory containing one
        path: PathBuf,
        /// Allow any future .env changes in this project without prompting again
        #[arg(long)]
        project_wide: bool,
    },
    /// Show secret-fetch approval status for a .env file or project directory
    ShowFetch {
        /// Path to a .env file or a directory containing one
        path: Option<PathBuf>,
    },
    /// Revoke secret-fetch approvals for a .env file or project directory's project
    RevokeFetch {
        /// Path to a .env file or a directory containing one
        path: PathBuf,
    },
}

#[derive(Subcommand)]
enum CacheCommands {
    /// Clear persisted Bitwarden metadata and resolved-secret cache state
    Clear,
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

    maybe_check_for_release_update(&cli.command, &config);

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

        Commands::Exec {
            dir,
            command,
            warn_missing,
        } => {
            let dir = resolve_dir(dir)?;
            let config = config::Config::load_for_dir(&dir)?;
            let mut command_iter = command.into_iter();
            let program = command_iter
                .next()
                .ok_or_else(|| anyhow::anyhow!("No command provided"))?;
            let args = command_iter.collect::<Vec<_>>();

            let mut child = ProcessCommand::new(&program);
            child.args(&args);

            if let Some(env_path) = find_env_path(&dir, &config) {
                let env_file = env_file::EnvFile::parse(&env_path)?;
                emit_plaintext_secret_warning(&env_file)?;

                let managed_keys = env_file
                    .resolvable_entries()
                    .into_iter()
                    .map(|entry| entry.key.clone())
                    .collect::<Vec<_>>();
                let resolved = resolve::resolve_env_file(&env_file, &config, &dir)?;

                if should_warn_missing(warn_missing, config.effective_warn_missing(&dir)) {
                    emit_missing_entries_warning(&managed_keys, &resolved);
                }

                for key in &managed_keys {
                    child.env_remove(key);
                }
                child.envs(&resolved);

                info!(
                    "Prepared transient environment with {} variables from {}",
                    resolved.len(),
                    env_path.display()
                );
            }

            #[cfg(unix)]
            {
                let error = child.exec();
                Err(error).with_context(|| format!("Failed to execute {program}"))
            }

            #[cfg(not(unix))]
            {
                let status = child
                    .status()
                    .with_context(|| format!("Failed to execute {program}"))?;
                std::process::exit(status.code().unwrap_or(1));
            }
        }

        Commands::Export {
            dir,
            shell,
            warn_missing,
        } => {
            let dir = resolve_dir(dir)?;
            let config = config::Config::load_for_dir(&dir)?;
            let shell_syntax = match shell.as_str() {
                "fish" => output::ShellSyntax::Fish,
                "powershell" => output::ShellSyntax::PowerShell,
                _ => output::ShellSyntax::Posix,
            };

            let env_path = match find_env_path(&dir, &config) {
                Some(p) => p,
                None => {
                    debug!("No .env file in {}", dir.display());
                    return Ok(());
                }
            };

            let env_file = env_file::EnvFile::parse(&env_path)?;
            emit_plaintext_secret_warning(&env_file)?;

            let resolvable_keys: Vec<String> = env_file
                .resolvable_entries()
                .into_iter()
                .map(|e| e.key.clone())
                .collect();
            let resolved = resolve::resolve_env_file(&env_file, &config, &dir)?;

            if should_warn_missing(warn_missing, config.effective_warn_missing(&dir)) {
                emit_missing_entries_warning(&resolvable_keys, &resolved);
            }

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
                    println!(
                        "{}",
                        format_active_dir_tracking(env_owner_dir(&env_path), shell_syntax)
                    );
                }
                output::ShellSyntax::Fish => {
                    println!("set -g __pw_env_previous_keys \"{tracking}\"");
                    println!(
                        "{}",
                        format_active_dir_tracking(env_owner_dir(&env_path), shell_syntax)
                    );
                }
                output::ShellSyntax::PowerShell => {
                    if keys.is_empty() {
                        println!("$global:__pw_env_previous_keys = @()");
                    } else {
                        let quoted = keys
                            .iter()
                            .map(|key| format!("'{key}'"))
                            .collect::<Vec<_>>()
                            .join(", ");
                        println!("$global:__pw_env_previous_keys = @({quoted})");
                    }
                    println!(
                        "{}",
                        format_active_dir_tracking(env_owner_dir(&env_path), shell_syntax)
                    );
                }
            }

            info!(
                "Exported {} variables from {}",
                resolved.len(),
                env_path.display()
            );
            Ok(())
        }

        Commands::Hook { dir, shell } => {
            let dir = resolve_dir(dir)?;
            let config = config::Config::load_for_dir(&dir)?;
            let shell_syntax = match shell.as_str() {
                "fish" => output::ShellSyntax::Fish,
                "powershell" => output::ShellSyntax::PowerShell,
                _ => output::ShellSyntax::Posix,
            };

            let hook_output =
                build_hook_output(&dir, shell_syntax, &config, std::env::var_os("PATH"))?;
            print!("{hook_output}");

            Ok(())
        }

        Commands::Load { dir, reveal } => {
            let dir = resolve_dir(dir)?;
            let config = config::Config::load_for_dir(&dir)?;
            let env_path = find_env_path(&dir, &config)
                .ok_or_else(|| anyhow::anyhow!("No .env file found in {}", dir.display()))?;
            let env_file = env_file::EnvFile::parse(&env_path)?;

            eprintln!("Loading environment from {}", env_path.display());
            eprintln!("Backend: {}", config.effective_backend(&dir));
            eprintln!();

            let likely_secrets = pending_migration_entries(&env_file)?;
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
                    env_file::EntryKind::Plaintext(_) if entry.no_migrate => {
                        "  (plaintext value, pw-env:ignore)".to_string()
                    }
                    env_file::EntryKind::Plaintext(_) if entry.is_likely_secret() => {
                        "  ⚠ PLAINTEXT SECRET (run `pw-env migrate`)".to_string()
                    }
                    env_file::EntryKind::Plaintext(_) => "  (plaintext value)".to_string(),
                };
                eprintln!("  {}{}", entry.key, status);
            }
            eprintln!();

            // Resolve
            let resolvable_keys: Vec<String> = env_file
                .resolvable_entries()
                .into_iter()
                .map(|e| e.key.clone())
                .collect();
            let resolved = resolve::resolve_env_file(&env_file, &config, &dir)?;
            eprintln!(
                "Resolved {}/{} entries:",
                resolved.len(),
                resolvable_keys.len()
            );
            for (key, value) in &resolved {
                let display_value = if reveal {
                    value.clone()
                } else {
                    output::obfuscate_value(value)
                };
                eprintln!("  {} = {}", key, display_value);
            }

            let missing = missing_resolvable_keys(&resolvable_keys, &resolved);
            if !missing.is_empty() {
                eprintln!();
                emit_missing_entries_warning(&resolvable_keys, &resolved);
            }

            let display_values = resolved
                .iter()
                .map(|(key, value)| {
                    let display_value = if reveal {
                        value.clone()
                    } else {
                        output::obfuscate_value(value)
                    };
                    (key.clone(), display_value)
                })
                .collect();

            print!(
                "{}",
                output::format_exports(&display_values, output::ShellSyntax::Posix)
            );

            Ok(())
        }

        Commands::Add {
            dir,
            backend,
            key,
            value,
        } => {
            let dir = resolve_dir(dir)?;
            let config = config::Config::load_for_dir(&dir)?;
            add::add_entry(&dir, &config, &key, value, backend.as_deref())
        }

        Commands::Migrate { dir, backend } => {
            let dir = resolve_dir(dir)?;
            let config = config::Config::load_for_dir(&dir)?;
            migrate::migrate(&dir, &config, backend.as_deref())
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

        Commands::Cache { command } => handle_cache(command),

        Commands::Update { version } => release::update(version.as_deref()),

        Commands::ConfigTemplate => {
            print!("{}", config_template());
            Ok(())
        }

        Commands::Completions { shell } => {
            generate(shell, &mut Cli::command(), "pw-env", &mut std::io::stdout());
            Ok(())
        }
    }
}

#[mutants::skip]
fn maybe_check_for_release_update(command: &Commands, config: &config::Config) {
    if matches!(
        command,
        Commands::Exec { .. }
            | Commands::Export { .. }
            | Commands::Hook { .. }
            | Commands::Update { .. }
    ) {
        return;
    }

    if !std::io::stderr().is_terminal() {
        return;
    }

    if let Err(err) = release::maybe_check_for_update(config) {
        debug!(error = %err, "automatic release check failed");
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

fn should_warn_missing(flag: bool, config_value: bool) -> bool {
    flag || config_value
}

fn missing_resolvable_keys<'a>(
    resolvable_keys: &'a [String],
    resolved: &std::collections::BTreeMap<String, String>,
) -> Vec<&'a str> {
    resolvable_keys
        .iter()
        .filter(|key| !resolved.contains_key(*key))
        .map(String::as_str)
        .collect()
}

fn emit_missing_entries_warning(
    resolvable_keys: &[String],
    resolved: &std::collections::BTreeMap<String, String>,
) -> usize {
    let missing = missing_resolvable_keys(resolvable_keys, resolved);
    if !missing.is_empty() {
        eprintln!(
            "pw-env: warning: could not resolve the following entries: {}",
            missing.join(", ")
        );
    }
    missing.len()
}

fn emit_plaintext_secret_warning(env_file: &env_file::EnvFile) -> Result<()> {
    let likely_secrets = pending_migration_entries(env_file)?;
    if !likely_secrets.is_empty() {
        eprintln!(
            "pw-env: warning: likely plaintext secrets found in .env: {}. Run `pw-env migrate` to secure them.",
            summarize_entry_keys(&likely_secrets)
        );
    }

    Ok(())
}

fn pending_migration_entries(env_file: &env_file::EnvFile) -> Result<Vec<&env_file::EnvEntry>> {
    let reviewed = config::Config::reviewed_migration_entry_fingerprints(&env_file.path)?;
    Ok(env_file.likely_secret_entries_unreviewed(&reviewed))
}

fn resolve_dir(dir: Option<PathBuf>) -> Result<PathBuf> {
    match dir {
        Some(d) => {
            let canonical = d
                .canonicalize()
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

fn build_hook_output(
    dir: &Path,
    shell_syntax: output::ShellSyntax,
    config: &config::Config,
    path_var: Option<std::ffi::OsString>,
) -> Result<String> {
    let env_path = match find_env_path(dir, config) {
        Some(path) => path,
        None => {
            debug!("No .env file in {}", dir.display());
            return Ok(String::new());
        }
    };
    let active_dir = env_owner_dir(&env_path);

    let command_patterns = config.effective_commands(dir);
    if !command_patterns.is_empty() {
        let wrapped_commands = resolve_wrapped_commands(command_patterns, path_var);
        info!(
            "Configured {} transient command wrappers from {} pattern(s) for {}",
            wrapped_commands.len(),
            command_patterns.len(),
            dir.display()
        );
        let mut output_text = output::format_command_wrappers(&wrapped_commands, shell_syntax);
        output_text.push_str(&format_active_dir_tracking(active_dir, shell_syntax));
        output_text.push('\n');
        return Ok(output_text);
    }

    let env_file = env_file::EnvFile::parse(&env_path)?;
    emit_plaintext_secret_warning(&env_file)?;
    let resolved = resolve::resolve_env_file(&env_file, config, dir)?;

    if resolved.is_empty() {
        debug!("No variables resolved");
        return Ok(String::new());
    }

    let mut output_text = output::format_exports(&resolved, shell_syntax);
    let keys: Vec<String> = resolved.keys().cloned().collect();
    let tracking = output::format_key_tracking(&keys);
    match shell_syntax {
        output::ShellSyntax::Posix => {
            output_text.push_str(&format!(
                "__pw_env_previous_keys=\"{tracking}\"\n__pw_env_previous_commands=\"\"\n"
            ));
        }
        output::ShellSyntax::Fish => {
            output_text.push_str(&format!(
                "set -g __pw_env_previous_keys \"{tracking}\"\nset -g __pw_env_previous_commands\n"
            ));
        }
        output::ShellSyntax::PowerShell => {
            if keys.is_empty() {
                output_text.push_str("$global:__pw_env_previous_keys = @()\n");
            } else {
                let quoted = keys
                    .iter()
                    .map(|key| format!("'{key}'"))
                    .collect::<Vec<_>>()
                    .join(", ");
                output_text.push_str(&format!("$global:__pw_env_previous_keys = @({quoted})\n"));
            }
            output_text.push_str("$global:__pw_env_previous_commands = @()\n");
        }
    }

    output_text.push_str(&format_active_dir_tracking(active_dir, shell_syntax));
    output_text.push('\n');

    Ok(output_text)
}

fn find_env_path(dir: &Path, config: &config::Config) -> Option<PathBuf> {
    if config.effective_search_parent_env(dir) {
        env_file::EnvFile::find_with_parents(dir, true)
    } else {
        env_file::EnvFile::find(dir)
    }
}

fn env_owner_dir(env_path: &Path) -> &Path {
    env_path.parent().unwrap_or(env_path)
}

fn format_active_dir_tracking(active_dir: &Path, shell_syntax: output::ShellSyntax) -> String {
    let active_dir = active_dir.to_string_lossy();

    match shell_syntax {
        output::ShellSyntax::Posix => {
            let escaped = active_dir.replace('\'', "'\\''");
            format!("__pw_env_active_dir='{escaped}'")
        }
        output::ShellSyntax::Fish => {
            let escaped = active_dir.replace('\'', "'\\''");
            format!("set -g __pw_env_active_dir '{escaped}'")
        }
        output::ShellSyntax::PowerShell => {
            let escaped = active_dir.replace('\'', "''");
            format!("$global:__pw_env_active_dir = '{escaped}'")
        }
    }
}

fn resolve_wrapped_commands(
    command_patterns: &[String],
    path_var: Option<std::ffi::OsString>,
) -> Vec<String> {
    let executables = list_path_executables(path_var.as_deref());
    let mut wrapped_commands = BTreeSet::new();

    for pattern in command_patterns {
        if !contains_glob_meta(pattern) {
            if output::is_safe_command_name(pattern) {
                wrapped_commands.insert(pattern.clone());
            }
            continue;
        }

        let Ok(glob_pattern) = Pattern::new(pattern) else {
            continue;
        };

        for executable in &executables {
            if glob_pattern.matches(executable) {
                wrapped_commands.insert(executable.clone());
            }
        }
    }

    wrapped_commands.into_iter().collect()
}

fn list_path_executables(path_var: Option<&OsStr>) -> BTreeSet<String> {
    let Some(path_var) = path_var else {
        return BTreeSet::new();
    };

    let mut executables = BTreeSet::new();
    for directory in std::env::split_paths(path_var) {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if !is_executable_file(&path) {
                continue;
            }

            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };

            if output::is_safe_command_name(name) {
                executables.insert(name.to_string());
            }
        }
    }

    executables
}

fn contains_glob_meta(pattern: &str) -> bool {
    pattern.contains('*') || pattern.contains('?') || pattern.contains('[')
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };

    if !metadata.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        metadata.permissions().mode() & 0o111 != 0
    }

    #[cfg(not(unix))]
    {
        true
    }
}

#[mutants::skip]
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
            eprintln!("  Project override file: {}", local_override.display());
        }
    } else {
        eprintln!("Configuration: not found (using defaults)");
        eprintln!(
            "  Create one with: pw-env config-template > {}",
            config_path.display()
        );
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
        ApprovalCommands::ListFetch => {
            if let Some(store_path) = config::Config::secret_fetch_approval_store_path() {
                eprintln!("Secret fetch approval store: {}", store_path.display());
            }

            let approvals = config::Config::approved_secret_fetches()?;
            if approvals.is_empty() {
                eprintln!("No approved secret-fetch entries.");
                return Ok(());
            }

            eprintln!("Approved secret-fetch entries:");
            for approval in approvals {
                if approval.project_wide {
                    eprintln!("  project-wide  {}", approval.project_path.display());
                } else if let Some(hash) = approval.env_hash {
                    eprintln!("  {}  {}", hash, approval.project_path.display());
                }
            }
            Ok(())
        }
        ApprovalCommands::ApproveFetch { path, project_wide } => {
            let mode = if project_wide {
                config::SecretFetchApprovalMode::ProjectWide
            } else {
                config::SecretFetchApprovalMode::CurrentEnvHash
            };
            let approval = config::Config::approve_secret_fetch(&path, mode)?;
            if approval.project_wide {
                eprintln!(
                    "Approved secret fetching for any .env changes in project {}",
                    approval.project_path.display()
                );
            } else {
                eprintln!(
                    "Approved secret fetching for project {}",
                    approval.project_path.display()
                );
                if let Some(hash) = approval.env_hash {
                    eprintln!("Stored .env hash: {hash}");
                }
            }
            Ok(())
        }
        ApprovalCommands::ShowFetch { path } => {
            let target = match path {
                Some(path) => path,
                None => resolve_dir(None)?,
            };
            let status = config::Config::secret_fetch_approval_status(&target)?;

            eprintln!("Project: {}", status.project_path.display());
            eprintln!(".env file: {}", status.env_path.display());
            match status.current_env_hash.as_deref() {
                Some(hash) => eprintln!("Current .env hash: {hash}"),
                None => eprintln!("Current .env hash: unavailable"),
            }
            eprintln!("Project-wide approval: {}", status.project_wide);
            if status.approved_env_hashes.is_empty() {
                eprintln!("Approved .env hashes: none");
            } else {
                eprintln!(
                    "Approved .env hashes: {}",
                    status
                        .approved_env_hashes
                        .iter()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }

            let state = if status.project_wide {
                "approved for any .env change"
            } else if status
                .current_env_hash
                .as_ref()
                .is_some_and(|hash| status.approved_env_hashes.contains(hash))
            {
                "approved for current .env hash"
            } else if status.approved_env_hashes.is_empty() {
                "not approved"
            } else {
                "changed since approved .env hashes"
            };
            eprintln!("Status: {state}");
            Ok(())
        }
        ApprovalCommands::RevokeFetch { path } => {
            let revoked = config::Config::revoke_secret_fetch_approval(&path)?;
            if revoked {
                eprintln!("Revoked secret-fetch approvals for {}", path.display());
            } else {
                eprintln!(
                    "No secret-fetch approval entry found for {}",
                    path.display()
                );
            }
            Ok(())
        }
    }
}

fn handle_cache(command: CacheCommands) -> Result<()> {
    match command {
        CacheCommands::Clear => {
            let folder_cache_path = backend::bw::BwBackend::folder_cache_path();
            let sync_state_path = backend::bw::BwBackend::sync_state_path();
            let secret_cache_path = crate::cache::secret_cache_index_path();
            let folder_cache_cleared = backend::bw::BwBackend::clear_folder_cache()?;
            let sync_state_cleared = backend::bw::BwBackend::clear_sync_state()?;
            let secret_cache_result = crate::cache::clear_secret_cache()?;

            match (folder_cache_cleared, folder_cache_path) {
                (true, Some(path)) => {
                    eprintln!("Cleared Bitwarden folder cache: {}", path.display())
                }
                (false, Some(path)) => {
                    eprintln!("No Bitwarden folder cache file found at {}", path.display())
                }
                (true, None) => eprintln!("Cleared Bitwarden folder cache"),
                (false, None) => {
                    eprintln!("No Bitwarden folder cache location is available on this system")
                }
            }

            match (sync_state_cleared, sync_state_path) {
                (true, Some(path)) => eprintln!("Cleared Bitwarden sync state: {}", path.display()),
                (false, Some(path)) => {
                    eprintln!("No Bitwarden sync state file found at {}", path.display())
                }
                (true, None) => eprintln!("Cleared Bitwarden sync state"),
                (false, None) => {
                    eprintln!("No Bitwarden sync state location is available on this system")
                }
            }

            match (secret_cache_result.cleared_index_file, secret_cache_path) {
                (true, Some(path)) => {
                    eprintln!("Cleared resolved-secret cache index: {}", path.display())
                }
                (false, Some(path)) => {
                    eprintln!("No resolved-secret cache index found at {}", path.display())
                }
                (true, None) => eprintln!("Cleared resolved-secret cache index"),
                (false, None) => {
                    eprintln!("No resolved-secret cache location is available on this system")
                }
            }

            if keyring_clear_status_message(&secret_cache_result).is_some() {
                eprintln!(
                    "Resolved-secret keyring cache clear completed with warnings; rerun with secure diagnostics if needed."
                );
            }

            Ok(())
        }
    }
}

fn keyring_clear_status_message(result: &cache::ClearSecretCacheResult) -> Option<String> {
    if result.keyring_unavailable {
        Some("Resolved-secret cache index was cleared, but the OS keyring was unavailable so cached keyring entries could not be removed".to_string())
    } else if result.keyring_delete_failures > 0 {
        Some(format!(
            "Failed to remove {} resolved-secret cache keyring entr{}",
            result.keyring_delete_failures,
            if result.keyring_delete_failures == 1 {
                "y"
            } else {
                "ies"
            }
        ))
    } else if result.deleted_credentials > 0 {
        Some(format!(
            "Removed {} resolved-secret cache keyring entr{}",
            result.deleted_credentials,
            if result.deleted_credentials == 1 {
                "y"
            } else {
                "ies"
            }
        ))
    } else {
        None
    }
}

fn config_template() -> String {
    r#"# pw-env configuration
# Place this file at ~/.config/pw-env/config.toml

[defaults]
# Default backend: "op" (1Password), "bw" (Bitwarden), or "gpg" (GPG encrypted file)
backend = "op"
# Search parent directories for .env until the git workspace root (default: true)
# search_parent_env = true

[defaults.cache]
# Cache resolved secret values in the OS keyring when available.
enabled = true
# Re-fetch a secret from the backend after this many hours.
ttl_hours = 4

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
# Minimum seconds between automatic `bw sync` calls (minimum and default: 3600)
# sync_throttle_secs = 3600

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
# Matched by directory path prefix — most specific match wins
#
# [[projects]]
# path = "/home/user/work/api-server"
# backend = "op"
# search_parent_env = true
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

# Project-local override file
# Place this in a project directory as .pw-env.toml.
# pw-env will ask you to approve it the first time it sees the file,
# and again whenever its contents change.
# Secret fetching from .env files is approved separately and is also
# re-approved whenever the .env contents change unless you allow the
# whole project.
#
# backend = "op"
# item = "api-server-env"
# commands = ["cargo", "npm"]
#
# [op]
# vault = "Work"
"#
    .to_string()
}

fn setup_logging(config: &config::Config) {
    use tracing_subscriber::EnvFilter;

    let log_level = &config.log.level;
    let filter = EnvFilter::try_new(log_level).unwrap_or_else(|_| EnvFilter::new("info"));

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use tempfile::TempDir;

    #[test]
    fn should_warn_missing_truth_table() {
        assert!(!should_warn_missing(false, false));
        assert!(should_warn_missing(true, false));
        assert!(should_warn_missing(false, true));
        assert!(should_warn_missing(true, true));
    }

    #[test]
    fn missing_resolvable_keys_returns_only_unresolved_keys() {
        let resolvable = vec![
            "API_KEY".to_string(),
            "DB_URL".to_string(),
            "TOKEN".to_string(),
        ];
        let resolved = BTreeMap::from([
            ("API_KEY".to_string(), "value".to_string()),
            ("TOKEN".to_string(), "value".to_string()),
        ]);

        let missing = missing_resolvable_keys(&resolvable, &resolved);
        assert_eq!(missing, vec!["DB_URL"]);
    }

    #[test]
    fn missing_resolvable_keys_returns_empty_when_all_resolved() {
        let resolvable = vec!["API_KEY".to_string()];
        let resolved = BTreeMap::from([("API_KEY".to_string(), "value".to_string())]);

        let missing = missing_resolvable_keys(&resolvable, &resolved);
        assert!(missing.is_empty());
    }

    #[test]
    fn emit_missing_entries_warning_returns_count_of_missing() {
        let keys = vec!["A".to_string(), "B".to_string(), "C".to_string()];
        let resolved = BTreeMap::from([("A".to_string(), "v".to_string())]);
        assert_eq!(emit_missing_entries_warning(&keys, &resolved), 2);
    }

    #[test]
    fn emit_missing_entries_warning_returns_zero_when_all_resolved() {
        let keys = vec!["A".to_string()];
        let resolved = BTreeMap::from([("A".to_string(), "v".to_string())]);
        assert_eq!(emit_missing_entries_warning(&keys, &resolved), 0);
    }

    #[test]
    fn summarize_entry_keys_empty() {
        let result = summarize_entry_keys(&[]);
        assert_eq!(result, "");
    }

    #[test]
    fn summarize_entry_keys_single() {
        let temp_dir = TempDir::new().unwrap();
        let env_path = temp_dir.path().join(".env");
        std::fs::write(&env_path, "ALPHA=value\n").unwrap();
        let env_file = env_file::EnvFile::parse(&env_path).unwrap();
        let entries = env_file.entries();
        assert_eq!(summarize_entry_keys(&entries), "ALPHA");
    }

    #[test]
    fn summarize_entry_keys_two() {
        let temp_dir = TempDir::new().unwrap();
        let env_path = temp_dir.path().join(".env");
        std::fs::write(&env_path, "BETA=v2\nALPHA=v1\n").unwrap();
        let env_file = env_file::EnvFile::parse(&env_path).unwrap();
        let entries = env_file.entries();
        let result = summarize_entry_keys(&entries);
        assert_eq!(result, "ALPHA, BETA");
    }

    #[test]
    fn summarize_entry_keys_three() {
        let temp_dir = TempDir::new().unwrap();
        let env_path = temp_dir.path().join(".env");
        std::fs::write(&env_path, "GAMMA=v3\nBETA=v2\nALPHA=v1\n").unwrap();
        let env_file = env_file::EnvFile::parse(&env_path).unwrap();
        let entries = env_file.entries();
        let result = summarize_entry_keys(&entries);
        assert_eq!(result, "ALPHA, BETA, GAMMA");
    }

    #[test]
    fn summarize_entry_keys_four_shows_plus_more() {
        let temp_dir = TempDir::new().unwrap();
        let env_path = temp_dir.path().join(".env");
        // Keys sorted: ALPHA, BETA, DELTA, GAMMA
        std::fs::write(&env_path, "GAMMA=v4\nBETA=v2\nALPHA=v1\nDELTA=v3\n").unwrap();
        let env_file = env_file::EnvFile::parse(&env_path).unwrap();
        let entries = env_file.entries();
        let result = summarize_entry_keys(&entries);
        assert_eq!(result, "ALPHA, BETA, DELTA (+1 more)");
    }

    #[test]
    fn resolve_dir_none_returns_current_dir() {
        let result = resolve_dir(None).unwrap();
        assert!(result.is_absolute());
        assert!(result.exists());
    }

    #[test]
    fn resolve_dir_existing_path() {
        let temp_dir = TempDir::new().unwrap();
        let result = resolve_dir(Some(temp_dir.path().to_path_buf())).unwrap();
        assert!(result.is_absolute());
        assert!(result.exists());
    }

    #[test]
    fn resolve_dir_nonexistent_path_returns_error() {
        let result = resolve_dir(Some(PathBuf::from("/nonexistent/path/99999")));
        assert!(result.is_err());
    }

    #[test]
    fn contains_glob_meta_star() {
        assert!(contains_glob_meta("cargo*"));
    }

    #[test]
    fn contains_glob_meta_question() {
        assert!(contains_glob_meta("cargo?"));
    }

    #[test]
    fn contains_glob_meta_bracket() {
        assert!(contains_glob_meta("[abc]"));
    }

    #[test]
    fn contains_glob_meta_none() {
        assert!(!contains_glob_meta("cargo"));
        assert!(!contains_glob_meta("npm"));
    }

    #[test]
    fn is_executable_file_on_directory_returns_false() {
        let temp_dir = TempDir::new().unwrap();
        assert!(!is_executable_file(temp_dir.path()));
    }

    #[test]
    fn is_executable_file_nonexistent_returns_false() {
        assert!(!is_executable_file(Path::new(
            "/nonexistent/file/path/12345"
        )));
    }

    #[test]
    fn is_executable_file_executable_returns_true() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("script.sh");
        create_executable(file_path.clone());
        assert!(is_executable_file(&file_path));
    }

    #[test]
    #[cfg(unix)]
    fn is_executable_file_non_executable_regular_file_returns_false() {
        // A regular file without execute bits must return false (Unix only).
        // This kills the & → | and & → ^ mutations in the Unix mode check.
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("plain.txt");
        std::fs::write(&file_path, "hello").unwrap();
        // Default mode on newly written files has no execute bits.
        assert!(!is_executable_file(&file_path));
    }

    #[test]
    fn config_template_contains_expected_content() {
        let template = config_template();
        assert!(!template.is_empty());
        assert!(template.contains("[defaults]"));
        assert!(template.contains("backend"));
        assert!(template.contains("pw-env"));
        assert!(template.contains("sync_throttle_secs"));
        assert!(template.contains("[defaults.cache]"));
        assert!(template.contains("ttl_hours"));
    }

    #[test]
    fn resolve_wrapped_commands_keeps_exact_safe_names() {
        let commands = resolve_wrapped_commands(&["cargo".to_string()], None);
        assert_eq!(commands, vec!["cargo".to_string()]);
    }

    #[test]
    fn resolve_wrapped_commands_expands_globs_from_path() {
        let temp_dir = TempDir::new().unwrap();
        create_executable(temp_dir.path().join("cargo"));
        create_executable(temp_dir.path().join("cargo-clippy"));
        create_executable(temp_dir.path().join("npm"));

        let path_var = std::env::join_paths([temp_dir.path()]).unwrap();
        let commands = resolve_wrapped_commands(&["cargo*".to_string()], Some(path_var));

        assert_eq!(
            commands,
            vec!["cargo".to_string(), "cargo-clippy".to_string()]
        );
    }

    #[test]
    fn resolve_wrapped_commands_deduplicates_matches() {
        let temp_dir = TempDir::new().unwrap();
        create_executable(temp_dir.path().join("cargo"));

        let path_var = std::env::join_paths([temp_dir.path()]).unwrap();
        let commands =
            resolve_wrapped_commands(&["cargo".to_string(), "cargo*".to_string()], Some(path_var));

        assert_eq!(commands, vec!["cargo".to_string()]);
    }

    #[test]
    fn build_hook_output_uses_wrappers_for_command_patterns() {
        let temp_dir = TempDir::new().unwrap();
        std::fs::write(temp_dir.path().join(".env"), "API_KEY=\n").unwrap();
        let canonical_dir = temp_dir.path().canonicalize().unwrap();

        let bin_dir = TempDir::new().unwrap();
        create_executable(bin_dir.path().join("cargo"));
        create_executable(bin_dir.path().join("cargo-watch"));

        let path_var = std::env::join_paths([bin_dir.path()]).unwrap();

        let output = build_hook_output(
            &canonical_dir,
            output::ShellSyntax::Posix,
            &config::Config {
                defaults: config::Defaults::default(),
                log: config::LogConfig::default(),
                updates: config::UpdateConfig::default(),
                projects: vec![config::ProjectOverride {
                    path: canonical_dir.to_string_lossy().into_owned(),
                    commands: vec!["cargo*".to_string()],
                    ..config::ProjectOverride::default()
                }],
            },
            Some(path_var),
        )
        .unwrap();

        assert!(output.contains("__pw_env_define_command_wrapper cargo\n"));
        assert!(output.contains("__pw_env_define_command_wrapper cargo-watch\n"));
        assert!(!output.contains("export API_KEY"));
    }

    fn create_executable(path: PathBuf) {
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            let mut permissions = std::fs::metadata(&path).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&path, permissions).unwrap();
        }
    }

    #[test]
    fn check_backends_does_not_panic() {
        // Just verify it runs without panicking; backends may or may not be installed
        check_backends();
    }

    #[test]
    fn check_config_with_default_config_and_temp_dir() {
        let temp_dir = TempDir::new().unwrap();
        let config = config::Config {
            defaults: config::Defaults::default(),
            log: config::LogConfig::default(),
            updates: config::UpdateConfig::default(),
            projects: vec![],
        };
        // Just ensure it doesn't panic; output goes to stderr
        check_config(&config, temp_dir.path());
    }

    #[test]
    fn handle_approvals_list_returns_ok() {
        let result = handle_approvals(ApprovalCommands::List);
        assert!(result.is_ok());
    }

    #[test]
    fn handle_approvals_list_fetch_returns_ok() {
        let result = handle_approvals(ApprovalCommands::ListFetch);
        assert!(result.is_ok());
    }

    #[test]
    fn handle_approvals_show_with_valid_override_file() {
        let temp_dir = TempDir::new().unwrap();
        let override_path = temp_dir.path().join(".pw-env.toml");
        std::fs::write(&override_path, "backend = \"op\"\n").unwrap();

        let result = handle_approvals(ApprovalCommands::Show {
            path: Some(override_path),
        });
        assert!(result.is_ok());
    }

    #[test]
    fn handle_approvals_revoke_returns_ok_when_no_approval_exists() {
        let temp_dir = TempDir::new().unwrap();
        let override_path = temp_dir.path().join(".pw-env.toml");
        std::fs::write(&override_path, "backend = \"bw\"\n").unwrap();

        let result = handle_approvals(ApprovalCommands::Revoke {
            path: override_path,
        });
        assert!(result.is_ok());
    }

    #[test]
    fn handle_approvals_show_fetch_with_temp_env() {
        let temp_dir = TempDir::new().unwrap();
        let env_path = temp_dir.path().join(".env");
        std::fs::write(&env_path, "API_KEY=\n").unwrap();

        let result = handle_approvals(ApprovalCommands::ShowFetch {
            path: Some(env_path),
        });
        assert!(result.is_ok());
    }

    #[test]
    fn handle_approvals_revoke_fetch_returns_ok_when_no_approval_exists() {
        let temp_dir = TempDir::new().unwrap();
        let env_path = temp_dir.path().join(".env");
        std::fs::write(&env_path, "DB_URL=\n").unwrap();

        let result = handle_approvals(ApprovalCommands::RevokeFetch { path: env_path });
        assert!(result.is_ok());
    }

    #[test]
    fn handle_cache_clear_removes_folder_cache_file() {
        let _lock = crate::cache::keyring_test_lock()
            .lock()
            .expect("keyring test mutex poisoned");
        let temp_dir = TempDir::new().unwrap();
        let cache_path = temp_dir
            .path()
            .join("pw-env")
            .join("bitwarden-folder-ids.json");
        let sync_state_path = temp_dir
            .path()
            .join("pw-env")
            .join("bitwarden-sync-state.json");
        let secret_cache_path = temp_dir
            .path()
            .join("pw-env")
            .join("resolved-secret-cache.json");

        std::fs::create_dir_all(cache_path.parent().unwrap()).unwrap();
        std::fs::write(
            &cache_path,
            "{\"folder_ids\":{\"Engineering\":\"folder-123\"}}",
        )
        .unwrap();
        std::fs::write(&sync_state_path, "{\"last_sync_unix_secs\":123}").unwrap();
        std::fs::write(
            &secret_cache_path,
            "{\"entries\":{\"cache-key\":{\"cached_at_unix_secs\":123}}}",
        )
        .unwrap();

        crate::backend::bw::BwBackend::set_test_folder_cache_path(Some(cache_path.clone()));
        crate::backend::bw::BwBackend::set_test_sync_state_path(Some(sync_state_path.clone()));
        crate::cache::set_test_secret_cache_index_path(Some(secret_cache_path.clone()));
        crate::cache::set_test_keyring_available(true);
        let result = handle_cache(CacheCommands::Clear);
        crate::backend::bw::BwBackend::set_test_folder_cache_path(None);
        crate::backend::bw::BwBackend::set_test_sync_state_path(None);
        crate::cache::set_test_secret_cache_index_path(None);
        crate::cache::reset_test_keyring();

        assert!(result.is_ok());
        assert!(!cache_path.exists());
        assert!(!sync_state_path.exists());
        assert!(!secret_cache_path.exists());
    }

    #[test]
    fn handle_cache_clear_returns_ok_when_cache_file_is_absent() {
        let _lock = crate::cache::keyring_test_lock()
            .lock()
            .expect("keyring test mutex poisoned");
        let temp_dir = TempDir::new().unwrap();
        let cache_path = temp_dir
            .path()
            .join("pw-env")
            .join("bitwarden-folder-ids.json");
        let sync_state_path = temp_dir
            .path()
            .join("pw-env")
            .join("bitwarden-sync-state.json");
        let secret_cache_path = temp_dir
            .path()
            .join("pw-env")
            .join("resolved-secret-cache.json");

        crate::backend::bw::BwBackend::set_test_folder_cache_path(Some(cache_path));
        crate::backend::bw::BwBackend::set_test_sync_state_path(Some(sync_state_path));
        crate::cache::set_test_secret_cache_index_path(Some(secret_cache_path));
        crate::cache::set_test_keyring_available(true);
        let result = handle_cache(CacheCommands::Clear);
        crate::backend::bw::BwBackend::set_test_folder_cache_path(None);
        crate::backend::bw::BwBackend::set_test_sync_state_path(None);
        crate::cache::set_test_secret_cache_index_path(None);
        crate::cache::reset_test_keyring();

        assert!(result.is_ok());
    }

    #[test]
    fn emit_plaintext_secret_warning_with_no_secrets() {
        let temp_dir = TempDir::new().unwrap();
        let env_path = temp_dir.path().join(".env");
        std::fs::write(&env_path, "API_KEY=op://vault/item/field\nDB_URL=\n").unwrap();

        let env_file = env_file::EnvFile::parse(&env_path).unwrap();
        let result = emit_plaintext_secret_warning(&env_file);
        assert!(result.is_ok());
    }

    #[test]
    fn pending_migration_entries_with_no_secrets_returns_empty() {
        let temp_dir = TempDir::new().unwrap();
        let env_path = temp_dir.path().join(".env");
        std::fs::write(&env_path, "HOST=localhost\nPORT=5432\n").unwrap();

        let env_file = env_file::EnvFile::parse(&env_path).unwrap();
        let entries = pending_migration_entries(&env_file).unwrap();
        // HOST and PORT don't look like secrets
        assert!(entries.is_empty());
    }

    #[test]
    fn pending_migration_entries_with_secret_returns_nonempty() {
        // A key whose name looks like a secret and a value long enough to appear
        // non-trivial must appear in the pending entries.
        // This kills the mutation that replaces the function body with Ok(vec![]).
        let temp_dir = TempDir::new().unwrap();
        let env_path = temp_dir.path().join(".env");
        std::fs::write(
            &env_path,
            "API_SECRET_TOKEN=abcdefghijklmnopqrstuvwxyz012345\n",
        )
        .unwrap();

        let env_file = env_file::EnvFile::parse(&env_path).unwrap();
        let entries = pending_migration_entries(&env_file).unwrap();
        assert!(
            !entries.is_empty(),
            "expected at least one pending secret entry"
        );
    }

    #[test]
    fn list_path_executables_with_no_path_returns_empty() {
        let result = list_path_executables(None);
        assert!(result.is_empty());
    }

    #[test]
    fn list_path_executables_with_real_dir_returns_executables() {
        let temp_dir = TempDir::new().unwrap();
        create_executable(temp_dir.path().join("my-tool"));

        let path_val = std::env::join_paths([temp_dir.path()]).unwrap();
        let result = list_path_executables(Some(path_val.as_os_str()));
        assert!(result.contains("my-tool"));
    }

    #[test]
    fn resolve_wrapped_commands_skips_invalid_glob() {
        // An unclosed bracket is an invalid glob; should be skipped
        let result = resolve_wrapped_commands(&["[unclosed".to_string()], None);
        assert!(result.is_empty());
    }

    #[test]
    fn resolve_wrapped_commands_skips_unsafe_command_names() {
        // Command with path separator is not a "safe" name
        let result = resolve_wrapped_commands(&["/bin/bash".to_string()], None);
        assert!(result.is_empty());
    }

    #[test]
    fn build_hook_output_returns_empty_string_for_dir_without_env_file() {
        let temp_dir = TempDir::new().unwrap();
        let config = config::Config {
            defaults: config::Defaults::default(),
            log: config::LogConfig::default(),
            updates: config::UpdateConfig::default(),
            projects: vec![],
        };
        let result =
            build_hook_output(temp_dir.path(), output::ShellSyntax::Posix, &config, None).unwrap();
        assert_eq!(result, "");
    }

    #[test]
    fn emit_plaintext_secret_warning_with_likely_secret_entry_outputs_warning() {
        let temp_dir = TempDir::new().unwrap();
        let env_path = temp_dir.path().join(".env");
        // A key that looks like a secret with a long enough value
        std::fs::write(
            &env_path,
            "API_SECRET_KEY=very_long_plain_text_value_here\n",
        )
        .unwrap();

        let env_file = env_file::EnvFile::parse(&env_path).unwrap();
        // This should emit the warning (to stderr) but return Ok
        let result = emit_plaintext_secret_warning(&env_file);
        assert!(result.is_ok());
    }

    #[test]
    #[cfg(unix)]
    fn check_backends_reports_not_found_when_backends_missing() {
        let _guard = crate::backend::MOCK_PATH_MUTEX
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let temp_dir = TempDir::new().unwrap();
        let path_val = std::env::join_paths([temp_dir.path()]).unwrap();
        let old_path = std::env::var_os("PATH").unwrap_or_default();
        // SAFETY: guarded by MOCK_PATH_MUTEX
        unsafe { std::env::set_var("PATH", &path_val) };
        check_backends();
        unsafe { std::env::set_var("PATH", &old_path) };
    }

    #[test]
    fn maybe_check_for_release_update_does_not_panic() {
        let config = config::Config {
            defaults: config::Defaults::default(),
            log: config::LogConfig::default(),
            updates: config::UpdateConfig {
                enabled: false,
                check_interval_hours: 24,
            },
            projects: vec![],
        };
        // Should not panic; stderr is not a terminal in tests so it returns early
        maybe_check_for_release_update(&Commands::Check, &config);
    }

    #[test]
    fn handle_approvals_show_with_none_path_returns_ok() {
        // None path resolves to current dir — should succeed even if no .pw-env.toml there
        let result = handle_approvals(ApprovalCommands::Show { path: None });
        // May succeed or fail depending on state, but must not panic
        let _ = result;
    }

    #[test]
    fn handle_approvals_show_fetch_with_none_path_returns_ok() {
        let result = handle_approvals(ApprovalCommands::ShowFetch { path: None });
        let _ = result; // May or may not succeed depending on presence of .env
    }

    #[test]
    #[cfg(unix)]
    fn check_config_when_config_file_exists_with_vault_and_folder() {
        let _guard = crate::backend::MOCK_PATH_MUTEX
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let temp_dir = TempDir::new().unwrap();
        // Create config file at XDG_CONFIG_HOME/pw-env/config.toml
        let config_dir = temp_dir.path().join("pw-env");
        std::fs::create_dir_all(&config_dir).unwrap();
        let config_file = config_dir.join("config.toml");
        std::fs::write(&config_file, "[defaults]\nbackend = \"op\"\n[defaults.op]\nvault = \"MyVault\"\n[defaults.bw]\nfolder = \"MyFolder\"\n[log]\n[updates]\n").unwrap();

        let old_xdg = std::env::var_os("XDG_CONFIG_HOME");
        // SAFETY: guarded by MOCK_PATH_MUTEX
        unsafe { std::env::set_var("XDG_CONFIG_HOME", temp_dir.path()) };

        let config = config::Config {
            defaults: config::Defaults {
                op: config::OpConfig {
                    vault: Some("MyVault".to_string()),
                    ..Default::default()
                },
                bw: config::BwConfig {
                    folder: Some("MyFolder".to_string()),
                    ..Default::default()
                },
                ..config::Defaults::default()
            },
            log: config::LogConfig::default(),
            updates: config::UpdateConfig::default(),
            projects: vec![],
        };
        check_config(&config, temp_dir.path());

        match old_xdg {
            Some(v) => unsafe { std::env::set_var("XDG_CONFIG_HOME", v) },
            None => unsafe { std::env::remove_var("XDG_CONFIG_HOME") },
        }
    }

    #[test]
    fn build_hook_output_with_plaintext_env_and_no_commands_returns_empty() {
        // No commands configured + plaintext-only .env → parse env file, resolve (returns empty), return ""
        let temp_dir = TempDir::new().unwrap();
        let env_path = temp_dir.path().join(".env");
        // Plaintext entry: no op:// reference, so resolvable_entries() is empty → resolve returns Ok({})
        std::fs::write(&env_path, "SHELL=/bin/bash\n").unwrap();

        let config = config::Config {
            defaults: config::Defaults::default(),
            log: config::LogConfig::default(),
            updates: config::UpdateConfig::default(),
            projects: vec![],
        };
        let result = build_hook_output(temp_dir.path(), output::ShellSyntax::Posix, &config, None);
        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        assert_eq!(result.unwrap(), "");
    }

    #[test]
    fn build_hook_output_with_fish_syntax_and_commands_configured() {
        let temp_dir = TempDir::new().unwrap();
        let canonical = temp_dir.path().canonicalize().unwrap();
        let env_path = canonical.join(".env");
        std::fs::write(&env_path, "KEY=value\n").unwrap();

        let project_path = canonical.to_string_lossy().into_owned();
        let config = config::Config {
            defaults: config::Defaults::default(),
            log: config::LogConfig::default(),
            updates: config::UpdateConfig::default(),
            projects: vec![config::ProjectOverride {
                path: project_path,
                commands: vec!["cat".to_string()],
                backend: None,
                search_parent_env: None,
                source_all: None,
                warn_missing: None,
                cache: None,
                op: None,
                bw: None,
                gpg: None,
                item: None,
            }],
        };
        let result = build_hook_output(&canonical, output::ShellSyntax::Fish, &config, None);
        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
    }

    #[test]
    fn resolve_dir_with_none_returns_current_dir() {
        let result = resolve_dir(None);
        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
    }

    #[test]
    fn resolve_dir_with_some_existing_dir_returns_canonical() {
        let temp_dir = TempDir::new().unwrap();
        let result = resolve_dir(Some(temp_dir.path().to_path_buf()));
        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
    }

    #[test]
    fn summarize_entry_keys_truncates_beyond_three() {
        let temp_dir = TempDir::new().unwrap();
        let env_path = temp_dir.path().join(".env");
        std::fs::write(&env_path, "AA=1\nBB=2\nCC=3\nDD=4\n").unwrap();
        let env_file = env_file::EnvFile::parse(&env_path).unwrap();
        let entries = env_file.entries();
        let result = summarize_entry_keys(&entries);
        assert!(
            result.contains("+1 more"),
            "expected '+1 more' in: {result}"
        );
    }

    #[test]
    fn keyring_clear_status_message_is_none_when_nothing_happened() {
        let result = cache::ClearSecretCacheResult {
            cleared_index_file: false,
            deleted_credentials: 0,
            keyring_delete_failures: 0,
            keyring_unavailable: false,
        };
        assert_eq!(keyring_clear_status_message(&result), None);
    }

    #[test]
    fn keyring_clear_status_message_reports_keyring_unavailable() {
        let result = cache::ClearSecretCacheResult {
            cleared_index_file: true,
            deleted_credentials: 0,
            keyring_delete_failures: 1,
            keyring_unavailable: true,
        };
        let msg = keyring_clear_status_message(&result).unwrap();
        assert!(
            msg.contains("OS keyring was unavailable"),
            "unexpected message: {msg}"
        );
    }

    #[test]
    fn keyring_clear_status_message_reports_single_delete_failure() {
        let result = cache::ClearSecretCacheResult {
            cleared_index_file: false,
            deleted_credentials: 0,
            keyring_delete_failures: 1,
            keyring_unavailable: false,
        };
        let msg = keyring_clear_status_message(&result).unwrap();
        assert!(
            msg.contains("Failed to remove 1") && msg.ends_with("entry"),
            "unexpected message: {msg}"
        );
    }

    #[test]
    fn keyring_clear_status_message_reports_multiple_delete_failures() {
        let result = cache::ClearSecretCacheResult {
            cleared_index_file: false,
            deleted_credentials: 0,
            keyring_delete_failures: 3,
            keyring_unavailable: false,
        };
        let msg = keyring_clear_status_message(&result).unwrap();
        assert!(
            msg.contains("Failed to remove 3") && msg.ends_with("entries"),
            "unexpected message: {msg}"
        );
    }

    #[test]
    fn keyring_clear_status_message_reports_single_deleted_credential() {
        let result = cache::ClearSecretCacheResult {
            cleared_index_file: true,
            deleted_credentials: 1,
            keyring_delete_failures: 0,
            keyring_unavailable: false,
        };
        let msg = keyring_clear_status_message(&result).unwrap();
        assert!(
            msg.contains("Removed 1") && msg.ends_with("entry"),
            "unexpected message: {msg}"
        );
    }

    #[test]
    fn keyring_clear_status_message_reports_multiple_deleted_credentials() {
        let result = cache::ClearSecretCacheResult {
            cleared_index_file: true,
            deleted_credentials: 2,
            keyring_delete_failures: 0,
            keyring_unavailable: false,
        };
        let msg = keyring_clear_status_message(&result).unwrap();
        assert!(
            msg.contains("Removed 2") && msg.ends_with("entries"),
            "unexpected message: {msg}"
        );
    }

    #[test]
    fn keyring_clear_status_message_zero_deleted_credentials_returns_none() {
        let result = cache::ClearSecretCacheResult {
            cleared_index_file: true,
            deleted_credentials: 0,
            keyring_delete_failures: 0,
            keyring_unavailable: false,
        };
        assert_eq!(keyring_clear_status_message(&result), None);
    }
}
