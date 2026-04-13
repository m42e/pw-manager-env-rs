use anyhow::{Context, Result};
use crossterm::cursor::{Hide, Show};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use serde::Serialize;
use std::io::{self, IsTerminal, Write};
use std::path::Path;

use crate::config::{self, Config, ProjectOverride};

#[cfg(test)]
const DEFAULT_BW_SYNC_THROTTLE_SECS: u64 = 3600;
#[cfg(test)]
const DEFAULT_GPG_FILE_PATTERN: &str = ".env.gpg";

pub fn run(initial_config: &Config) -> Result<()> {
    if !is_interactive() {
        anyhow::bail!("pw-env config-wizard requires an interactive terminal");
    }

    let app = WizardApp::new(initial_config);
    match run_tui(app)? {
        WizardOutcome::Cancelled => {
            eprintln!("Config wizard cancelled.");
            Ok(())
        }
        WizardOutcome::Save(config_text) => {
            let output_path = Config::config_path();
            save_config_to_path(&output_path, &config_text)?;
            eprintln!("Wrote config to {}", output_path.display());
            Ok(())
        }
    }
}

fn is_interactive() -> bool {
    is_interactive_check(
        cfg!(not(test)),
        io::stdin().is_terminal(),
        io::stdout().is_terminal(),
        io::stderr().is_terminal(),
    )
}

fn is_interactive_check(
    not_test: bool,
    stdin_terminal: bool,
    stdout_terminal: bool,
    stderr_terminal: bool,
) -> bool {
    not_test && stdin_terminal && stdout_terminal && stderr_terminal
}

fn run_tui(mut app: WizardApp) -> Result<WizardOutcome> {
    let _session = TerminalSession::enter()?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend).context("Failed to create terminal UI")?;

    loop {
        terminal
            .draw(|frame| render(frame, &app))
            .context("Failed to render config wizard")?;

        let Event::Key(key) = event::read().context("Failed to read terminal input")? else {
            continue;
        };

        if key.kind != KeyEventKind::Press {
            continue;
        }

        if let Some(outcome) = app.handle_key(key)? {
            return Ok(outcome);
        }
    }
}

fn render(frame: &mut Frame, app: &WizardApp) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(8), Constraint::Length(8)])
        .split(frame.area());

    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(areas[0]);

    let mut list_state = ListState::default();
    list_state.select(Some(app.selected));

    let question_items = ALL_FIELDS
        .iter()
        .map(|field| {
            let label_style = if field.is_backend_specific(app.state.backend) {
                Style::default().fg(Color::White)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!("{}: ", field.label()), label_style),
                value_span(field.value(&app.state)),
            ]))
        })
        .collect::<Vec<_>>();

    let questions = List::new(question_items)
        .block(Block::default().borders(Borders::ALL).title("Questions"))
        .highlight_style(
            Style::default()
                .bg(Color::Blue)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");
    frame.render_stateful_widget(questions, panes[0], &mut list_state);

    let config_preview = app.state.render_config();
    let preview = Paragraph::new(highlight_toml(&config_preview))
        .block(Block::default().borders(Borders::ALL).title("Built Config"))
        .wrap(Wrap { trim: false });
    frame.render_widget(preview, panes[1]);

    let selected_field = app.selected_field();
    let (mode_lines, controls_block) = match &app.mode {
        InputMode::Normal => (
            vec![
                Line::from(
                    "Arrows move. Space toggles booleans. Left/right cycles choices. Enter edits text.",
                ),
                Line::from("s saves. q quits."),
            ],
            Block::default().borders(Borders::ALL).title("Controls"),
        ),
        InputMode::Editing { buffer } => {
            let displayed = if buffer.is_empty() {
                "<empty>".to_string()
            } else {
                buffer.clone()
            };
            (
                vec![Line::from(vec![
                    Span::raw("Editing "),
                    Span::styled(
                        selected_field.label(),
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(": "),
                    Span::styled(displayed, Style::default().fg(Color::Yellow)),
                ])],
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Yellow))
                    .title(Span::styled(
                        "Controls",
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    )),
            )
        }
    };
    let status_style = if app.status.starts_with("Updated") {
        Style::default().fg(Color::Green)
    } else if app.status.starts_with("Cancelled") {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default().fg(Color::Cyan)
    };
    let help = Paragraph::new(Text::from(vec![
        Line::from(Span::raw(selected_field.help())),
        Line::from(Span::styled(app.status.clone(), status_style)),
    ]))
    .block(Block::default().borders(Borders::ALL).title("Help"))
    .wrap(Wrap { trim: false });
    let mode = Paragraph::new(Text::from(mode_lines))
        .block(controls_block)
        .wrap(Wrap { trim: false });

    let bottom = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(4), Constraint::Length(4)])
        .split(areas[1]);
    frame.render_widget(help, bottom[0]);
    frame.render_widget(mode, bottom[1]);
}

fn save_config_to_path(path: &Path, config_text: &str) -> Result<()> {
    let _: Config = toml::from_str(config_text).context("Generated config is invalid")?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }

    config::write_private_file(path, config_text)
        .with_context(|| format!("Failed to write {}", path.display()))
}

struct TerminalSession<W: Write = io::Stdout> {
    writer: W,
}

impl TerminalSession<io::Stdout> {
    fn enter() -> Result<Self> {
        enable_raw_mode().context("Failed to enable raw mode")?;
        let mut writer = io::stdout();
        execute!(writer, EnterAlternateScreen, Hide).context("Failed to enter alternate screen")?;
        Ok(Self { writer })
    }
}

impl<W: Write> TerminalSession<W> {
    #[cfg(test)]
    fn new_for_test(writer: W) -> Self {
        Self { writer }
    }
}

impl<W: Write> Drop for TerminalSession<W> {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(self.writer.by_ref(), Show, LeaveAlternateScreen);
    }
}

#[derive(Clone)]
struct ConfigWizardState {
    backend: BackendChoice,
    search_parent_env: bool,
    source_all: bool,
    warn_missing: bool,
    fallback_example_env: bool,
    cache_enabled: bool,
    cache_ttl_hours: u64,
    op_vault: Option<String>,
    op_account: Option<String>,
    op_item: Option<String>,
    bw_folder: Option<String>,
    bw_organization: Option<String>,
    bw_item: Option<String>,
    bw_sync_throttle_secs: u64,
    gpg_file_pattern: String,
    gpg_recipient: Option<String>,
    log_level: LogLevelChoice,
    log_file: Option<String>,
    updates_enabled: bool,
    updates_check_interval_hours: u64,
    projects: Vec<ProjectOverride>,
}

impl ConfigWizardState {
    fn from_config(config: &Config) -> Self {
        Self {
            backend: BackendChoice::from_value(&config.defaults.backend),
            search_parent_env: config.defaults.search_parent_env,
            source_all: config.defaults.source_all,
            warn_missing: config.defaults.warn_missing,
            fallback_example_env: config.defaults.fallback_example_env,
            cache_enabled: config.defaults.cache.enabled,
            cache_ttl_hours: config.defaults.cache.ttl_hours,
            op_vault: config.defaults.op.vault.clone(),
            op_account: config.defaults.op.account.clone(),
            op_item: config.defaults.op.item.clone(),
            bw_folder: config.defaults.bw.folder.clone(),
            bw_organization: config.defaults.bw.organization.clone(),
            bw_item: config.defaults.bw.item.clone(),
            bw_sync_throttle_secs: config.defaults.bw.sync_throttle_secs,
            gpg_file_pattern: config.defaults.gpg.file_pattern.clone(),
            gpg_recipient: config.defaults.gpg.recipient.clone(),
            log_level: LogLevelChoice::from_value(&config.log.level),
            log_file: config.log.file.clone(),
            updates_enabled: config.updates.enabled,
            updates_check_interval_hours: config.updates.check_interval_hours,
            projects: config.projects.clone(),
        }
    }

    #[cfg(test)]
    fn to_config(&self) -> Config {
        Config {
            defaults: crate::config::Defaults {
                backend: self.backend.as_str().to_string(),
                search_parent_env: self.search_parent_env,
                source_all: self.source_all,
                warn_missing: self.warn_missing,
                fallback_example_env: self.fallback_example_env,
                cache: crate::config::CacheConfig {
                    enabled: self.cache_enabled,
                    ttl_hours: self.cache_ttl_hours,
                },
                op: crate::config::OpConfig {
                    vault: self.op_vault.clone(),
                    account: self.op_account.clone(),
                    item: self.op_item.clone(),
                },
                bw: crate::config::BwConfig {
                    folder: self.bw_folder.clone(),
                    organization: self.bw_organization.clone(),
                    item: self.bw_item.clone(),
                    sync_throttle_secs: self.bw_sync_throttle_secs,
                },
                gpg: crate::config::GpgConfig {
                    file_pattern: self.gpg_file_pattern.clone(),
                    recipient: self.gpg_recipient.clone(),
                },
            },
            log: crate::config::LogConfig {
                level: self.log_level.as_str().to_string(),
                file: self.log_file.clone(),
            },
            updates: crate::config::UpdateConfig {
                enabled: self.updates_enabled,
                check_interval_hours: self.updates_check_interval_hours,
            },
            projects: self.projects.clone(),
        }
    }

    fn render_config(&self) -> String {
        let mut lines = vec![
            "# Generated by pw-env config-wizard".to_string(),
            format!("# Path: {}", Config::config_path().display()),
            String::new(),
            "[defaults]".to_string(),
            format!("backend = {}", quoted(self.backend.as_str())),
            format!("search_parent_env = {}", self.search_parent_env),
            format!("source_all = {}", self.source_all),
            format!("warn_missing = {}", self.warn_missing),
            format!("fallback_example_env = {}", self.fallback_example_env),
            String::new(),
            "[defaults.cache]".to_string(),
            format!("enabled = {}", self.cache_enabled),
            format!("ttl_hours = {}", self.cache_ttl_hours),
        ];

        if self.op_vault.is_some() || self.op_account.is_some() || self.op_item.is_some() {
            lines.push(String::new());
            lines.push("[defaults.op]".to_string());
            if let Some(vault) = &self.op_vault {
                lines.push(format!("vault = {}", quoted(vault)));
            }
            if let Some(account) = &self.op_account {
                lines.push(format!("account = {}", quoted(account)));
            }
            if let Some(item) = &self.op_item {
                lines.push(format!("item = {}", quoted(item)));
            }
        }

        lines.push(String::new());
        lines.push("[defaults.bw]".to_string());
        if let Some(folder) = &self.bw_folder {
            lines.push(format!("folder = {}", quoted(folder)));
        }
        if let Some(organization) = &self.bw_organization {
            lines.push(format!("organization = {}", quoted(organization)));
        }
        if let Some(item) = &self.bw_item {
            lines.push(format!("item = {}", quoted(item)));
        }
        lines.push(format!(
            "sync_throttle_secs = {}",
            self.bw_sync_throttle_secs
        ));

        lines.push(String::new());
        lines.push("[defaults.gpg]".to_string());
        lines.push(format!("file_pattern = {}", quoted(&self.gpg_file_pattern)));
        if let Some(recipient) = &self.gpg_recipient {
            lines.push(format!("recipient = {}", quoted(recipient)));
        }

        lines.push(String::new());
        lines.push("[log]".to_string());
        lines.push(format!("level = {}", quoted(self.log_level.as_str())));
        if let Some(file) = &self.log_file {
            lines.push(format!("file = {}", quoted(file)));
        }

        lines.push(String::new());
        lines.push("[updates]".to_string());
        lines.push(format!("enabled = {}", self.updates_enabled));
        lines.push(format!(
            "check_interval_hours = {}",
            self.updates_check_interval_hours
        ));

        if !self.projects.is_empty() {
            lines.push(String::new());
            lines.push("# Existing project overrides are preserved below.".to_string());
            let project_section = toml::to_string_pretty(&ProjectsDocument {
                projects: &self.projects,
            })
            .unwrap_or_default();
            lines.push(project_section.trim_end().to_string());
        }

        format!("{}\n", lines.join("\n"))
    }
}

#[derive(Serialize)]
struct ProjectsDocument<'a> {
    projects: &'a [ProjectOverride],
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BackendChoice {
    Op,
    Bw,
    Gpg,
}

impl BackendChoice {
    fn from_value(value: &str) -> Self {
        match value {
            "bw" => Self::Bw,
            "gpg" => Self::Gpg,
            _ => Self::Op,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Op => "op",
            Self::Bw => "bw",
            Self::Gpg => "gpg",
        }
    }

    fn cycle(self, direction: i8) -> Self {
        match (self, direction.is_negative()) {
            (Self::Op, true) => Self::Gpg,
            (Self::Op, false) => Self::Bw,
            (Self::Bw, true) => Self::Op,
            (Self::Bw, false) => Self::Gpg,
            (Self::Gpg, true) => Self::Bw,
            (Self::Gpg, false) => Self::Op,
        }
    }
}

#[derive(Clone, Copy)]
enum LogLevelChoice {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevelChoice {
    fn from_value(value: &str) -> Self {
        match value {
            "trace" => Self::Trace,
            "debug" => Self::Debug,
            "warn" => Self::Warn,
            "error" => Self::Error,
            _ => Self::Info,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Trace => "trace",
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }

    fn cycle(self, direction: i8) -> Self {
        match (self, direction.is_negative()) {
            (Self::Trace, true) => Self::Error,
            (Self::Trace, false) => Self::Debug,
            (Self::Debug, true) => Self::Trace,
            (Self::Debug, false) => Self::Info,
            (Self::Info, true) => Self::Debug,
            (Self::Info, false) => Self::Warn,
            (Self::Warn, true) => Self::Info,
            (Self::Warn, false) => Self::Error,
            (Self::Error, true) => Self::Warn,
            (Self::Error, false) => Self::Trace,
        }
    }
}

struct WizardApp {
    state: ConfigWizardState,
    selected: usize,
    mode: InputMode,
    status: String,
}

impl WizardApp {
    fn new(config: &Config) -> Self {
        let project_note = if config.projects.is_empty() {
            "Press s to save to ~/.config/pw-env/config.toml.".to_string()
        } else {
            format!(
                "{} existing [[projects]] entr{} will be preserved when you save.",
                config.projects.len(),
                if config.projects.len() == 1 {
                    "y"
                } else {
                    "ies"
                }
            )
        };
        Self {
            state: ConfigWizardState::from_config(config),
            selected: 0,
            mode: InputMode::Normal,
            status: project_note,
        }
    }

    fn selected_field(&self) -> FieldId {
        ALL_FIELDS[self.selected]
    }

    fn handle_key(&mut self, key: KeyEvent) -> Result<Option<WizardOutcome>> {
        if matches!(self.mode, InputMode::Normal) {
            return self.handle_normal_key(key);
        }

        let InputMode::Editing { mut buffer } =
            std::mem::replace(&mut self.mode, InputMode::Normal)
        else {
            unreachable!("editing mode expected")
        };

        let keep_editing = self.handle_editing_key(key, &mut buffer)?;
        if keep_editing {
            self.mode = InputMode::Editing { buffer };
        }

        Ok(None)
    }

    fn handle_normal_key(&mut self, key: KeyEvent) -> Result<Option<WizardOutcome>> {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(Some(WizardOutcome::Cancelled)),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return Ok(Some(WizardOutcome::Cancelled));
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if self.selected > 0 {
                    self.selected -= 1;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.selected + 1 < ALL_FIELDS.len() {
                    self.selected += 1;
                }
            }
            KeyCode::Left | KeyCode::Char('h') => {
                self.selected_field().adjust(&mut self.state, -1);
            }
            KeyCode::Right | KeyCode::Char('l') => {
                self.selected_field().adjust(&mut self.state, 1);
            }
            KeyCode::Enter => {
                if self.selected_field().starts_editing() {
                    self.mode = InputMode::Editing {
                        buffer: self.selected_field().edit_buffer(&self.state),
                    };
                    self.status = format!(
                        "Press Enter to apply {} or Esc to cancel.",
                        self.selected_field().label()
                    );
                } else {
                    self.selected_field().adjust(&mut self.state, 1);
                }
            }
            KeyCode::Char(' ') => {
                self.selected_field().adjust(&mut self.state, 1);
            }
            KeyCode::Char('s') => {
                return Ok(Some(WizardOutcome::Save(self.state.render_config())));
            }
            _ => {}
        }

        Ok(None)
    }

    fn handle_editing_key(&mut self, key: KeyEvent, buffer: &mut String) -> Result<bool> {
        match key.code {
            KeyCode::Esc => {
                self.mode = InputMode::Normal;
                self.status = format!("Cancelled edit for {}.", self.selected_field().label());
                return Ok(false);
            }
            KeyCode::Enter => {
                self.selected_field().apply_edit(&mut self.state, buffer)?;
                self.mode = InputMode::Normal;
                self.status = format!("Updated {}.", self.selected_field().label());
                return Ok(false);
            }
            KeyCode::Backspace => {
                buffer.pop();
            }
            KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                buffer.push(ch);
            }
            _ => {}
        }

        Ok(true)
    }
}

enum InputMode {
    Normal,
    Editing { buffer: String },
}

enum WizardOutcome {
    Cancelled,
    Save(String),
}

#[derive(Clone, Copy)]
enum FieldId {
    Backend,
    SearchParentEnv,
    SourceAll,
    WarnMissing,
    FallbackExampleEnv,
    CacheEnabled,
    CacheTtlHours,
    OpVault,
    OpAccount,
    OpItem,
    BwFolder,
    BwOrganization,
    BwItem,
    BwSyncThrottleSecs,
    GpgFilePattern,
    GpgRecipient,
    LogLevel,
    LogFile,
    UpdatesEnabled,
    UpdateCheckIntervalHours,
}

const ALL_FIELDS: [FieldId; 20] = [
    FieldId::Backend,
    FieldId::SearchParentEnv,
    FieldId::SourceAll,
    FieldId::WarnMissing,
    FieldId::FallbackExampleEnv,
    FieldId::CacheEnabled,
    FieldId::CacheTtlHours,
    FieldId::OpVault,
    FieldId::OpAccount,
    FieldId::OpItem,
    FieldId::BwFolder,
    FieldId::BwOrganization,
    FieldId::BwItem,
    FieldId::BwSyncThrottleSecs,
    FieldId::GpgFilePattern,
    FieldId::GpgRecipient,
    FieldId::LogLevel,
    FieldId::LogFile,
    FieldId::UpdatesEnabled,
    FieldId::UpdateCheckIntervalHours,
];

impl FieldId {
    fn label(self) -> &'static str {
        match self {
            Self::Backend => "Default backend",
            Self::SearchParentEnv => "Search parent .env files",
            Self::SourceAll => "Export plaintext values too",
            Self::WarnMissing => "Warn on unresolved entries",
            Self::FallbackExampleEnv => "Fallback to .env.example",
            Self::CacheEnabled => "Enable keyring cache",
            Self::CacheTtlHours => "Cache TTL hours",
            Self::OpVault => "1Password vault",
            Self::OpAccount => "1Password account",
            Self::OpItem => "1Password item",
            Self::BwFolder => "Bitwarden folder",
            Self::BwOrganization => "Bitwarden organization",
            Self::BwItem => "Bitwarden item",
            Self::BwSyncThrottleSecs => "Bitwarden sync throttle",
            Self::GpgFilePattern => "GPG file pattern",
            Self::GpgRecipient => "GPG recipient",
            Self::LogLevel => "Log level",
            Self::LogFile => "Log file path",
            Self::UpdatesEnabled => "Automatic update checks",
            Self::UpdateCheckIntervalHours => "Update check interval",
        }
    }

    fn help(self) -> &'static str {
        match self {
            Self::Backend => "Which backend should pw-env use by default for empty .env values?",
            Self::SearchParentEnv => {
                "Should pw-env search parent directories up to the git root for a .env file?"
            }
            Self::SourceAll => {
                "Should pw-env export plaintext .env values alongside resolved secrets?"
            }
            Self::WarnMissing => {
                "Should pw-env print warnings for entries that could not be resolved?"
            }
            Self::FallbackExampleEnv => {
                "Should pw-env fall back to .env.example when no .env file exists?"
            }
            Self::CacheEnabled => {
                "Should resolved secrets be cached in the OS keyring when available?"
            }
            Self::CacheTtlHours => {
                "How many hours should pw-env reuse cached secrets before re-fetching them?"
            }
            Self::OpVault => "Optional 1Password vault name to search by default.",
            Self::OpAccount => {
                "Optional 1Password account shorthand when multiple accounts are configured."
            }
            Self::OpItem => {
                "Optional 1Password item name. When set, keys resolve as fields on this item."
            }
            Self::BwFolder => "Optional Bitwarden folder to search by default.",
            Self::BwOrganization => "Optional Bitwarden organization identifier.",
            Self::BwItem => {
                "Optional Bitwarden item name. When set, keys resolve as custom fields on this item."
            }
            Self::BwSyncThrottleSecs => "Minimum seconds between automatic bw sync calls.",
            Self::GpgFilePattern => "Encrypted file name or pattern used for the GPG backend.",
            Self::GpgRecipient => {
                "Optional GPG recipient used when pw-env migrate encrypts values."
            }
            Self::LogLevel => "How verbose should pw-env logging be?",
            Self::LogFile => {
                "Optional log file path. Leave empty to use the default state-directory log path."
            }
            Self::UpdatesEnabled => {
                "Should pw-env check GitHub releases for updates automatically?"
            }
            Self::UpdateCheckIntervalHours => "Minimum hours between automatic update checks.",
        }
    }

    fn is_backend_specific(self, backend: BackendChoice) -> bool {
        matches!(
            (self, backend),
            (
                Self::OpVault | Self::OpAccount | Self::OpItem,
                BackendChoice::Op
            ) | (
                Self::BwFolder | Self::BwOrganization | Self::BwItem | Self::BwSyncThrottleSecs,
                BackendChoice::Bw,
            ) | (
                Self::GpgFilePattern | Self::GpgRecipient,
                BackendChoice::Gpg
            )
        ) || !matches!(
            self,
            Self::OpVault
                | Self::OpAccount
                | Self::OpItem
                | Self::BwFolder
                | Self::BwOrganization
                | Self::BwItem
                | Self::BwSyncThrottleSecs
                | Self::GpgFilePattern
                | Self::GpgRecipient
        )
    }

    fn value(self, state: &ConfigWizardState) -> String {
        match self {
            Self::Backend => state.backend.as_str().to_string(),
            Self::SearchParentEnv => yes_no(state.search_parent_env),
            Self::SourceAll => yes_no(state.source_all),
            Self::WarnMissing => yes_no(state.warn_missing),
            Self::FallbackExampleEnv => yes_no(state.fallback_example_env),
            Self::CacheEnabled => yes_no(state.cache_enabled),
            Self::CacheTtlHours => state.cache_ttl_hours.to_string(),
            Self::OpVault => option_display(&state.op_vault),
            Self::OpAccount => option_display(&state.op_account),
            Self::OpItem => option_display(&state.op_item),
            Self::BwFolder => option_display(&state.bw_folder),
            Self::BwOrganization => option_display(&state.bw_organization),
            Self::BwItem => option_display(&state.bw_item),
            Self::BwSyncThrottleSecs => state.bw_sync_throttle_secs.to_string(),
            Self::GpgFilePattern => state.gpg_file_pattern.clone(),
            Self::GpgRecipient => option_display(&state.gpg_recipient),
            Self::LogLevel => state.log_level.as_str().to_string(),
            Self::LogFile => option_display(&state.log_file),
            Self::UpdatesEnabled => yes_no(state.updates_enabled),
            Self::UpdateCheckIntervalHours => state.updates_check_interval_hours.to_string(),
        }
    }

    fn starts_editing(self) -> bool {
        matches!(
            self,
            Self::CacheTtlHours
                | Self::OpVault
                | Self::OpAccount
                | Self::OpItem
                | Self::BwFolder
                | Self::BwOrganization
                | Self::BwItem
                | Self::BwSyncThrottleSecs
                | Self::GpgFilePattern
                | Self::GpgRecipient
                | Self::LogFile
                | Self::UpdateCheckIntervalHours
        )
    }

    fn edit_buffer(self, state: &ConfigWizardState) -> String {
        match self {
            Self::CacheTtlHours => state.cache_ttl_hours.to_string(),
            Self::OpVault => state.op_vault.clone().unwrap_or_default(),
            Self::OpAccount => state.op_account.clone().unwrap_or_default(),
            Self::OpItem => state.op_item.clone().unwrap_or_default(),
            Self::BwFolder => state.bw_folder.clone().unwrap_or_default(),
            Self::BwOrganization => state.bw_organization.clone().unwrap_or_default(),
            Self::BwItem => state.bw_item.clone().unwrap_or_default(),
            Self::BwSyncThrottleSecs => state.bw_sync_throttle_secs.to_string(),
            Self::GpgFilePattern => state.gpg_file_pattern.clone(),
            Self::GpgRecipient => state.gpg_recipient.clone().unwrap_or_default(),
            Self::LogFile => state.log_file.clone().unwrap_or_default(),
            Self::UpdateCheckIntervalHours => state.updates_check_interval_hours.to_string(),
            _ => String::new(),
        }
    }

    fn adjust(self, state: &mut ConfigWizardState, direction: i8) {
        match self {
            Self::Backend => state.backend = state.backend.cycle(direction),
            Self::SearchParentEnv => state.search_parent_env = !state.search_parent_env,
            Self::SourceAll => state.source_all = !state.source_all,
            Self::WarnMissing => state.warn_missing = !state.warn_missing,
            Self::FallbackExampleEnv => state.fallback_example_env = !state.fallback_example_env,
            Self::CacheEnabled => state.cache_enabled = !state.cache_enabled,
            Self::LogLevel => state.log_level = state.log_level.cycle(direction),
            Self::UpdatesEnabled => state.updates_enabled = !state.updates_enabled,
            _ => {}
        }
    }

    fn apply_edit(self, state: &mut ConfigWizardState, buffer: &str) -> Result<()> {
        match self {
            Self::CacheTtlHours => {
                state.cache_ttl_hours = parse_u64(self.label(), buffer)?;
            }
            Self::OpVault => state.op_vault = optional_string(buffer),
            Self::OpAccount => state.op_account = optional_string(buffer),
            Self::OpItem => state.op_item = optional_string(buffer),
            Self::BwFolder => state.bw_folder = optional_string(buffer),
            Self::BwOrganization => state.bw_organization = optional_string(buffer),
            Self::BwItem => state.bw_item = optional_string(buffer),
            Self::BwSyncThrottleSecs => {
                state.bw_sync_throttle_secs = parse_u64(self.label(), buffer)?;
            }
            Self::GpgFilePattern => {
                let value = buffer.trim();
                if value.is_empty() {
                    anyhow::bail!("{} cannot be empty", self.label());
                }
                state.gpg_file_pattern = value.to_string();
            }
            Self::GpgRecipient => state.gpg_recipient = optional_string(buffer),
            Self::LogFile => state.log_file = optional_string(buffer),
            Self::UpdateCheckIntervalHours => {
                state.updates_check_interval_hours = parse_u64(self.label(), buffer)?;
            }
            _ => {}
        }

        Ok(())
    }
}

fn parse_u64(label: &str, value: &str) -> Result<u64> {
    value
        .trim()
        .parse::<u64>()
        .with_context(|| format!("{} must be a whole number", label))
}

fn optional_string(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn option_display(value: &Option<String>) -> String {
    value.clone().unwrap_or_else(|| "<empty>".to_string())
}

fn yes_no(value: bool) -> String {
    if value {
        "yes".to_string()
    } else {
        "no".to_string()
    }
}

fn quoted(value: &str) -> String {
    toml::Value::String(value.to_string()).to_string()
}

#[mutants::skip]
fn value_span(value: String) -> Span<'static> {
    let style = match value.as_str() {
        "yes" => Style::default().fg(Color::Green),
        "no" => Style::default().fg(Color::Red),
        "<empty>" => Style::default().fg(Color::DarkGray),
        "op" => Style::default().fg(Color::Cyan),
        "bw" => Style::default().fg(Color::Yellow),
        "gpg" => Style::default().fg(Color::Magenta),
        "trace" => Style::default().fg(Color::Gray),
        "debug" => Style::default().fg(Color::Blue),
        "info" => Style::default().fg(Color::Green),
        "warn" => Style::default().fg(Color::Yellow),
        "error" => Style::default().fg(Color::Red),
        _ => Style::default(),
    };
    Span::styled(value, style)
}

fn highlight_toml(text: &str) -> Text<'static> {
    let lines = text
        .lines()
        .map(|line| {
            if line.starts_with('#') {
                Line::from(Span::styled(
                    line.to_owned(),
                    Style::default().fg(Color::DarkGray),
                ))
            } else if line.starts_with('[') {
                Line::from(Span::styled(
                    line.to_owned(),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ))
            } else if let Some(eq_pos) = line.find('=') {
                let key = line[..eq_pos].to_owned();
                let value = line[eq_pos + 1..].to_owned();
                let value_style = match value.trim() {
                    "true" => Style::default().fg(Color::Green),
                    "false" => Style::default().fg(Color::Red),
                    v if v.starts_with('"') => Style::default().fg(Color::Green),
                    _ => Style::default().fg(Color::Cyan),
                };
                Line::from(vec![
                    Span::styled(key, Style::default().fg(Color::Cyan)),
                    Span::raw("="),
                    Span::styled(value, value_style),
                ])
            } else {
                Line::from(Span::raw(line.to_owned()))
            }
        })
        .collect::<Vec<_>>();
    Text::from(lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        BwConfig, CacheConfig, Defaults, GpgConfig, LogConfig, OpConfig, UpdateConfig,
    };
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::backend::TestBackend;
    use std::fs;
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;

    #[derive(Clone, Default)]
    struct SharedWriter {
        buffer: Arc<Mutex<Vec<u8>>>,
    }

    impl SharedWriter {
        fn snapshot(&self) -> String {
            String::from_utf8(self.buffer.lock().unwrap().clone()).unwrap()
        }
    }

    impl Write for SharedWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.buffer.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn key_event(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl_key_event(ch: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL)
    }

    fn normalize_rendered_config_path(rendered: &str) -> String {
        rendered
            .lines()
            .map(|line| {
                if line.starts_with("# Path: ") {
                    "# Path: <dynamic>"
                } else {
                    line
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn is_interactive_check_requires_all_terminal_streams() {
        assert!(!is_interactive_check(true, false, true, true));
        assert!(!is_interactive_check(true, true, false, true));
        assert!(!is_interactive_check(true, true, true, false));
        assert!(!is_interactive_check(false, true, true, true));
        assert!(is_interactive_check(true, true, true, true));
    }

    #[test]
    fn state_round_trips_core_values_and_preserves_projects() {
        let config = Config {
            defaults: Defaults {
                backend: "bw".to_string(),
                search_parent_env: false,
                source_all: true,
                warn_missing: true,
                fallback_example_env: true,
                cache: CacheConfig {
                    enabled: false,
                    ttl_hours: 12,
                },
                op: OpConfig {
                    vault: Some("Work".to_string()),
                    account: Some("team".to_string()),
                    item: Some("shared-env".to_string()),
                },
                bw: BwConfig {
                    folder: Some("env".to_string()),
                    organization: Some("acme".to_string()),
                    item: Some("app".to_string()),
                    sync_throttle_secs: 7200,
                },
                gpg: GpgConfig {
                    file_pattern: ".secrets.gpg".to_string(),
                    recipient: Some("ops@example.com".to_string()),
                },
            },
            log: LogConfig {
                level: "debug".to_string(),
                file: Some("/tmp/pw-env.log".to_string()),
            },
            updates: UpdateConfig {
                enabled: false,
                check_interval_hours: 48,
            },
            projects: vec![ProjectOverride {
                path: "/tmp/project".to_string(),
                backend: Some("op".to_string()),
                commands: vec!["cargo".to_string()],
                ..ProjectOverride::default()
            }],
        };

        let round_trip = ConfigWizardState::from_config(&config).to_config();

        assert_eq!(round_trip.defaults.backend, "bw");
        assert!(!round_trip.defaults.search_parent_env);
        assert!(round_trip.defaults.source_all);
        assert!(round_trip.defaults.warn_missing);
        assert!(round_trip.defaults.fallback_example_env);
        assert!(!round_trip.defaults.cache.enabled);
        assert_eq!(round_trip.defaults.cache.ttl_hours, 12);
        assert_eq!(round_trip.defaults.op.vault.as_deref(), Some("Work"));
        assert_eq!(round_trip.defaults.op.account.as_deref(), Some("team"));
        assert_eq!(round_trip.defaults.op.item.as_deref(), Some("shared-env"));
        assert_eq!(round_trip.defaults.bw.folder.as_deref(), Some("env"));
        assert_eq!(round_trip.defaults.bw.organization.as_deref(), Some("acme"));
        assert_eq!(round_trip.defaults.bw.item.as_deref(), Some("app"));
        assert_eq!(round_trip.defaults.bw.sync_throttle_secs, 7200);
        assert_eq!(round_trip.defaults.gpg.file_pattern, ".secrets.gpg");
        assert_eq!(
            round_trip.defaults.gpg.recipient.as_deref(),
            Some("ops@example.com")
        );
        assert_eq!(round_trip.log.level, "debug");
        assert_eq!(round_trip.log.file.as_deref(), Some("/tmp/pw-env.log"));
        assert!(!round_trip.updates.enabled);
        assert_eq!(round_trip.updates.check_interval_hours, 48);
        assert_eq!(round_trip.projects.len(), 1);
        assert_eq!(round_trip.projects[0].path, "/tmp/project");
        assert_eq!(round_trip.projects[0].backend.as_deref(), Some("op"));
        assert_eq!(round_trip.projects[0].commands, vec!["cargo".to_string()]);
    }

    #[test]
    fn render_config_includes_live_values_and_project_overrides() {
        let state = ConfigWizardState {
            backend: BackendChoice::Gpg,
            search_parent_env: true,
            source_all: false,
            warn_missing: true,
            fallback_example_env: false,
            cache_enabled: true,
            cache_ttl_hours: 8,
            op_vault: None,
            op_account: None,
            op_item: None,
            bw_folder: Some("secrets".to_string()),
            bw_organization: None,
            bw_item: Some("api".to_string()),
            bw_sync_throttle_secs: 5400,
            gpg_file_pattern: ".team.gpg".to_string(),
            gpg_recipient: Some("dev@example.com".to_string()),
            log_level: LogLevelChoice::Warn,
            log_file: Some("/tmp/pw-env.log".to_string()),
            updates_enabled: true,
            updates_check_interval_hours: 12,
            projects: vec![ProjectOverride {
                path: "/workspace/app".to_string(),
                backend: Some("gpg".to_string()),
                ..ProjectOverride::default()
            }],
        };

        let rendered = state.render_config();

        assert!(rendered.contains("backend = \"gpg\""));
        assert!(rendered.contains("warn_missing = true"));
        assert!(rendered.contains("folder = \"secrets\""));
        assert!(rendered.contains("sync_throttle_secs = 5400"));
        assert!(rendered.contains("file_pattern = \".team.gpg\""));
        assert!(rendered.contains("recipient = \"dev@example.com\""));
        assert!(rendered.contains("level = \"warn\""));
        assert!(rendered.contains("[[projects]]"));
        assert!(rendered.contains("path = \"/workspace/app\""));
    }

    #[test]
    fn render_config_includes_defaults_op_section_when_only_account_is_set() {
        let mut state = ConfigWizardState::from_config(&Config::default());
        state.op_account = Some("team".to_string());

        let rendered = state.render_config();

        assert!(rendered.contains("[defaults.op]"));
        assert!(rendered.contains("account = \"team\""));
        assert!(!rendered.contains("vault = "));
        assert!(!rendered.contains("item = "));
    }

    #[test]
    fn save_config_to_path_creates_expected_file() {
        let workspace = TempDir::new().unwrap();
        let target = workspace.path().join("pw-env/config.toml");
        let contents = ConfigWizardState::from_config(&Config::default()).render_config();

        save_config_to_path(&target, &contents).unwrap();

        let saved = fs::read_to_string(&target).unwrap();
        assert_eq!(saved, contents);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mode = fs::metadata(&target).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn apply_edit_rejects_invalid_numbers() {
        let mut state = ConfigWizardState::from_config(&Config::default());
        let error = FieldId::CacheTtlHours
            .apply_edit(&mut state, "nope")
            .unwrap_err();

        assert!(error.to_string().contains("whole number"));
        assert_eq!(state.cache_ttl_hours, 4);
    }

    #[test]
    fn default_state_uses_expected_backend_defaults() {
        let state = ConfigWizardState::from_config(&Config::default());

        assert_eq!(state.backend.as_str(), "op");
        assert_eq!(state.bw_sync_throttle_secs, DEFAULT_BW_SYNC_THROTTLE_SECS);
        assert_eq!(state.gpg_file_pattern, DEFAULT_GPG_FILE_PATTERN);
    }

    #[test]
    fn backend_choice_from_value_maps_gpg() {
        assert_eq!(BackendChoice::from_value("gpg").as_str(), "gpg");
    }

    #[test]
    fn log_level_choice_from_value_maps_warn_and_error() {
        assert_eq!(LogLevelChoice::from_value("warn").as_str(), "warn");
        assert_eq!(LogLevelChoice::from_value("error").as_str(), "error");
    }

    #[test]
    fn log_level_choice_from_value_maps_trace() {
        assert_eq!(LogLevelChoice::from_value("trace").as_str(), "trace");
    }

    #[test]
    fn field_labels_and_help_text_match_expected_copy() {
        assert_eq!(FieldId::LogLevel.label(), "Log level");
        assert_eq!(
            FieldId::LogLevel.help(),
            "How verbose should pw-env logging be?"
        );
    }

    #[test]
    fn value_returns_exact_op_account_string() {
        let mut state = ConfigWizardState::from_config(&Config::default());
        state.op_account = Some("team".to_string());

        let value = FieldId::OpAccount.value(&state);

        assert_eq!(value, "team");
    }

    #[test]
    fn value_returns_exact_gpg_file_pattern_string() {
        let mut state = ConfigWizardState::from_config(&Config::default());
        state.gpg_file_pattern = ".team.gpg".to_string();

        let value = FieldId::GpgFilePattern.value(&state);

        assert_eq!(value, ".team.gpg");
    }

    #[test]
    fn backend_specific_fields_follow_backend_and_shared_visibility_rules() {
        assert!(FieldId::OpVault.is_backend_specific(BackendChoice::Op));
        assert!(!FieldId::OpVault.is_backend_specific(BackendChoice::Bw));
        assert!(FieldId::LogLevel.is_backend_specific(BackendChoice::Gpg));
    }

    #[test]
    fn adjust_toggles_boolean_fields_and_cycles_log_level() {
        let mut state = ConfigWizardState::from_config(&Config::default());

        FieldId::SearchParentEnv.adjust(&mut state, 1);
        FieldId::SourceAll.adjust(&mut state, 1);
        FieldId::WarnMissing.adjust(&mut state, 1);
        FieldId::FallbackExampleEnv.adjust(&mut state, 1);
        FieldId::CacheEnabled.adjust(&mut state, 1);
        FieldId::LogLevel.adjust(&mut state, 1);
        FieldId::UpdatesEnabled.adjust(&mut state, 1);

        assert!(!state.search_parent_env);
        assert!(state.source_all);
        assert!(state.warn_missing);
        assert!(state.fallback_example_env);
        assert!(!state.cache_enabled);
        assert_eq!(state.log_level.as_str(), "warn");
        assert!(!state.updates_enabled);
    }

    #[test]
    fn apply_edit_updates_optional_and_numeric_fields() {
        let mut state = ConfigWizardState::from_config(&Config::default());

        FieldId::OpVault.apply_edit(&mut state, " work ").unwrap();
        FieldId::OpAccount.apply_edit(&mut state, " team ").unwrap();
        FieldId::BwSyncThrottleSecs
            .apply_edit(&mut state, "7200")
            .unwrap();
        FieldId::GpgFilePattern
            .apply_edit(&mut state, " .secrets.gpg ")
            .unwrap();

        assert_eq!(state.op_vault.as_deref(), Some("work"));
        assert_eq!(state.op_account.as_deref(), Some("team"));
        assert_eq!(state.bw_sync_throttle_secs, 7200);
        assert_eq!(state.gpg_file_pattern, ".secrets.gpg");
    }

    #[test]
    fn apply_edit_updates_backend_specific_optional_fields() {
        let mut state = ConfigWizardState::from_config(&Config::default());

        FieldId::OpItem.apply_edit(&mut state, " deploy ").unwrap();
        FieldId::BwFolder.apply_edit(&mut state, " env ").unwrap();
        FieldId::BwOrganization
            .apply_edit(&mut state, " acme ")
            .unwrap();
        FieldId::BwItem.apply_edit(&mut state, " api ").unwrap();
        FieldId::GpgRecipient
            .apply_edit(&mut state, " ops@example.com ")
            .unwrap();

        assert_eq!(state.op_item.as_deref(), Some("deploy"));
        assert_eq!(state.bw_folder.as_deref(), Some("env"));
        assert_eq!(state.bw_organization.as_deref(), Some("acme"));
        assert_eq!(state.bw_item.as_deref(), Some("api"));
        assert_eq!(state.gpg_recipient.as_deref(), Some("ops@example.com"));
    }

    #[test]
    fn apply_edit_updates_log_file_and_update_interval() {
        let mut state = ConfigWizardState::from_config(&Config::default());

        FieldId::LogFile
            .apply_edit(&mut state, " /tmp/custom.log ")
            .unwrap();
        FieldId::UpdateCheckIntervalHours
            .apply_edit(&mut state, "72")
            .unwrap();

        assert_eq!(state.log_file.as_deref(), Some("/tmp/custom.log"));
        assert_eq!(state.updates_check_interval_hours, 72);
    }

    #[test]
    fn handle_key_delegates_normal_mode_keys() {
        let mut app = WizardApp::new(&Config::default());

        let outcome = app.handle_key(key_event(KeyCode::Esc)).unwrap();

        assert!(matches!(outcome, Some(WizardOutcome::Cancelled)));
    }

    #[test]
    fn handle_normal_key_esc_cancels() {
        let mut app = WizardApp::new(&Config::default());

        let outcome = app.handle_normal_key(key_event(KeyCode::Esc)).unwrap();

        assert!(matches!(outcome, Some(WizardOutcome::Cancelled)));
    }

    #[test]
    fn handle_normal_key_ctrl_c_cancels() {
        let mut app = WizardApp::new(&Config::default());

        let outcome = app.handle_normal_key(ctrl_key_event('c')).unwrap();

        assert!(matches!(outcome, Some(WizardOutcome::Cancelled)));
    }

    #[test]
    fn handle_normal_key_plain_c_does_not_cancel() {
        let mut app = WizardApp::new(&Config::default());

        let outcome = app
            .handle_normal_key(key_event(KeyCode::Char('c')))
            .unwrap();

        assert!(outcome.is_none());
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn handle_normal_key_up_moves_selection_up() {
        let mut app = WizardApp::new(&Config::default());
        app.selected = 1;

        let outcome = app.handle_normal_key(key_event(KeyCode::Up)).unwrap();

        assert!(outcome.is_none());
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn handle_normal_key_up_at_first_field_keeps_selection_at_zero() {
        let mut app = WizardApp::new(&Config::default());

        let outcome = app.handle_normal_key(key_event(KeyCode::Up)).unwrap();

        assert!(outcome.is_none());
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn handle_normal_key_down_moves_selection_down() {
        let mut app = WizardApp::new(&Config::default());

        let outcome = app.handle_normal_key(key_event(KeyCode::Down)).unwrap();

        assert!(outcome.is_none());
        assert_eq!(app.selected, 1);
    }

    #[test]
    fn handle_normal_key_down_at_last_field_keeps_selection_on_last_field() {
        let mut app = WizardApp::new(&Config::default());
        app.selected = ALL_FIELDS.len() - 1;

        let outcome = app.handle_normal_key(key_event(KeyCode::Down)).unwrap();

        assert!(outcome.is_none());
        assert_eq!(app.selected, ALL_FIELDS.len() - 1);
        assert_eq!(app.selected_field().label(), "Update check interval");
    }

    #[test]
    fn handle_normal_key_left_cycles_backend_backward() {
        let mut app = WizardApp::new(&Config::default());

        let outcome = app.handle_normal_key(key_event(KeyCode::Left)).unwrap();

        assert!(outcome.is_none());
        assert_eq!(app.state.backend.as_str(), "gpg");
    }

    #[test]
    fn handle_normal_key_right_cycles_backend_forward() {
        let mut app = WizardApp::new(&Config::default());

        let outcome = app.handle_normal_key(key_event(KeyCode::Right)).unwrap();

        assert!(outcome.is_none());
        assert_eq!(app.state.backend.as_str(), "bw");
    }

    #[test]
    fn handle_normal_key_enter_starts_editing_for_editable_field() {
        let mut config = Config::default();
        config.defaults.op.vault = Some("Work".to_string());
        let mut app = WizardApp::new(&config);
        app.selected = 7;

        let outcome = app.handle_normal_key(key_event(KeyCode::Enter)).unwrap();

        assert!(outcome.is_none());
        assert!(matches!(
            app.mode,
            InputMode::Editing { ref buffer } if buffer == "Work"
        ));
        assert_eq!(
            app.status,
            "Press Enter to apply 1Password vault or Esc to cancel."
        );
    }

    #[test]
    fn handle_normal_key_enter_toggles_non_editable_field_without_edit_mode() {
        let mut app = WizardApp::new(&Config::default());
        app.selected = 1;

        let outcome = app.handle_normal_key(key_event(KeyCode::Enter)).unwrap();

        assert!(outcome.is_none());
        assert!(!app.state.search_parent_env);
        assert!(matches!(app.mode, InputMode::Normal));
    }

    #[test]
    fn handle_normal_key_space_toggles_selected_field() {
        let mut app = WizardApp::new(&Config::default());
        app.selected = 1;

        let outcome = app
            .handle_normal_key(key_event(KeyCode::Char(' ')))
            .unwrap();

        assert!(outcome.is_none());
        assert!(!app.state.search_parent_env);
    }

    #[test]
    fn handle_normal_key_saves_rendered_config() {
        let mut app = WizardApp::new(&Config::default());
        let expected = normalize_rendered_config_path(&app.state.render_config());

        let outcome = app
            .handle_normal_key(key_event(KeyCode::Char('s')))
            .unwrap();

        match outcome {
            Some(WizardOutcome::Save(rendered)) => {
                assert_eq!(normalize_rendered_config_path(&rendered), expected)
            }
            _ => panic!("expected save outcome"),
        }
    }

    #[test]
    fn handle_editing_key_appends_plain_characters() {
        let mut app = WizardApp::new(&Config::default());
        let mut buffer = "ab".to_string();

        let keep_editing = app
            .handle_editing_key(key_event(KeyCode::Char('c')), &mut buffer)
            .unwrap();

        assert!(keep_editing);
        assert_eq!(buffer, "abc");
    }

    #[test]
    fn handle_editing_key_escape_cancels_current_edit() {
        let mut app = WizardApp::new(&Config::default());
        app.selected = 11;
        app.mode = InputMode::Editing {
            buffer: "acme".to_string(),
        };
        let mut buffer = "acme".to_string();

        let keep_editing = app
            .handle_editing_key(key_event(KeyCode::Esc), &mut buffer)
            .unwrap();

        assert!(!keep_editing);
        assert!(matches!(app.mode, InputMode::Normal));
        assert_eq!(buffer, "acme");
        assert_eq!(app.status, "Cancelled edit for Bitwarden organization.");
    }

    #[test]
    fn handle_editing_key_enter_applies_bitwarden_organization_edit() {
        let mut app = WizardApp::new(&Config::default());
        app.selected = 11;
        app.mode = InputMode::Editing {
            buffer: " acme ".to_string(),
        };
        let mut buffer = " acme ".to_string();

        let keep_editing = app
            .handle_editing_key(key_event(KeyCode::Enter), &mut buffer)
            .unwrap();

        assert!(!keep_editing);
        assert!(matches!(app.mode, InputMode::Normal));
        assert_eq!(app.state.bw_organization.as_deref(), Some("acme"));
        assert_eq!(app.status, "Updated Bitwarden organization.");
    }

    #[test]
    fn handle_editing_key_backspace_removes_last_character() {
        let mut app = WizardApp::new(&Config::default());
        let mut buffer = "acme".to_string();

        let keep_editing = app
            .handle_editing_key(key_event(KeyCode::Backspace), &mut buffer)
            .unwrap();

        assert!(keep_editing);
        assert_eq!(buffer, "acm");
    }

    #[test]
    fn handle_editing_key_ignores_control_characters() {
        let mut app = WizardApp::new(&Config::default());
        let mut buffer = "ab".to_string();

        let keep_editing = app
            .handle_editing_key(ctrl_key_event('c'), &mut buffer)
            .unwrap();

        assert!(keep_editing);
        assert_eq!(buffer, "ab");
    }

    #[test]
    fn starts_editing_returns_false_for_toggle_field() {
        assert!(!FieldId::SearchParentEnv.starts_editing());
    }

    #[test]
    fn edit_buffer_returns_op_account_value() {
        let mut state = ConfigWizardState::from_config(&Config::default());
        state.op_account = Some("team".to_string());

        let buffer = FieldId::OpAccount.edit_buffer(&state);

        assert_eq!(buffer, "team");
    }

    #[test]
    fn edit_buffer_returns_op_item_value() {
        let mut state = ConfigWizardState::from_config(&Config::default());
        state.op_item = Some("deploy".to_string());

        let buffer = FieldId::OpItem.edit_buffer(&state);

        assert_eq!(buffer, "deploy");
    }

    #[test]
    fn edit_buffer_returns_bitwarden_folder_value() {
        let mut state = ConfigWizardState::from_config(&Config::default());
        state.bw_folder = Some("env".to_string());

        let buffer = FieldId::BwFolder.edit_buffer(&state);

        assert_eq!(buffer, "env");
    }

    #[test]
    fn edit_buffer_returns_bitwarden_organization_value() {
        let mut state = ConfigWizardState::from_config(&Config::default());
        state.bw_organization = Some("acme".to_string());

        let buffer = FieldId::BwOrganization.edit_buffer(&state);

        assert_eq!(buffer, "acme");
    }

    #[test]
    fn edit_buffer_returns_bitwarden_item_value() {
        let mut state = ConfigWizardState::from_config(&Config::default());
        state.bw_item = Some("api".to_string());

        let buffer = FieldId::BwItem.edit_buffer(&state);

        assert_eq!(buffer, "api");
    }

    #[test]
    fn edit_buffer_returns_bitwarden_sync_throttle_value() {
        let mut state = ConfigWizardState::from_config(&Config::default());
        state.bw_sync_throttle_secs = 7200;

        let buffer = FieldId::BwSyncThrottleSecs.edit_buffer(&state);

        assert_eq!(buffer, "7200");
    }

    #[test]
    fn edit_buffer_returns_gpg_file_pattern_value() {
        let mut state = ConfigWizardState::from_config(&Config::default());
        state.gpg_file_pattern = ".team.gpg".to_string();

        let buffer = FieldId::GpgFilePattern.edit_buffer(&state);

        assert_eq!(buffer, ".team.gpg");
    }

    #[test]
    fn edit_buffer_returns_log_file_value() {
        let mut state = ConfigWizardState::from_config(&Config::default());
        state.log_file = Some("/tmp/custom.log".to_string());

        let buffer = FieldId::LogFile.edit_buffer(&state);

        assert_eq!(buffer, "/tmp/custom.log");
    }

    #[test]
    fn edit_buffer_returns_update_check_interval_value() {
        let mut state = ConfigWizardState::from_config(&Config::default());
        state.updates_check_interval_hours = 72;

        let buffer = FieldId::UpdateCheckIntervalHours.edit_buffer(&state);

        assert_eq!(buffer, "72");
    }

    #[test]
    fn option_display_formats_some_and_none() {
        assert_eq!(option_display(&Some("value".to_string())), "value");
        assert_eq!(option_display(&None), "<empty>");
    }

    #[test]
    fn yes_no_formats_true_and_false() {
        assert_eq!(yes_no(true), "yes");
        assert_eq!(yes_no(false), "no");
    }

    #[test]
    fn render_draws_visible_sections_into_terminal_buffer() {
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let app = WizardApp::new(&Config::default());

        terminal.draw(|frame| render(frame, &app)).unwrap();

        let screen = format!("{}", terminal.backend());
        assert!(screen.contains("Questions"), "screen was: {screen}");
        assert!(screen.contains("Built Config"), "screen was: {screen}");
        assert!(screen.contains("Controls"), "screen was: {screen}");
    }

    #[test]
    fn terminal_session_drop_restores_terminal_escape_sequences() {
        let writer = SharedWriter::default();
        let snapshot = writer.clone();

        drop(TerminalSession::new_for_test(writer));

        let output = snapshot.snapshot();
        assert!(output.contains("\u{1b}[?25h"), "output was: {output:?}");
        assert!(output.contains("\u{1b}[?1049l"), "output was: {output:?}");
    }
}
