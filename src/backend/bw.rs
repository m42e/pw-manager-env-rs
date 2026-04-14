use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Mutex;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tracing::{debug, info, trace, warn};

use super::{
    Backend, CREATED_WITH_FIELD_NAME, MIGRATED_FROM_FIELD_NAME, PROJECT_FIELD_NAME,
    REPOSITORY_FIELD_NAME, ResolveContext, StoreContext,
};
use crate::progress::suspend_progress_output;

/// Cached BW_SESSION key. `None` means not yet determined.
static SESSION: Mutex<Option<String>> = Mutex::new(None);

/// Cached folder name → UUID mappings (non-secret metadata).
static FOLDER_ID_CACHE: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);
static SYNC_THROTTLE_OVERRIDE_SECS: Mutex<Option<u64>> = Mutex::new(None);

const FOLDER_ID_CACHE_FILE_NAME: &str = "bitwarden-folder-ids.json";
const SYNC_STATE_FILE_NAME: &str = "bitwarden-sync-state.json";
const MIN_SYNC_THROTTLE_SECS: u64 = 3600;
const DEFAULT_SYNC_THROTTLE_SECS: u64 = MIN_SYNC_THROTTLE_SECS;

#[derive(Debug, Default, Deserialize, Serialize)]
struct FolderIdCacheStore {
    #[serde(default)]
    folder_ids: HashMap<String, String>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct SyncStateStore {
    last_sync_unix_secs: Option<u64>,
}

#[cfg(test)]
thread_local! {
    static TEST_FOLDER_CACHE_PATH: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
    static TEST_SYNC_STATE_PATH: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
    static TEST_PROMPT_UNLOCK_PASSWORD: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Write a file with owner-only permissions (0o600) on Unix.
fn write_private_file(path: &Path, contents: &str) -> Result<()> {
    #[cfg(unix)]
    {
        use std::fs::OpenOptions;
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;

        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("Failed to create {}", path.display()))?;
        file.write_all(contents.as_bytes())
            .with_context(|| format!("Failed to write {}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, contents)
            .with_context(|| format!("Failed to write {}", path.display()))?;
    }

    Ok(())
}

impl FolderIdCacheStore {
    fn load() -> Result<Self> {
        let Some(path) = Self::path() else {
            return Ok(Self::default());
        };

        if !path.exists() {
            return Ok(Self::default());
        }

        let contents = std::fs::read_to_string(&path).with_context(|| {
            format!(
                "Failed to read Bitwarden folder cache from {}",
                path.display()
            )
        })?;
        serde_json::from_str(&contents).with_context(|| {
            format!(
                "Failed to parse Bitwarden folder cache from {}",
                path.display()
            )
        })
    }

    fn save(&self) -> Result<()> {
        let Some(path) = Self::path() else {
            return Ok(());
        };

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create state directory {}", parent.display())
            })?;
        }

        let contents = serde_json::to_string_pretty(self)
            .context("Failed to serialize Bitwarden folder cache")?;
        write_private_file(&path, &contents).with_context(|| {
            format!(
                "Failed to write Bitwarden folder cache to {}",
                path.display()
            )
        })
    }

    fn path() -> Option<PathBuf> {
        #[cfg(test)]
        if let Some(path) = TEST_FOLDER_CACHE_PATH.with(|value| value.borrow().clone()) {
            return Some(path);
        }

        Some(
            crate::config::state_dir()
                .join("pw-env")
                .join(FOLDER_ID_CACHE_FILE_NAME),
        )
    }
}

impl SyncStateStore {
    fn load() -> Result<Self> {
        let Some(path) = Self::path() else {
            return Ok(Self::default());
        };

        if !path.exists() {
            return Ok(Self::default());
        }

        let contents = std::fs::read_to_string(&path).with_context(|| {
            format!(
                "Failed to read Bitwarden sync state from {}",
                path.display()
            )
        })?;
        serde_json::from_str(&contents).with_context(|| {
            format!(
                "Failed to parse Bitwarden sync state from {}",
                path.display()
            )
        })
    }

    fn save(&self) -> Result<()> {
        let Some(path) = Self::path() else {
            return Ok(());
        };

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create state directory {}", parent.display())
            })?;
        }

        let contents = serde_json::to_string_pretty(self)
            .context("Failed to serialize Bitwarden sync state")?;
        write_private_file(&path, &contents)
            .with_context(|| format!("Failed to write Bitwarden sync state to {}", path.display()))
    }

    fn path() -> Option<PathBuf> {
        #[cfg(test)]
        if let Some(path) = TEST_SYNC_STATE_PATH.with(|value| value.borrow().clone()) {
            return Some(path);
        }

        Some(
            crate::config::state_dir()
                .join("pw-env")
                .join(SYNC_STATE_FILE_NAME),
        )
    }
}

pub struct BwBackend;

impl BwBackend {
    fn effective_sync_throttle_secs() -> u64 {
        SYNC_THROTTLE_OVERRIDE_SECS
            .lock()
            .unwrap()
            .unwrap_or(DEFAULT_SYNC_THROTTLE_SECS)
            .max(MIN_SYNC_THROTTLE_SECS)
    }

    fn configure_sync_throttle(sync_throttle_secs: u64) {
        *SYNC_THROTTLE_OVERRIDE_SECS.lock().unwrap() = Some(sync_throttle_secs);
    }

    fn should_retry_after_sync(error: &anyhow::Error) -> bool {
        let message = error.to_string().to_ascii_lowercase();
        message.contains("no bitwarden items found")
            || message.contains("field '") && message.contains("not found in bitwarden item")
            || message.contains("failed to fetch bitwarden item")
            || message.contains("failed to list bitwarden items")
            || message.contains("object not found")
    }

    fn current_unix_secs() -> Option<u64> {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|duration| duration.as_secs())
    }

    fn log_action_timing(action: &str, started_at: Instant, success: bool) {
        debug!(
            action,
            duration_ms = started_at.elapsed().as_millis(),
            success,
            "Bitwarden action finished"
        );
    }

    fn run_timed_command(mut cmd: Command, action: &str, exec_context: &str) -> Result<Output> {
        let started_at = Instant::now();
        match cmd.output() {
            Ok(output) => {
                Self::log_action_timing(action, started_at, output.status.success());
                Ok(output)
            }
            Err(error) => {
                Self::log_action_timing(action, started_at, false);
                Err(error).context(exec_context.to_string())
            }
        }
    }

    pub fn folder_cache_path() -> Option<PathBuf> {
        FolderIdCacheStore::path()
    }

    pub fn sync_state_path() -> Option<PathBuf> {
        SyncStateStore::path()
    }

    pub fn clear_folder_cache() -> Result<bool> {
        *FOLDER_ID_CACHE.lock().unwrap() = None;

        let Some(path) = FolderIdCacheStore::path() else {
            return Ok(false);
        };

        if !path.exists() {
            return Ok(false);
        }

        std::fs::remove_file(&path).with_context(|| {
            format!(
                "Failed to remove Bitwarden folder cache at {}",
                path.display()
            )
        })?;
        Ok(true)
    }

    pub fn clear_sync_state() -> Result<bool> {
        let Some(path) = SyncStateStore::path() else {
            return Ok(false);
        };

        if !path.exists() {
            return Ok(false);
        }

        std::fs::remove_file(&path).with_context(|| {
            format!(
                "Failed to remove Bitwarden sync state at {}",
                path.display()
            )
        })?;
        Ok(true)
    }

    fn migration_metadata_fields(ctx: &StoreContext) -> Vec<serde_json::Value> {
        let mut fields = vec![
            serde_json::json!({
                "name": MIGRATED_FROM_FIELD_NAME,
                "value": ctx.migrated_from(),
                "type": 0
            }),
            serde_json::json!({
                "name": CREATED_WITH_FIELD_NAME,
                "value": ctx.created_with(),
                "type": 0
            }),
        ];
        if let Some(project) = ctx.project.as_deref() {
            fields.push(serde_json::json!({
                "name": PROJECT_FIELD_NAME,
                "value": project,
                "type": 0
            }));
        }
        if let Some(repository) = ctx.repository.as_deref() {
            fields.push(serde_json::json!({
                "name": REPOSITORY_FIELD_NAME,
                "value": repository,
                "type": 0
            }));
        }
        fields
    }

    fn upsert_custom_field(item: &mut serde_json::Value, name: &str, value: &str, field_type: u8) {
        let fields = item
            .as_object_mut()
            .and_then(|object| {
                object
                    .entry("fields")
                    .or_insert_with(|| serde_json::json!([]))
                    .as_array_mut()
            })
            .expect("Bitwarden item fields must be an array");

        if let Some(existing) = fields.iter_mut().find(|field| {
            field.get("name").and_then(|field_name| field_name.as_str()) == Some(name)
        }) {
            *existing = serde_json::json!({
                "name": name,
                "value": value,
                "type": field_type
            });
        } else {
            fields.push(serde_json::json!({
                "name": name,
                "value": value,
                "type": field_type
            }));
        }
    }

    /// Ensure the Bitwarden vault is unlocked and return the session key.
    ///
    /// Resolution order:
    /// 1. Return the cached session from a previous call.
    /// 2. Use the `BW_SESSION` environment variable if set.
    /// 3. Run `bw status` to inspect vault state:
    ///    - *unauthenticated* → fail fast, ask user to `bw login`.
    ///    - *locked*          → interactively prompt `bw unlock` and cache the key.
    ///    - *unlocked*        → proceed without a session key.
    fn ensure_session() -> Result<String> {
        let mut guard = SESSION.lock().unwrap();
        if let Some(ref session) = *guard {
            trace!("Reusing cached Bitwarden session");
            return Ok(session.clone());
        }

        debug!("No cached Bitwarden session, resolving…");

        // 1. BW_SESSION already exported by the caller / shell
        if let Ok(session) = std::env::var("BW_SESSION") {
            if !session.is_empty() {
                info!("Using BW_SESSION from environment");
                *guard = Some(session.clone());
                drop(guard);
                Self::sync_vault();
                return Ok(session);
            }
            debug!("BW_SESSION is set but empty, falling back to bw status");
        }

        // 2. Ask bw for its current status
        debug!("Running: bw status");
        let status_json = {
            let mut cmd = Command::new("bw");
            cmd.arg("status");
            cmd.stdin(std::process::Stdio::null());
            let output = Self::run_timed_command(
                cmd,
                "bw status",
                "Failed to execute `bw` CLI. Is Bitwarden CLI installed?",
            )?;
            String::from_utf8(output.stdout).context("bw output was not valid UTF-8")?
        };

        let status: serde_json::Value = serde_json::from_str(status_json.trim())
            .context("Failed to parse `bw status` output")?;
        let vault_status = status
            .get("status")
            .and_then(|s| s.as_str())
            .unwrap_or("unknown");
        debug!(vault_status, "Bitwarden vault status");

        match vault_status {
            "unauthenticated" => {
                bail!("Not logged in to Bitwarden. Please log in first:\n\n  bw login\n");
            }
            "locked" => {
                let session = Self::prompt_unlock()?;
                *guard = Some(session.clone());
                drop(guard);
                Self::sync_vault();
                Ok(session)
            }
            "unlocked" => {
                debug!("Bitwarden vault is already unlocked");
                let session = String::new();
                *guard = Some(session.clone());
                drop(guard);
                Self::sync_vault();
                Ok(session)
            }
            other => {
                bail!("Unknown Bitwarden vault status: {other}");
            }
        }
    }

    /// Sync the Bitwarden vault to ensure the local cache is up to date.
    /// Logs a warning on failure but does not abort — a stale cache is
    /// better than blocking the entire workflow.
    fn sync_vault() {
        Self::sync_vault_with_mode(false);
    }

    fn force_sync_vault() {
        Self::sync_vault_with_mode(true);
    }

    fn sync_vault_with_mode(force: bool) {
        let sync_throttle_secs = Self::effective_sync_throttle_secs();
        if !force {
            match SyncStateStore::load() {
                Ok(state) => {
                    if let (Some(last_sync), Some(now)) =
                        (state.last_sync_unix_secs, Self::current_unix_secs())
                    {
                        let age_secs = now.saturating_sub(last_sync);
                        if age_secs < sync_throttle_secs {
                            debug!(
                                age_secs,
                                throttle_secs = sync_throttle_secs,
                                "Skipping Bitwarden sync because the last successful sync was recent"
                            );
                            return;
                        }
                    }
                }
                Err(error) => {
                    warn!("Failed to load Bitwarden sync state (continuing with sync): {error}");
                }
            }
        } else {
            debug!("Forcing Bitwarden sync after lookup failure");
        }

        debug!("Syncing Bitwarden vault");
        match Self::run_bw(&["sync"]) {
            Ok(_) => {
                info!("Bitwarden vault synced");
                if let Some(now) = Self::current_unix_secs()
                    && let Err(error) = (SyncStateStore {
                        last_sync_unix_secs: Some(now),
                    })
                    .save()
                {
                    warn!("Failed to persist Bitwarden sync state (continuing): {error}");
                }
            }
            Err(e) => warn!("Bitwarden sync failed (continuing with local cache): {e}"),
        }
    }

    /// Create a pre-configured `bw` [`Command`] with the session key set.
    fn bw_command() -> Result<Command> {
        let session = Self::ensure_session()?;
        let mut cmd = Command::new("bw");
        if !session.is_empty() {
            trace!("Injecting BW_SESSION into command environment");
            cmd.env("BW_SESSION", &session);
        }
        Ok(cmd)
    }

    /// Prompt the user for their master password and unlock the vault.
    ///
    /// Uses `dialoguer` for the password prompt (no ANSI escape leakage)
    /// and passes the password to `bw unlock` via `--passwordenv` so the
    /// child process never writes interactive escape codes.
    fn prompt_unlock() -> Result<String> {
        info!("Bitwarden vault is locked, prompting for unlock");
        let password = {
            #[cfg(test)]
            if let Some(password) = TEST_PROMPT_UNLOCK_PASSWORD.with(|value| value.borrow().clone())
            {
                password
            } else {
                let _progress_suspension = suspend_progress_output();
                dialoguer::Password::new()
                    .with_prompt("Bitwarden master password")
                    .interact()
                    .context("Failed to read master password")?
            }
            #[cfg(not(test))]
            {
                let _progress_suspension = suspend_progress_output();
                dialoguer::Password::new()
                    .with_prompt("Bitwarden master password")
                    .interact()
                    .context("Failed to read master password")?
            }
        };

        debug!("Running: bw unlock --raw --passwordenv ...");
        let mut cmd = Command::new("bw");
        cmd.args(["unlock", "--raw", "--passwordenv", "BW_MASTER_PW"])
            .env("BW_MASTER_PW", &password)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let output = Self::run_timed_command(
            cmd,
            "bw unlock --raw --passwordenv",
            "Failed to run `bw unlock`",
        )?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            debug!(%stderr, "bw unlock failed");
            bail!("Failed to unlock Bitwarden vault: {stderr}");
        }

        let session = String::from_utf8(output.stdout)
            .context("BW_SESSION was not valid UTF-8")?
            .trim()
            .to_string();
        if session.is_empty() {
            bail!("Failed to obtain session key from `bw unlock`");
        }
        info!("Bitwarden vault unlocked successfully");
        Ok(session)
    }

    /// Invalidate the cached session so the next call to [`ensure_session`]
    /// re-checks the vault status and potentially re-prompts.
    fn invalidate_session() {
        let mut guard = SESSION.lock().unwrap();
        if guard.is_some() {
            debug!("Invalidating cached Bitwarden session");
            *guard = None;
        }
    }

    /// Returns `true` if the stderr output indicates a stale / invalid session.
    fn is_stale_session_error(stderr: &str) -> bool {
        let lower = stderr.to_lowercase();
        lower.contains("invalid master password")
            || lower.contains("session key is invalid")
            || lower.contains("not logged in")
    }

    /// Resolve a Bitwarden folder name to its UUID, using an in-process cache.
    fn resolve_folder_id(folder_name: &str) -> Result<Option<String>> {
        {
            let mut guard = FOLDER_ID_CACHE.lock().unwrap();
            if guard.is_none() {
                match FolderIdCacheStore::load() {
                    Ok(store) => {
                        let len = store.folder_ids.len();
                        if len > 0 {
                            debug!(entry_count = len, "Loaded persisted Bitwarden folder cache");
                        }
                        *guard = Some(store.folder_ids);
                    }
                    Err(error) => {
                        warn!(
                            "Failed to load Bitwarden folder cache (continuing without cache): {error}"
                        );
                        *guard = Some(HashMap::new());
                    }
                }
            }

            if let Some(cache) = guard.as_ref()
                && let Some(id) = cache.get(folder_name)
            {
                trace!(folder_name, folder_id = %id, "Folder ID cache hit");
                return Ok(Some(id.clone()));
            }
        }

        debug!(folder_name, "Looking up Bitwarden folder ID");
        let folders_json = Self::run_bw(&["list", "folders", "--search", folder_name])?;
        let folders: serde_json::Value = serde_json::from_str(&folders_json)?;
        if let Some(folder_arr) = folders.as_array() {
            // --search is a fuzzy/substring match; find the exact name match
            let exact = folder_arr
                .iter()
                .find(|f| f.get("name").and_then(|n| n.as_str()) == Some(folder_name));
            if let Some(folder) = exact
                && let Some(id) = folder.get("id").and_then(|i| i.as_str())
            {
                debug!(folder_name, folder_id = %id, "Resolved Bitwarden folder");

                let mut guard = FOLDER_ID_CACHE.lock().unwrap();
                let cache = guard.get_or_insert_with(HashMap::new);
                cache.insert(folder_name.to_string(), id.to_string());

                if let Err(error) = (FolderIdCacheStore {
                    folder_ids: cache.clone(),
                })
                .save()
                {
                    warn!("Failed to persist Bitwarden folder cache (continuing): {error}");
                }

                return Ok(Some(id.to_string()));
            }
        }
        warn!(folder_name, "Bitwarden folder not found");
        Ok(None)
    }

    /// Invalidate the folder ID cache (used in tests).
    #[cfg(all(test, unix))]
    fn invalidate_folder_cache() {
        *FOLDER_ID_CACHE.lock().unwrap() = None;
    }

    #[cfg(test)]
    pub(crate) fn set_test_folder_cache_path(path: Option<PathBuf>) {
        TEST_FOLDER_CACHE_PATH.with(|value| {
            *value.borrow_mut() = path;
        });
    }

    #[cfg(test)]
    pub(crate) fn set_test_sync_state_path(path: Option<PathBuf>) {
        TEST_SYNC_STATE_PATH.with(|value| {
            *value.borrow_mut() = path;
        });
    }

    #[cfg(all(test, unix))]
    pub(crate) fn set_test_sync_throttle_override(sync_throttle_secs: Option<u64>) {
        *SYNC_THROTTLE_OVERRIDE_SECS.lock().unwrap() = sync_throttle_secs;
    }

    #[cfg(all(test, unix))]
    pub(crate) fn set_test_prompt_unlock_password(password: Option<&str>) {
        TEST_PROMPT_UNLOCK_PASSWORD.with(|value| {
            *value.borrow_mut() = password.map(str::to_string);
        });
    }

    fn run_bw(args: &[&str]) -> Result<String> {
        let mut cmd = Self::bw_command()?;
        cmd.args(args);
        cmd.stdin(std::process::Stdio::null());
        let action = format!("bw {}", args.join(" "));
        debug!("Running: {}", action);
        let output = Self::run_timed_command(
            cmd,
            &action,
            "Failed to execute `bw` CLI. Is Bitwarden CLI installed?",
        )?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            debug!(args = args.join(" "), %stderr, "bw command failed");

            // Detect stale session → invalidate, re-auth, and retry once
            if Self::is_stale_session_error(&stderr) {
                warn!("Bitwarden session is stale, re-authenticating");
                Self::invalidate_session();
                let mut retry_cmd = Self::bw_command()?;
                retry_cmd.args(args);
                retry_cmd.stdin(std::process::Stdio::null());
                debug!("Retrying: {}", action);
                let retry_action = format!("{} (retry)", action);
                let retry_output = Self::run_timed_command(
                    retry_cmd,
                    &retry_action,
                    "Failed to execute `bw` CLI on retry",
                )?;
                if !retry_output.status.success() {
                    let retry_stderr = String::from_utf8_lossy(&retry_output.stderr);
                    bail!("bw command failed after re-auth: {retry_stderr}");
                }
                trace!(
                    args = args.join(" "),
                    "bw command succeeded (after re-auth)"
                );
                let stdout = String::from_utf8(retry_output.stdout)
                    .context("bw output was not valid UTF-8")?;
                return Ok(stdout.trim().to_string());
            }

            bail!("bw command failed: {stderr}");
        }
        trace!(args = args.join(" "), "bw command succeeded");
        let stdout = String::from_utf8(output.stdout).context("bw output was not valid UTF-8")?;
        Ok(stdout.trim().to_string())
    }

    /// Parse a bw:// reference: bw://[folder/]item/field
    fn parse_bw_reference(reference: &str) -> Option<(Option<&str>, &str, &str)> {
        let path = reference.strip_prefix("bw://")?;
        let parts: Vec<&str> = path.splitn(3, '/').collect();
        match parts.len() {
            2 => Some((None, parts[0], parts[1])),
            3 => Some((Some(parts[0]), parts[1], parts[2])),
            _ => None,
        }
    }

    /// Get a specific field from a Bitwarden item JSON.
    fn extract_field_from_item(item_json: &str, field_name: &str) -> Result<String> {
        let item: serde_json::Value =
            serde_json::from_str(item_json).context("Failed to parse Bitwarden item JSON")?;
        Self::extract_field_from_value(&item, field_name)
    }

    /// Get a specific field from a parsed Bitwarden item.
    fn extract_field_from_value(item: &serde_json::Value, field_name: &str) -> Result<String> {
        // Check login fields first
        if field_name == "password"
            && let Some(password) = item
                .get("login")
                .and_then(|l| l.get("password"))
                .and_then(|p| p.as_str())
        {
            return Ok(password.to_string());
        }
        if field_name == "username"
            && let Some(username) = item
                .get("login")
                .and_then(|l| l.get("username"))
                .and_then(|u| u.as_str())
        {
            return Ok(username.to_string());
        }

        // Check custom fields
        if let Some(fields) = item.get("fields").and_then(|f| f.as_array()) {
            for f in fields {
                if f.get("name").and_then(|n| n.as_str()) == Some(field_name)
                    && let Some(val) = f.get("value").and_then(|v| v.as_str())
                {
                    return Ok(val.to_string());
                }
            }
        }

        // Check notes
        if field_name == "notes"
            && let Some(notes) = item.get("notes").and_then(|n| n.as_str())
        {
            return Ok(notes.to_string());
        }

        bail!("Field '{field_name}' not found in Bitwarden item");
    }

    fn item_matches_custom_field(
        item: &serde_json::Value,
        field_name: &str,
        expected: &str,
    ) -> bool {
        item.get("fields")
            .and_then(|fields| fields.as_array())
            .is_some_and(|fields| {
                fields.iter().any(|field| {
                    let name = field.get("name").and_then(|value| value.as_str());
                    name.is_some_and(|name| name.eq_ignore_ascii_case(field_name))
                        && field.get("value").and_then(|value| value.as_str()) == Some(expected)
                })
            })
    }

    /// Resolve a key when multiple items share the same name, by narrowing
    /// candidates first by repository, then folder, then the "project" custom field.
    ///
    /// If more than one candidate remains after all filters, a warning is
    /// printed and an empty string is returned so the caller can proceed
    /// without blocking the entire env resolution.
    fn disambiguate_items(
        key: &str,
        repository: Option<&str>,
        folder_id: Option<&str>,
        project: Option<&str>,
    ) -> Result<String> {
        let search_json = Self::run_bw(&["list", "items", "--search", key])?;
        let items: Vec<serde_json::Value> =
            serde_json::from_str(&search_json).context("Failed to parse bw list items JSON")?;

        Self::disambiguate_items_from_list(key, &items, repository, folder_id, project)
    }

    fn resolve_reference_folder_id(
        reference_folder: Option<&str>,
        configured_folder: Option<&str>,
    ) -> Result<Option<String>> {
        reference_folder
            .or(configured_folder)
            .map(Self::resolve_folder_id)
            .transpose()
            .map(|folder_id| folder_id.flatten())
    }

    fn select_item_from_list<'a>(
        key: &str,
        items: &'a [serde_json::Value],
        repository: Option<&str>,
        folder_id: Option<&str>,
        project: Option<&str>,
    ) -> Result<Option<&'a serde_json::Value>> {
        debug!(
            key,
            item_count = items.len(),
            "Search returned {} item(s)",
            items.len()
        );

        // Filter by exact name match (also accept legacy "export KEY" names)
        let mut matching: Vec<&serde_json::Value> = items
            .iter()
            .filter(|item| {
                let name = item.get("name").and_then(|n| n.as_str()).unwrap_or("");
                name == key || name == format!("export {key}")
            })
            .collect();

        if matching.is_empty() {
            if !items.is_empty() {
                let names: Vec<&str> = items
                    .iter()
                    .filter_map(|i| i.get("name").and_then(|n| n.as_str()))
                    .collect();
                debug!(key, ?names, "No exact match among search results");
            }
            bail!("No Bitwarden items found with name '{key}'");
        }

        if matching.len() == 1 {
            return Ok(Some(matching[0]));
        }

        // Multiple matches — try narrowing by repository
        if let Some(repository) = repository {
            info!(
                "Found {} items named '{key}', narrowing by repository '{repository}'",
                matching.len()
            );
            let repository_filtered: Vec<&serde_json::Value> = matching
                .iter()
                .filter(|item| {
                    Self::item_matches_custom_field(item, REPOSITORY_FIELD_NAME, repository)
                })
                .copied()
                .collect();
            if !repository_filtered.is_empty() {
                matching = repository_filtered;
            }
            if matching.len() == 1 {
                debug!("Disambiguated Bitwarden item by repository field '{repository}'");
                return Ok(Some(matching[0]));
            }
        }

        // Multiple matches — try narrowing by folder
        if let Some(fid) = folder_id {
            info!(
                "Found {} items named '{key}', narrowing by folder",
                matching.len()
            );
            let folder_filtered: Vec<&serde_json::Value> = matching
                .iter()
                .filter(|item| item.get("folderId").and_then(|f| f.as_str()) == Some(fid))
                .copied()
                .collect();
            if !folder_filtered.is_empty() {
                matching = folder_filtered;
            }
            if matching.len() == 1 {
                debug!("Disambiguated Bitwarden item by folder for '{key}'");
                return Ok(Some(matching[0]));
            }
        }

        // Still ambiguous — try narrowing by project
        if let Some(proj) = project {
            info!(
                "Found {} items named '{key}', narrowing by project '{proj}'",
                matching.len()
            );
            let project_filtered: Vec<&serde_json::Value> = matching
                .iter()
                .filter(|item| Self::item_matches_custom_field(item, PROJECT_FIELD_NAME, proj))
                .copied()
                .collect();
            if !project_filtered.is_empty() {
                matching = project_filtered;
            }
            if matching.len() == 1 {
                debug!("Disambiguated Bitwarden item by project field '{proj}'");
                return Ok(Some(matching[0]));
            }
        }

        warn!(
            "Multiple Bitwarden items found for '{key}' — could not disambiguate, leaving value blank"
        );
        eprintln!(
            "pw-env: multiple Bitwarden items found for '{key}'. \
             Add a 'repository' or 'project' field, or configure defaults.bw.folder to disambiguate."
        );
        Ok(None)
    }

    fn resolve_reference_field(
        item_name: &str,
        field_name: &str,
        repository: Option<&str>,
        folder_id: Option<&str>,
        project: Option<&str>,
    ) -> Result<String> {
        let Some(item) = Self::resolve_reference_item(item_name, repository, folder_id, project)?
        else {
            return Ok(String::new());
        };

        Self::extract_field_from_value(&item, field_name)
    }

    fn resolve_reference_item(
        item_name: &str,
        repository: Option<&str>,
        folder_id: Option<&str>,
        project: Option<&str>,
    ) -> Result<Option<serde_json::Value>> {
        let search_json = Self::run_bw(&["list", "items", "--search", item_name])?;
        let items: Vec<serde_json::Value> =
            serde_json::from_str(&search_json).context("Failed to parse bw list items JSON")?;

        let Some(item) =
            Self::select_item_from_list(item_name, &items, repository, folder_id, project)?
        else {
            return Ok(None);
        };

        let item_identifier = item
            .get("id")
            .and_then(|id| id.as_str())
            .unwrap_or(item_name);
        let item_json = Self::run_bw(&["get", "item", item_identifier])?;
        let item: serde_json::Value =
            serde_json::from_str(&item_json).context("Failed to parse Bitwarden item JSON")?;
        Ok(Some(item))
    }

    /// Core disambiguation logic that works on a pre-fetched list of items.
    /// Used by both the single-key `disambiguate_items` and the batch resolver.
    fn disambiguate_items_from_list(
        key: &str,
        items: &[serde_json::Value],
        repository: Option<&str>,
        folder_id: Option<&str>,
        project: Option<&str>,
    ) -> Result<String> {
        let Some(item) = Self::select_item_from_list(key, items, repository, folder_id, project)?
        else {
            return Ok(String::new());
        };

        Self::extract_field_from_value(item, "password")
    }

    /// Batch-resolve multiple Bitwarden entries with minimal CLI calls.
    ///
    /// - **Item mode** (when `effective_item()` returns `Some`): a single `bw get item`
    ///   call fetches the configured item, then all requested keys are extracted as
    ///   custom fields from the cached JSON.
    /// - **Item-per-key mode**: a single `bw list items` call fetches all items, then
    ///   each key is disambiguated in-process using repository, folder, and project metadata.
    /// - **bw:// references**: grouped by item name so each unique item is fetched once.
    ///
    /// Returns a map of key → resolved value. Keys that fail to resolve are omitted
    /// (the caller logs warnings per key).
    pub fn resolve_batch(
        keys: &[(&str, Option<&str>)],
        ctx: &ResolveContext,
    ) -> BTreeMap<String, Result<String>> {
        Self::configure_sync_throttle(ctx.config.effective_bw(ctx.dir).sync_throttle_secs);
        let started_at = Instant::now();
        let mut retried_after_sync = false;

        let mut results = Self::resolve_batch_once(keys, ctx);
        if results
            .values()
            .filter_map(|result| result.as_ref().err())
            .any(Self::should_retry_after_sync)
        {
            warn!(
                "Bitwarden batch resolve failed for at least one lookup, forcing sync and retrying once"
            );
            Self::force_sync_vault();
            results = Self::resolve_batch_once(keys, ctx);
            retried_after_sync = true;
        }

        let success_count = results.values().filter(|result| result.is_ok()).count();
        let failure_count = results.len().saturating_sub(success_count);
        debug!(
            duration_ms = started_at.elapsed().as_millis(),
            total_entries = keys.len(),
            success_count,
            failure_count,
            item_mode = ctx.config.effective_item(ctx.dir).is_some(),
            retried_after_sync,
            "Bitwarden batch resolve finished"
        );

        results
    }

    fn resolve_batch_once(
        keys: &[(&str, Option<&str>)],
        ctx: &ResolveContext,
    ) -> BTreeMap<String, Result<String>> {
        let bw_config = ctx.config.effective_bw(ctx.dir);
        let mut results: BTreeMap<String, Result<String>> = BTreeMap::new();

        // Separate bw:// references from key-based lookups
        let mut ref_entries: Vec<(&str, &str)> = Vec::new(); // (key, reference)
        let mut key_entries: Vec<&str> = Vec::new();

        for &(key, reference) in keys {
            if let Some(ref_str) = reference
                && ref_str.starts_with("bw://")
            {
                ref_entries.push((key, ref_str));
                continue;
            }
            key_entries.push(key);
        }

        // --- Handle bw:// references: group by item name, one fetch per unique item ---
        if !ref_entries.is_empty() {
            let mut reference_item_cache: HashMap<
                (Option<String>, String),
                std::result::Result<Option<serde_json::Value>, String>,
            > = HashMap::new();

            for (env_key, ref_str) in &ref_entries {
                if let Some((reference_folder, item_name, field_name)) =
                    Self::parse_bw_reference(ref_str)
                {
                    let folder_id = Self::resolve_reference_folder_id(
                        reference_folder,
                        bw_config.folder.as_deref(),
                    );
                    results.insert(
                        env_key.to_string(),
                        match folder_id {
                            Ok(folder_id) => {
                                let cache_key = (folder_id.clone(), item_name.to_string());
                                let folder_id_for_lookup = folder_id.clone();
                                let cached_item =
                                    reference_item_cache.entry(cache_key).or_insert_with(|| {
                                        Self::resolve_reference_item(
                                            item_name,
                                            ctx.repository.as_deref(),
                                            folder_id_for_lookup.as_deref(),
                                            ctx.project.as_deref(),
                                        )
                                        .map_err(|error| error.to_string())
                                    });

                                match cached_item.as_ref() {
                                    Ok(Some(item)) => {
                                        Self::extract_field_from_value(item, field_name)
                                    }
                                    Ok(None) => Ok(String::new()),
                                    Err(error) => Err(anyhow::anyhow!(error.clone())),
                                }
                            }
                            Err(error) => Err(error),
                        },
                    );
                } else {
                    results.insert(
                        env_key.to_string(),
                        Err(anyhow::anyhow!(
                            "Invalid bw:// reference format: {ref_str}. Expected bw://[folder/]item/field"
                        )),
                    );
                }
            }
        }

        // --- Handle key-based lookups ---
        if !key_entries.is_empty() {
            if let Some(item_name) = ctx.config.effective_item(ctx.dir) {
                // Item mode: single fetch, extract all keys as fields
                debug!(
                    "Batch resolving {} keys as fields on Bitwarden item '{item_name}'",
                    key_entries.len()
                );
                match Self::run_bw(&["get", "item", item_name]) {
                    Ok(item_json) => {
                        let item: std::result::Result<serde_json::Value, _> =
                            serde_json::from_str(&item_json);
                        match item {
                            Ok(item_val) => {
                                for key in &key_entries {
                                    results.insert(
                                        key.to_string(),
                                        Self::extract_field_from_value(&item_val, key),
                                    );
                                }
                            }
                            Err(e) => {
                                for key in &key_entries {
                                    results.insert(
                                        key.to_string(),
                                        Err(anyhow::anyhow!(
                                            "Failed to parse Bitwarden item JSON: {e}"
                                        )),
                                    );
                                }
                            }
                        }
                    }
                    Err(e) => {
                        for key in &key_entries {
                            results.insert(
                                key.to_string(),
                                Err(anyhow::anyhow!(
                                    "Failed to fetch Bitwarden item '{item_name}': {e}"
                                )),
                            );
                        }
                    }
                }
            } else {
                // Item-per-key mode: single list call, disambiguate in-process
                debug!(
                    "Batch resolving {} keys via Bitwarden item list",
                    key_entries.len()
                );
                let folder_id: Option<String> = bw_config
                    .folder
                    .as_deref()
                    .map(Self::resolve_folder_id)
                    .transpose()
                    .ok()
                    .flatten()
                    .flatten();

                match Self::run_bw(&["list", "items"]) {
                    Ok(items_json) => {
                        let all_items: Vec<serde_json::Value> =
                            serde_json::from_str(&items_json).unwrap_or_default();
                        for key in &key_entries {
                            results.insert(
                                key.to_string(),
                                Self::disambiguate_items_from_list(
                                    key,
                                    &all_items,
                                    ctx.repository.as_deref(),
                                    folder_id.as_deref(),
                                    ctx.project.as_deref(),
                                ),
                            );
                        }
                    }
                    Err(e) => {
                        for key in &key_entries {
                            results.insert(
                                key.to_string(),
                                Err(anyhow::anyhow!("Failed to list Bitwarden items: {e}")),
                            );
                        }
                    }
                }
            }
        }

        results
    }

    fn resolve_once(key: &str, reference: Option<&str>, ctx: &ResolveContext) -> Result<String> {
        debug!(key, reference, "Resolving Bitwarden secret");
        let bw_config = ctx.config.effective_bw(ctx.dir);

        // Handle bw:// references
        if let Some(ref_str) = reference
            && ref_str.starts_with("bw://")
        {
            if let Some((reference_folder, item, field)) = Self::parse_bw_reference(ref_str) {
                debug!("Resolving Bitwarden reference: {ref_str}");
                let folder_id = Self::resolve_reference_folder_id(
                    reference_folder,
                    bw_config.folder.as_deref(),
                )?;
                return Self::resolve_reference_field(
                    item,
                    field,
                    ctx.repository.as_deref(),
                    folder_id.as_deref(),
                    ctx.project.as_deref(),
                );
            } else {
                bail!(
                    "Invalid bw:// reference format: {ref_str}. Expected bw://[folder/]item/field"
                );
            }
        }

        // Key-based lookup
        if let Some(item) = ctx.config.effective_item(ctx.dir) {
            // Look up key as a custom field on the configured item
            debug!("Resolving key '{key}' as field on Bitwarden item '{item}'");
            let item_json = Self::run_bw(&["get", "item", item])?;
            Self::extract_field_from_item(&item_json, key)
        } else {
            // Look up the key as a password item using list + exact-name filter
            // (`bw get password` and `bw get item` do substring matching, which
            // can return the wrong entry when item names are similar.)
            debug!("Resolving key '{key}' as Bitwarden item password");
            let folder_id: Option<String> = bw_config
                .folder
                .as_deref()
                .map(Self::resolve_folder_id)
                .transpose()?
                .flatten();
            Self::disambiguate_items(
                key,
                ctx.repository.as_deref(),
                folder_id.as_deref(),
                ctx.project.as_deref(),
            )
        }
    }
}

impl Backend for BwBackend {
    fn resolve(&self, key: &str, reference: Option<&str>, ctx: &ResolveContext) -> Result<String> {
        Self::configure_sync_throttle(ctx.config.effective_bw(ctx.dir).sync_throttle_secs);

        match Self::resolve_once(key, reference, ctx) {
            Ok(value) => Ok(value),
            Err(error) if Self::should_retry_after_sync(&error) => {
                warn!(
                    key,
                    "Bitwarden lookup failed, forcing sync and retrying once"
                );
                Self::force_sync_vault();
                Self::resolve_once(key, reference, ctx)
            }
            Err(error) => Err(error),
        }
    }

    fn store(&self, key: &str, value: &str, ctx: &StoreContext) -> Result<()> {
        Self::configure_sync_throttle(ctx.config.effective_bw(ctx.dir).sync_throttle_secs);
        debug!(key, "Storing secret in Bitwarden");
        let bw_config = ctx.config.effective_bw(ctx.dir);
        let metadata_fields = Self::migration_metadata_fields(ctx);

        if let Some(item_name) = ctx.config.effective_item(ctx.dir) {
            // Try to get existing item and add a custom field
            debug!("Storing key '{key}' as custom field on Bitwarden item '{item_name}'");
            let item_result = Self::run_bw(&["get", "item", item_name]);
            if let Ok(item_json) = item_result {
                let mut item: serde_json::Value = serde_json::from_str(&item_json)?;
                Self::upsert_custom_field(&mut item, key, value, 1);
                for field in &metadata_fields {
                    if let (Some(name), Some(field_value), Some(field_type)) = (
                        field.get("name").and_then(|field_name| field_name.as_str()),
                        field
                            .get("value")
                            .and_then(|field_value| field_value.as_str()),
                        field.get("type").and_then(|field_type| field_type.as_u64()),
                    ) {
                        Self::upsert_custom_field(&mut item, name, field_value, field_type as u8);
                    }
                }
                let encoded = serde_json::to_string(&item)?;
                // bw edit item expects base64-encoded JSON on stdin, but we use a simpler approach
                let mut cmd = Self::bw_command()?;
                cmd.args(["edit", "item", item_name]);
                cmd.stdin(std::process::Stdio::piped());
                cmd.stdout(std::process::Stdio::piped());
                cmd.stderr(std::process::Stdio::piped());
                let started_at = Instant::now();
                let mut child = cmd.spawn()?;
                if let Some(mut stdin) = child.stdin.take() {
                    use std::io::Write;
                    // bw expects base64-encoded JSON
                    let encoded_b64 = base64_encode(encoded.as_bytes());
                    stdin.write_all(encoded_b64.as_bytes())?;
                }
                let output = child.wait_with_output()?;
                Self::log_action_timing(
                    &format!("bw edit item {item_name}"),
                    started_at,
                    output.status.success(),
                );
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    bail!("bw edit failed: {stderr}");
                }
                info!(
                    key,
                    item_name, "Updated custom field on existing Bitwarden item"
                );
                return Ok(());
            }
        }

        // Create a new login item
        debug!("Creating new Bitwarden item for key '{key}'");
        let folder_id: Option<String> = bw_config
            .folder
            .as_deref()
            .map(Self::resolve_folder_id)
            .transpose()?
            .flatten();
        let item_template = serde_json::json!({
            "type": 1,
            "name": key,
            "login": {
                "password": value
            },
            "folderId": folder_id,
            "fields": metadata_fields
        });
        let encoded = serde_json::to_string(&item_template)?;
        let encoded_b64 = base64_encode(encoded.as_bytes());

        let mut cmd = Self::bw_command()?;
        cmd.args(["create", "item"]);
        cmd.stdin(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        let started_at = Instant::now();
        let mut child = cmd.spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            stdin.write_all(encoded_b64.as_bytes())?;
        }
        let output = child.wait_with_output()?;
        Self::log_action_timing("bw create item", started_at, output.status.success());
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("bw create failed: {stderr}");
        }
        info!(key, "Created new Bitwarden item");
        Ok(())
    }

    fn has(&self, key: &str, ctx: &ResolveContext) -> Result<bool> {
        Self::configure_sync_throttle(ctx.config.effective_bw(ctx.dir).sync_throttle_secs);
        match self.resolve(key, None, ctx) {
            Ok(_) => Ok(true),
            Err(_) => Ok(false),
        }
    }

    fn reference_url(&self, key: &str, ctx: &StoreContext) -> Option<String> {
        let bw_config = ctx.config.effective_bw(ctx.dir);
        let (item_name, field_name) = if let Some(item) = ctx.config.effective_item(ctx.dir) {
            (item.to_string(), key.to_string())
        } else {
            (key.to_string(), "password".to_string())
        };
        if let Some(folder) = &bw_config.folder {
            Some(format!("bw://{folder}/{item_name}/{field_name}"))
        } else {
            Some(format!("bw://{item_name}/{field_name}"))
        }
    }

    fn name(&self) -> &str {
        "Bitwarden"
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::config::{BwConfig, Config, Defaults, LogConfig, UpdateConfig};
    use std::cell::Cell;
    use std::io::Write;
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    fn capture_debug_output(f: impl FnOnce()) -> String {
        struct BufWriter(Arc<Mutex<Vec<u8>>>);

        impl Write for BufWriter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        struct BufMakeWriter(Arc<Mutex<Vec<u8>>>);

        impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for BufMakeWriter {
            type Writer = BufWriter;

            fn make_writer(&'a self) -> Self::Writer {
                BufWriter(self.0.clone())
            }
        }

        let buf = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::fmt()
            .with_writer(BufMakeWriter(buf.clone()))
            .with_max_level(tracing::Level::DEBUG)
            .without_time()
            .with_ansi(false)
            .finish();

        tracing::subscriber::with_default(subscriber, f);

        String::from_utf8(buf.lock().unwrap().clone()).unwrap()
    }

    #[test]
    fn test_parse_bw_reference_two_parts() {
        let result = BwBackend::parse_bw_reference("bw://item/field");
        assert_eq!(result, Some((None, "item", "field")));
    }

    #[test]
    fn test_parse_bw_reference_three_parts() {
        let result = BwBackend::parse_bw_reference("bw://folder/item/field");
        assert_eq!(result, Some((Some("folder"), "item", "field")));
    }

    #[test]
    fn test_parse_bw_reference_invalid_only_one_part() {
        let result = BwBackend::parse_bw_reference("bw://item");
        assert_eq!(result, None);
    }

    #[test]
    fn test_parse_bw_reference_no_prefix() {
        let result = BwBackend::parse_bw_reference("item/field");
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_field_from_value_password() {
        let item = serde_json::json!({
            "login": { "password": "secret123" }
        });
        let result = BwBackend::extract_field_from_value(&item, "password");
        assert_eq!(result.unwrap(), "secret123");
    }

    #[test]
    fn test_extract_field_from_value_username() {
        let item = serde_json::json!({
            "login": { "username": "user@example.com" }
        });
        let result = BwBackend::extract_field_from_value(&item, "username");
        assert_eq!(result.unwrap(), "user@example.com");
    }

    #[test]
    fn test_extract_field_from_value_custom_field() {
        let item = serde_json::json!({
            "fields": [
                { "name": "API_KEY", "value": "abc123", "type": 1 }
            ]
        });
        let result = BwBackend::extract_field_from_value(&item, "API_KEY");
        assert_eq!(result.unwrap(), "abc123");
    }

    #[test]
    fn test_extract_field_from_value_notes() {
        let item = serde_json::json!({
            "notes": "some secure notes"
        });
        let result = BwBackend::extract_field_from_value(&item, "notes");
        assert_eq!(result.unwrap(), "some secure notes");
    }

    #[test]
    fn test_extract_field_from_value_missing() {
        let item = serde_json::json!({});
        let result = BwBackend::extract_field_from_value(&item, "password");
        assert!(result.is_err());
    }

    #[test]
    fn test_extract_field_from_item_valid_json() {
        let item_json = r#"{"login": {"password": "mypass"}}"#;
        let result = BwBackend::extract_field_from_item(item_json, "password");
        assert_eq!(result.unwrap(), "mypass");
    }

    #[test]
    fn test_extract_field_from_item_invalid_json() {
        let result = BwBackend::extract_field_from_item("not valid json", "password");
        assert!(result.is_err());
    }

    #[test]
    fn test_base64_encode_known_value() {
        // "Man" encodes to "TWFu" in standard base64
        assert_eq!(base64_encode(b"Man"), "TWFu");
    }

    #[test]
    fn test_base64_encode_empty() {
        assert_eq!(base64_encode(b""), "");
    }

    #[test]
    fn test_base64_encode_single_byte() {
        // "M" encodes to "TQ==" in standard base64
        assert_eq!(base64_encode(b"M"), "TQ==");
    }

    #[test]
    fn test_upsert_custom_field_adds_new_field() {
        let mut item = serde_json::json!({ "fields": [] });
        BwBackend::upsert_custom_field(&mut item, "api_key", "value123", 0);
        let fields = item.get("fields").and_then(|v| v.as_array()).unwrap();
        assert_eq!(fields.len(), 1);
        assert_eq!(
            fields[0].get("name").and_then(|v| v.as_str()),
            Some("api_key")
        );
        assert_eq!(
            fields[0].get("value").and_then(|v| v.as_str()),
            Some("value123")
        );
    }

    #[test]
    fn test_upsert_custom_field_creates_fields_array_when_absent() {
        let mut item = serde_json::json!({});
        BwBackend::upsert_custom_field(&mut item, "key", "val", 0);
        let fields = item.get("fields").and_then(|v| v.as_array()).unwrap();
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].get("name").and_then(|v| v.as_str()), Some("key"));
    }

    fn test_store_context() -> StoreContext<'static> {
        let config = Box::leak(Box::new(Config {
            defaults: Defaults::default(),
            log: LogConfig::default(),
            updates: UpdateConfig::default(),
            projects: vec![],
        }));

        StoreContext {
            dir: Path::new("/tmp/example/service"),
            config,
            project: Some("example".to_string()),
            repository: Some("git@github.com:example/example.git".to_string()),
        }
    }

    // ------- Mock-bw infrastructure -------

    fn with_mock_bw<F: FnOnce()>(script: &str, f: F) {
        let _guard = super::super::MOCK_PATH_MUTEX
            .lock()
            .unwrap_or_else(|p| p.into_inner());

        // Reset the cached session and folder IDs so each test starts fresh
        *super::SESSION.lock().unwrap() = None;
        BwBackend::invalidate_folder_cache();
        BwBackend::set_test_folder_cache_path(None);
        BwBackend::set_test_sync_state_path(None);
        BwBackend::set_test_sync_throttle_override(None);
        BwBackend::set_test_prompt_unlock_password(None);

        let dir = tempfile::TempDir::new().unwrap();
        let script_path = dir.path().join("bw");
        std::fs::write(&script_path, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&script_path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&script_path, perms).unwrap();
        }
        let old_path = std::env::var_os("PATH").unwrap_or_default();
        let new_path = std::env::join_paths(
            std::iter::once(dir.path().to_path_buf()).chain(std::env::split_paths(&old_path)),
        )
        .unwrap();
        let old_session = std::env::var_os("BW_SESSION");
        // SAFETY: guarded by MOCK_PATH_MUTEX, single-threaded access to env here
        unsafe {
            std::env::set_var("PATH", &new_path);
            std::env::set_var("BW_SESSION", "mock-session");
        }
        f();
        unsafe {
            std::env::set_var("PATH", &old_path);
            match old_session {
                Some(val) => std::env::set_var("BW_SESSION", val),
                None => std::env::remove_var("BW_SESSION"),
            }
        }
        // Reset session and folder cache after test too
        *super::SESSION.lock().unwrap() = None;
        BwBackend::invalidate_folder_cache();
        BwBackend::set_test_folder_cache_path(None);
        BwBackend::set_test_sync_state_path(None);
        BwBackend::set_test_sync_throttle_override(None);
        BwBackend::set_test_prompt_unlock_password(None);
    }

    #[test]
    fn with_mock_bw_executes_callback() {
        let called = Cell::new(false);
        with_mock_bw("#!/bin/sh\nexit 0\n", || {
            called.set(true);
        });
        assert!(called.get(), "expected callback to be executed");
    }

    #[test]
    fn configure_sync_throttle_updates_effective_value() {
        BwBackend::set_test_sync_throttle_override(None);
        assert_eq!(
            BwBackend::effective_sync_throttle_secs(),
            DEFAULT_SYNC_THROTTLE_SECS
        );

        BwBackend::configure_sync_throttle(7200);
        assert_eq!(BwBackend::effective_sync_throttle_secs(), 7200);

        BwBackend::configure_sync_throttle(30);
        assert_eq!(
            BwBackend::effective_sync_throttle_secs(),
            MIN_SYNC_THROTTLE_SECS
        );

        BwBackend::set_test_sync_throttle_override(None);
    }

    #[test]
    fn set_test_sync_throttle_override_updates_effective_value() {
        BwBackend::set_test_sync_throttle_override(None);
        assert_eq!(
            BwBackend::effective_sync_throttle_secs(),
            DEFAULT_SYNC_THROTTLE_SECS
        );

        BwBackend::set_test_sync_throttle_override(Some(7200));
        assert_eq!(BwBackend::effective_sync_throttle_secs(), 7200);

        BwBackend::set_test_sync_throttle_override(Some(30));
        assert_eq!(
            BwBackend::effective_sync_throttle_secs(),
            MIN_SYNC_THROTTLE_SECS
        );

        BwBackend::set_test_sync_throttle_override(None);
        assert_eq!(
            BwBackend::effective_sync_throttle_secs(),
            DEFAULT_SYNC_THROTTLE_SECS
        );
    }

    #[test]
    fn should_retry_after_sync_matches_expected_patterns() {
        for message in [
            "No Bitwarden items found with name 'KEY'",
            "Field 'token' not found in Bitwarden item",
            "Failed to fetch Bitwarden item 'my-item'",
            "Failed to list Bitwarden items: boom",
            "Object not found",
        ] {
            let error = anyhow::anyhow!(message);
            assert!(
                BwBackend::should_retry_after_sync(&error),
                "expected retryable error: {message}"
            );
        }

        let error = anyhow::anyhow!("permission denied");
        assert!(!BwBackend::should_retry_after_sync(&error));
    }

    #[test]
    fn should_retry_after_sync_requires_both_field_markers() {
        for message in [
            "Field 'token' exists but retrieval timed out",
            "Credential not found in Bitwarden item metadata",
        ] {
            let error = anyhow::anyhow!(message);
            assert!(
                !BwBackend::should_retry_after_sync(&error),
                "expected non-retryable error: {message}"
            );
        }
    }

    #[test]
    fn log_action_timing_emits_debug_log() {
        let output = capture_debug_output(|| {
            BwBackend::log_action_timing("bw status", Instant::now(), true);
        });

        assert!(output.contains("Bitwarden action finished"));
        assert!(output.contains("bw status"));
        assert!(output.contains("success=true"));
        assert!(output.contains("duration_ms="));
    }

    #[test]
    fn current_unix_secs_is_close_to_system_time() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let observed = BwBackend::current_unix_secs().expect("expected unix timestamp");
        assert!(observed <= now.saturating_add(2));
        assert!(observed.saturating_add(2) >= now);
    }

    #[test]
    fn test_paths_use_test_overrides() {
        let state_dir = tempfile::TempDir::new().unwrap();
        let folder_path = state_dir.path().join("folder-cache.json");
        let sync_path = state_dir.path().join("sync-state.json");

        BwBackend::set_test_folder_cache_path(Some(folder_path.clone()));
        BwBackend::set_test_sync_state_path(Some(sync_path.clone()));

        assert_eq!(
            BwBackend::folder_cache_path().as_deref(),
            Some(folder_path.as_path())
        );
        assert_eq!(
            BwBackend::sync_state_path().as_deref(),
            Some(sync_path.as_path())
        );

        BwBackend::set_test_folder_cache_path(None);
        BwBackend::set_test_sync_state_path(None);
    }

    #[test]
    fn sync_state_store_save_persists_contents() {
        let state_dir = tempfile::TempDir::new().unwrap();
        let sync_path = state_dir.path().join("pw-env").join(SYNC_STATE_FILE_NAME);
        BwBackend::set_test_sync_state_path(Some(sync_path.clone()));

        SyncStateStore {
            last_sync_unix_secs: Some(12345),
        }
        .save()
        .unwrap();

        let persisted: SyncStateStore =
            serde_json::from_str(&std::fs::read_to_string(&sync_path).unwrap()).unwrap();
        assert_eq!(persisted.last_sync_unix_secs, Some(12345));

        BwBackend::set_test_sync_state_path(None);
    }

    #[test]
    fn is_stale_session_error_detects_expected_messages() {
        assert!(BwBackend::is_stale_session_error("Invalid master password"));
        assert!(BwBackend::is_stale_session_error("Session key is invalid"));
        assert!(BwBackend::is_stale_session_error("Not logged in"));
        assert!(!BwBackend::is_stale_session_error("network timeout"));
    }

    #[test]
    fn invalidate_session_clears_cached_value() {
        *super::SESSION.lock().unwrap() = Some("session-token".to_string());
        BwBackend::invalidate_session();
        assert!(super::SESSION.lock().unwrap().is_none());
    }

    #[test]
    fn ensure_session_uses_bw_session_from_environment() {
        let state_dir = tempfile::TempDir::new().unwrap();
        let call_log = state_dir.path().join("bw-calls.log");
        let sync_state_path = state_dir.path().join("pw-env").join(SYNC_STATE_FILE_NAME);
        let script = format!(
            "#!/bin/sh\necho \"$@\" >> '{}'\nexit 0\n",
            call_log.display()
        );

        with_mock_bw(&script, || {
            *super::SESSION.lock().unwrap() = None;
            BwBackend::set_test_sync_state_path(Some(sync_state_path.clone()));
            // SAFETY: guarded by MOCK_PATH_MUTEX in with_mock_bw
            unsafe {
                std::env::set_var("BW_SESSION", "session-from-env");
            }

            let session = BwBackend::ensure_session().unwrap();
            assert_eq!(session, "session-from-env");

            // A sync should still be triggered when using an env-provided session.
            let log = std::fs::read_to_string(&call_log).unwrap();
            assert!(log.lines().any(|line| line == "sync"));
        });
    }

    #[test]
    fn run_bw_does_not_inject_empty_bw_session_env() {
        let script = "#!/bin/sh\nif [ \"$1\" = \"status\" ]; then\n  echo '{\"status\":\"unlocked\"}'\n  exit 0\nfi\nif printenv BW_SESSION >/dev/null 2>&1; then\n  echo 'BW_SESSION should not be present for unlocked status' >&2\n  exit 1\nfi\necho 'ok'\n";

        with_mock_bw(script, || {
            *super::SESSION.lock().unwrap() = None;
            // SAFETY: guarded by MOCK_PATH_MUTEX in with_mock_bw
            unsafe {
                std::env::remove_var("BW_SESSION");
            }

            let output = BwBackend::run_bw(&["list", "items"]).unwrap();
            assert_eq!(output, "ok");
        });
    }

    fn make_bw_item_json(password: &str) -> String {
        format!(r#"{{"type":1,"name":"test-item","login":{{"password":"{password}"}}}}"#)
    }

    fn make_resolve_context<'a>(
        config: &'a Config,
        dir: &'a Path,
    ) -> super::super::ResolveContext<'a> {
        super::super::ResolveContext {
            dir,
            config,
            project: Some("test-project".to_string()),
            repository: Some("git@github.com:example/test-repo.git".to_string()),
        }
    }

    #[test]
    fn run_bw_returns_stdout_on_success() {
        with_mock_bw("#!/bin/sh\necho 'hello-value'\n", || {
            let result = BwBackend::run_bw(&["any", "arg"]);
            assert_eq!(result.unwrap(), "hello-value");
        });
    }

    #[test]
    fn run_bw_returns_err_on_non_zero_exit() {
        with_mock_bw("#!/bin/sh\necho 'error output' >&2\nexit 1\n", || {
            let result = BwBackend::run_bw(&["any", "arg"]);
            assert!(result.is_err());
        });
    }

    #[test]
    fn run_bw_retries_once_on_stale_session_and_returns_retry_output() {
        let state_dir = tempfile::TempDir::new().unwrap();
        let marker = state_dir.path().join("retry-marker");
        let script = format!(
            "#!/bin/sh\nif [ \"$1\" = \"status\" ]; then\n  echo '{{\"status\":\"unlocked\"}}'\n  exit 0\nfi\nif [ ! -f '{}' ]; then\n  touch '{}'\n  echo 'Session key is invalid' >&2\n  exit 1\nfi\necho 'retry-ok'\n",
            marker.display(),
            marker.display()
        );

        with_mock_bw(&script, || {
            let result = BwBackend::run_bw(&["list", "items"]);
            assert_eq!(result.unwrap(), "retry-ok");
        });
    }

    #[test]
    fn prompt_unlock_runs_bw_unlock_and_returns_session() {
        let state_dir = tempfile::TempDir::new().unwrap();
        let args_log = state_dir.path().join("bw-args.log");
        let password_log = state_dir.path().join("bw-password.log");
        let script = format!(
            "#!/bin/sh\necho \"$@\" > '{}'\nprintf '%s' \"$BW_MASTER_PW\" > '{}'\necho 'session-from-bw'\n",
            args_log.display(),
            password_log.display()
        );

        with_mock_bw(&script, || {
            BwBackend::set_test_prompt_unlock_password(Some("correct horse battery staple"));

            let session = BwBackend::prompt_unlock().unwrap();

            assert_eq!(session, "session-from-bw");
            assert_eq!(
                std::fs::read_to_string(&args_log).unwrap().trim(),
                "unlock --raw --passwordenv BW_MASTER_PW"
            );
            assert_eq!(
                std::fs::read_to_string(&password_log).unwrap(),
                "correct horse battery staple"
            );
        });
    }

    #[test]
    fn sync_vault_runs_sync_when_state_is_stale() {
        let state_dir = tempfile::TempDir::new().unwrap();
        let sync_state_path = state_dir.path().join("pw-env").join(SYNC_STATE_FILE_NAME);
        std::fs::create_dir_all(sync_state_path.parent().unwrap()).unwrap();
        let now = BwBackend::current_unix_secs().unwrap();
        std::fs::write(
            &sync_state_path,
            format!("{{\"last_sync_unix_secs\":{}}}", now.saturating_sub(7200)),
        )
        .unwrap();

        let call_log = state_dir.path().join("bw-sync-calls.log");
        let script = format!(
            "#!/bin/sh\necho \"$@\" >> '{}'\nexit 0\n",
            call_log.display()
        );

        with_mock_bw(&script, || {
            BwBackend::set_test_sync_state_path(Some(sync_state_path.clone()));
            BwBackend::set_test_sync_throttle_override(Some(MIN_SYNC_THROTTLE_SECS));
            BwBackend::sync_vault();

            let log = std::fs::read_to_string(&call_log).unwrap();
            assert!(
                log.lines().any(|line| line == "sync"),
                "expected a sync call when state is stale"
            );
        });
    }

    #[test]
    fn sync_vault_runs_sync_when_age_equals_throttle() {
        let state_dir = tempfile::TempDir::new().unwrap();
        let sync_state_path = state_dir.path().join("pw-env").join(SYNC_STATE_FILE_NAME);
        std::fs::create_dir_all(sync_state_path.parent().unwrap()).unwrap();
        let now = BwBackend::current_unix_secs().unwrap();
        std::fs::write(
            &sync_state_path,
            format!(
                "{{\"last_sync_unix_secs\":{}}}",
                now.saturating_sub(MIN_SYNC_THROTTLE_SECS)
            ),
        )
        .unwrap();

        let call_log = state_dir.path().join("bw-sync-calls.log");
        let script = format!(
            "#!/bin/sh\necho \"$@\" >> '{}'\nexit 0\n",
            call_log.display()
        );

        with_mock_bw(&script, || {
            BwBackend::set_test_sync_state_path(Some(sync_state_path.clone()));
            BwBackend::set_test_sync_throttle_override(Some(MIN_SYNC_THROTTLE_SECS));
            BwBackend::sync_vault();

            let log = std::fs::read_to_string(&call_log).unwrap();
            assert!(
                log.lines().any(|line| line == "sync"),
                "expected a sync call when age equals throttle"
            );
        });
    }

    #[test]
    fn resolve_reference_folder_id_prefers_reference_folder() {
        let script = "#!/bin/sh\nif [ \"$1\" = \"list\" ] && [ \"$2\" = \"folders\" ] && [ \"$4\" = \"ref-folder\" ]; then\n  echo '[{\"name\":\"ref-folder\",\"id\":\"folder-ref\"}]'\n  exit 0\nfi\nif [ \"$1\" = \"list\" ] && [ \"$2\" = \"folders\" ] && [ \"$4\" = \"cfg-folder\" ]; then\n  echo '[{\"name\":\"cfg-folder\",\"id\":\"folder-cfg\"}]'\n  exit 0\nfi\nexit 1\n";

        with_mock_bw(script, || {
            let result =
                BwBackend::resolve_reference_folder_id(Some("ref-folder"), Some("cfg-folder"))
                    .unwrap();
            assert_eq!(result.as_deref(), Some("folder-ref"));

            let configured_only =
                BwBackend::resolve_reference_folder_id(None, Some("cfg-folder")).unwrap();
            assert_eq!(configured_only.as_deref(), Some("folder-cfg"));

            let none = BwBackend::resolve_reference_folder_id(None, None).unwrap();
            assert!(none.is_none());
        });
    }

    #[test]
    fn select_item_from_list_errors_when_no_exact_name() {
        let items = vec![serde_json::json!({"name":"OTHER_KEY"})];
        let result = BwBackend::select_item_from_list("MY_KEY", &items, None, None, None);
        assert!(result.is_err());
    }

    #[test]
    fn select_item_from_list_does_not_log_available_names_for_empty_results() {
        let output = capture_debug_output(|| {
            let result = BwBackend::select_item_from_list("MY_KEY", &[], None, None, None);
            assert!(result.is_err());
        });

        assert!(!output.contains("No exact match among search results"));
    }

    #[test]
    fn select_item_from_list_returns_single_repository_match() {
        let items = vec![
            serde_json::json!({
                "name": "API_KEY",
                "fields": [
                    {"name":"repository","value":"git@github.com:example/test-repo.git","type":0}
                ]
            }),
            serde_json::json!({
                "name": "API_KEY",
                "fields": [
                    {"name":"repository","value":"git@github.com:example/other.git","type":0}
                ]
            }),
        ];

        let selected = BwBackend::select_item_from_list(
            "API_KEY",
            &items,
            Some("git@github.com:example/test-repo.git"),
            None,
            None,
        )
        .unwrap()
        .unwrap();

        assert_eq!(
            selected.get("fields").unwrap()[0].get("value").unwrap(),
            "git@github.com:example/test-repo.git"
        );
    }

    #[test]
    fn resolve_does_not_force_sync_for_non_retryable_errors() {
        let state_dir = tempfile::TempDir::new().unwrap();
        let call_log = state_dir.path().join("bw-calls.log");
        let script = format!(
            "#!/bin/sh\necho \"$@\" >> '{}'\nif [ \"$1\" = \"list\" ] && [ \"$2\" = \"items\" ]; then\n  echo 'permission denied' >&2\n  exit 1\nfi\nif [ \"$1\" = \"sync\" ]; then\n  echo 'unexpected sync' >&2\n  exit 1\nfi\nexit 1\n",
            call_log.display()
        );

        with_mock_bw(&script, || {
            let config = Config {
                defaults: Defaults::default(),
                log: LogConfig::default(),
                updates: UpdateConfig::default(),
                projects: vec![],
            };
            let ctx = make_resolve_context(&config, std::path::Path::new("/tmp"));
            let backend = BwBackend;
            let result = backend.resolve("MY_KEY", None, &ctx);
            assert!(result.is_err());

            let log = std::fs::read_to_string(&call_log).unwrap_or_default();
            assert!(
                !log.lines().any(|line| line == "sync"),
                "did not expect a forced sync for non-retryable errors"
            );
        });
    }

    #[test]
    fn backend_store_returns_err_when_bw_create_fails() {
        with_mock_bw("#!/bin/sh\nexit 1\n", || {
            let config = Config {
                defaults: Defaults::default(),
                log: LogConfig::default(),
                updates: UpdateConfig::default(),
                projects: vec![],
            };
            let ctx = StoreContext {
                dir: Path::new("/tmp"),
                config: &config,
                project: None,
                repository: None,
            };
            let result = BwBackend.store("NEW_KEY", "new-value", &ctx);
            assert!(result.is_err(), "expected store failure to be propagated");
        });
    }

    #[test]
    fn backend_resolve_with_bw_reference() {
        let item_json = make_bw_item_json("secret-pw");
        let script = format!(
            "#!/bin/sh\nif [ \"$1\" = \"list\" ] && [ \"$2\" = \"items\" ]; then\n  echo '[{{\"id\":\"item-1\",\"name\":\"my-item\",\"login\":{{\"password\":\"secret-pw\"}},\"fields\":[]}}]'\n  exit 0\nfi\nif [ \"$1\" = \"get\" ] && [ \"$2\" = \"item\" ] && [ \"$3\" = \"item-1\" ]; then\n  echo '{}'\n  exit 0\nfi\necho 'unexpected command' >&2\nexit 1\n",
            item_json
        );
        with_mock_bw(&script, || {
            let config = Config {
                defaults: Defaults::default(),
                log: LogConfig::default(),
                updates: UpdateConfig::default(),
                projects: vec![],
            };
            let dir = std::path::Path::new("/tmp");
            let ctx = make_resolve_context(&config, dir);
            let backend = BwBackend;
            let result = backend.resolve("API_KEY", Some("bw://my-item/password"), &ctx);
            assert_eq!(result.unwrap(), "secret-pw");
        });
    }

    #[test]
    fn backend_resolve_with_bw_reference_disambiguates_by_repository_before_folder_and_project() {
        let selected_item = serde_json::json!({
            "id": "item-1",
            "name": "my-item",
            "folderId": "folder-other",
            "login": { "password": "repo-pw" },
            "fields": [{"name":"repository","value":"git@github.com:example/test-repo.git","type":0}]
        });
        let items_json = serde_json::json!([
            selected_item,
            {
                "id": "item-2",
                "name": "my-item",
                "folderId": "folder-abc",
                "login": { "password": "folder-project-pw" },
                "fields": [{"name":"project","value":"test-project","type":0}]
            },
            {
                "id": "item-3",
                "name": "my-item",
                "folderId": "folder-other",
                "login": { "password": "wrong-pw" },
                "fields": []
            }
        ])
        .to_string();
        let selected_item_json = serde_json::json!({
            "id": "item-1",
            "name": "my-item",
            "login": { "password": "repo-pw" },
            "fields": [{"name":"repository","value":"git@github.com:example/test-repo.git","type":0}]
        })
        .to_string();
        let script = format!(
            "#!/bin/sh\nif [ \"$1\" = \"list\" ] && [ \"$2\" = \"folders\" ]; then\n  echo '[{{\"name\":\"Secrets\",\"id\":\"folder-abc\"}}]'\n  exit 0\nfi\nif [ \"$1\" = \"list\" ] && [ \"$2\" = \"items\" ]; then\n  echo '{}'\n  exit 0\nfi\nif [ \"$1\" = \"get\" ] && [ \"$2\" = \"item\" ] && [ \"$3\" = \"item-1\" ]; then\n  echo '{}'\n  exit 0\nfi\necho 'unexpected command' >&2\nexit 1\n",
            items_json, selected_item_json
        );

        with_mock_bw(&script, || {
            let config = Config {
                defaults: Defaults {
                    bw: BwConfig {
                        folder: Some("Secrets".to_string()),
                        ..Default::default()
                    },
                    ..Defaults::default()
                },
                log: LogConfig::default(),
                updates: UpdateConfig::default(),
                projects: vec![],
            };
            let ctx = make_resolve_context(&config, std::path::Path::new("/tmp"));
            let backend = BwBackend;
            let result = backend.resolve("API_KEY", Some("bw://my-item/password"), &ctx);
            assert_eq!(result.unwrap(), "repo-pw");
        });
    }

    #[test]
    fn resolve_batch_reference_disambiguates_by_folder_then_project() {
        let items_json = serde_json::json!([
            {
                "id": "item-1",
                "name": "my-item",
                "folderId": "folder-abc",
                "login": { "password": "proj-pw" },
                "fields": [{"name":"project","value":"test-project","type":0}]
            },
            {
                "id": "item-2",
                "name": "my-item",
                "folderId": "folder-abc",
                "login": { "password": "other-pw" },
                "fields": [{"name":"project","value":"other-project","type":0}]
            }
        ])
        .to_string();
        let selected_item_json = serde_json::json!({
            "id": "item-1",
            "name": "my-item",
            "login": { "password": "proj-pw" },
            "fields": [{"name":"project","value":"test-project","type":0}]
        })
        .to_string();
        let script = format!(
            "#!/bin/sh\nif [ \"$1\" = \"list\" ] && [ \"$2\" = \"folders\" ]; then\n  echo '[{{\"name\":\"Secrets\",\"id\":\"folder-abc\"}}]'\n  exit 0\nfi\nif [ \"$1\" = \"list\" ] && [ \"$2\" = \"items\" ]; then\n  echo '{}'\n  exit 0\nfi\nif [ \"$1\" = \"get\" ] && [ \"$2\" = \"item\" ] && [ \"$3\" = \"item-1\" ]; then\n  echo '{}'\n  exit 0\nfi\necho 'unexpected command' >&2\nexit 1\n",
            items_json, selected_item_json
        );

        with_mock_bw(&script, || {
            let config = Config {
                defaults: Defaults {
                    bw: BwConfig {
                        folder: Some("Secrets".to_string()),
                        ..Default::default()
                    },
                    ..Defaults::default()
                },
                log: LogConfig::default(),
                updates: UpdateConfig::default(),
                projects: vec![],
            };
            let ctx = make_resolve_context(&config, std::path::Path::new("/tmp"));
            let results =
                BwBackend::resolve_batch(&[("API_KEY", Some("bw://my-item/password"))], &ctx);
            assert_eq!(results.get("API_KEY").unwrap().as_ref().unwrap(), "proj-pw");
        });
    }

    #[test]
    fn resolve_batch_reference_reuses_selected_item_for_multiple_fields() {
        let state_dir = tempfile::TempDir::new().unwrap();
        let call_log = state_dir.path().join("bw-calls.log");
        let items_json = serde_json::json!([
            {
                "id": "item-1",
                "name": "my-item",
                "folderId": "folder-abc",
                "fields": []
            }
        ])
        .to_string();
        let item_json = serde_json::json!({
            "id": "item-1",
            "name": "my-item",
            "login": {
                "username": "service-user",
                "password": "service-pass"
            },
            "fields": []
        })
        .to_string();
        let script = format!(
            "#!/bin/sh\necho \"$@\" >> '{}'\nif [ \"$1\" = \"sync\" ]; then\n  exit 0\nfi\nif [ \"$1\" = \"list\" ] && [ \"$2\" = \"folders\" ]; then\n  echo '[{{\"name\":\"Secrets\",\"id\":\"folder-abc\"}}]'\n  exit 0\nfi\nif [ \"$1\" = \"list\" ] && [ \"$2\" = \"items\" ] && [ \"$3\" = \"--search\" ] && [ \"$4\" = \"my-item\" ]; then\n  echo '{}'\n  exit 0\nfi\nif [ \"$1\" = \"get\" ] && [ \"$2\" = \"item\" ] && [ \"$3\" = \"item-1\" ]; then\n  echo '{}'\n  exit 0\nfi\necho 'unexpected command' >&2\nexit 1\n",
            call_log.display(),
            items_json,
            item_json
        );

        with_mock_bw(&script, || {
            let config = Config {
                defaults: Defaults {
                    bw: BwConfig {
                        folder: Some("Secrets".to_string()),
                        ..Default::default()
                    },
                    ..Defaults::default()
                },
                log: LogConfig::default(),
                updates: UpdateConfig::default(),
                projects: vec![],
            };
            let ctx = make_resolve_context(&config, std::path::Path::new("/tmp"));
            let results = BwBackend::resolve_batch(
                &[
                    ("BW_USERNAME", Some("bw://my-item/username")),
                    ("BW_PASSWORD", Some("bw://my-item/password")),
                ],
                &ctx,
            );

            assert_eq!(
                results.get("BW_USERNAME").unwrap().as_ref().unwrap(),
                "service-user"
            );
            assert_eq!(
                results.get("BW_PASSWORD").unwrap().as_ref().unwrap(),
                "service-pass"
            );

            let log = std::fs::read_to_string(&call_log).unwrap();
            assert_eq!(
                log.lines()
                    .filter(|line| *line == "list items --search my-item")
                    .count(),
                1,
                "expected one list-items search for repeated bw:// item references"
            );
            assert_eq!(
                log.lines()
                    .filter(|line| *line == "get item item-1")
                    .count(),
                1,
                "expected one get-item fetch for repeated bw:// item references"
            );
        });
    }

    #[test]
    fn resolve_batch_key_lookup_disambiguates_by_repository_before_folder_and_project() {
        let items_json = serde_json::json!([
            {
                "name": "API_KEY",
                "folderId": "folder-other",
                "login": { "password": "repo-pw" },
                "fields": [{"name":"repository","value":"git@github.com:example/test-repo.git","type":0}]
            },
            {
                "name": "API_KEY",
                "folderId": "folder-abc",
                "login": { "password": "folder-project-pw" },
                "fields": [{"name":"project","value":"test-project","type":0}]
            }
        ])
        .to_string();
        let script = format!(
            "#!/bin/sh\nif [ \"$1\" = \"list\" ] && [ \"$2\" = \"folders\" ]; then\n  echo '[{{\"name\":\"Secrets\",\"id\":\"folder-abc\"}}]'\n  exit 0\nfi\nif [ \"$1\" = \"list\" ] && [ \"$2\" = \"items\" ]; then\n  echo '{}'\n  exit 0\nfi\necho 'unexpected command' >&2\nexit 1\n",
            items_json
        );

        with_mock_bw(&script, || {
            let config = Config {
                defaults: Defaults {
                    bw: BwConfig {
                        folder: Some("Secrets".to_string()),
                        ..Default::default()
                    },
                    ..Defaults::default()
                },
                log: LogConfig::default(),
                updates: UpdateConfig::default(),
                projects: vec![],
            };
            let ctx = make_resolve_context(&config, std::path::Path::new("/tmp"));
            let results = BwBackend::resolve_batch(&[("API_KEY", None)], &ctx);
            assert_eq!(results.get("API_KEY").unwrap().as_ref().unwrap(), "repo-pw");
        });
    }

    #[test]
    fn backend_resolve_direct_password_lookup() {
        let items_json =
            r#"[{"name":"MY_KEY","login":{"password":"direct-password"},"fields":[]}]"#;
        let script = format!("#!/bin/sh\necho '{}'\n", items_json);
        with_mock_bw(&script, || {
            let config = Config {
                defaults: Defaults::default(),
                log: LogConfig::default(),
                updates: UpdateConfig::default(),
                projects: vec![],
            };
            let dir = std::path::Path::new("/tmp");
            let ctx = make_resolve_context(&config, dir);
            let backend = BwBackend;
            let result = backend.resolve("MY_KEY", None, &ctx);
            assert_eq!(result.unwrap(), "direct-password");
        });
    }

    #[test]
    fn backend_resolve_retries_after_forced_sync_on_missing_item() {
        let state_dir = tempfile::TempDir::new().unwrap();
        let synced_marker = state_dir.path().join("synced-marker");
        let script = format!(
            "#!/bin/sh\nif [ \"$1\" = \"sync\" ]; then\n  touch '{}'\n  echo 'synced'\n  exit 0\nfi\nif [ \"$1\" = \"list\" ] && [ \"$2\" = \"items\" ]; then\n  if [ -f '{}' ]; then\n    echo '[{{\"name\":\"MY_KEY\",\"login\":{{\"password\":\"after-sync\"}},\"fields\":[]}}]'\n  else\n    echo '[]'\n  fi\n  exit 0\nfi\nexit 1\n",
            synced_marker.display(),
            synced_marker.display()
        );

        with_mock_bw(&script, || {
            let config = Config {
                defaults: Defaults::default(),
                log: LogConfig::default(),
                updates: UpdateConfig::default(),
                projects: vec![],
            };
            let dir = std::path::Path::new("/tmp");
            let ctx = make_resolve_context(&config, dir);
            let backend = BwBackend;
            let result = backend.resolve("MY_KEY", None, &ctx);
            assert_eq!(result.unwrap(), "after-sync");
        });
    }

    #[test]
    fn resolve_batch_retries_after_forced_sync_on_missing_item() {
        let state_dir = tempfile::TempDir::new().unwrap();
        let synced_marker = state_dir.path().join("synced-marker");
        let script = format!(
            "#!/bin/sh\nif [ \"$1\" = \"sync\" ]; then\n  touch '{}'\n  echo 'synced'\n  exit 0\nfi\nif [ \"$1\" = \"list\" ] && [ \"$2\" = \"items\" ]; then\n  if [ -f '{}' ]; then\n    echo '[{{\"name\":\"MY_KEY\",\"login\":{{\"password\":\"after-sync\"}},\"fields\":[]}}]'\n  else\n    echo '[]'\n  fi\n  exit 0\nfi\nexit 1\n",
            synced_marker.display(),
            synced_marker.display()
        );

        with_mock_bw(&script, || {
            let config = Config {
                defaults: Defaults::default(),
                log: LogConfig::default(),
                updates: UpdateConfig::default(),
                projects: vec![],
            };
            let dir = std::path::Path::new("/tmp");
            let ctx = make_resolve_context(&config, dir);
            let results = BwBackend::resolve_batch(&[("MY_KEY", None)], &ctx);
            assert_eq!(
                results.get("MY_KEY").unwrap().as_ref().unwrap(),
                "after-sync"
            );
        });
    }

    #[test]
    fn backend_has_returns_true_when_resolve_succeeds() {
        let items_json = r#"[{"name":"MY_KEY","login":{"password":"some-value"},"fields":[]}]"#;
        let script = format!("#!/bin/sh\necho '{}'\n", items_json);
        with_mock_bw(&script, || {
            let config = Config {
                defaults: Defaults::default(),
                log: LogConfig::default(),
                updates: UpdateConfig::default(),
                projects: vec![],
            };
            let dir = std::path::Path::new("/tmp");
            let ctx = make_resolve_context(&config, dir);
            let backend = BwBackend;
            assert_eq!(backend.has("MY_KEY", &ctx).unwrap(), true);
        });
    }

    #[test]
    fn backend_has_returns_false_when_resolve_fails() {
        with_mock_bw("#!/bin/sh\necho 'error' >&2\nexit 1\n", || {
            let config = Config {
                defaults: Defaults::default(),
                log: LogConfig::default(),
                updates: UpdateConfig::default(),
                projects: vec![],
            };
            let dir = std::path::Path::new("/tmp");
            let ctx = make_resolve_context(&config, dir);
            let backend = BwBackend;
            assert_eq!(backend.has("MY_KEY", &ctx).unwrap(), false);
        });
    }

    #[test]
    fn backend_name_is_bitwarden() {
        assert_eq!(BwBackend.name(), "Bitwarden");
    }

    #[test]
    fn backend_resolve_returns_err_for_invalid_bw_reference() {
        with_mock_bw("#!/bin/sh\necho 'ignored'\n", || {
            let config = Config {
                defaults: Defaults::default(),
                log: LogConfig::default(),
                updates: UpdateConfig::default(),
                projects: vec![],
            };
            let ctx = make_resolve_context(&config, std::path::Path::new("/tmp"));
            // "bw://invalid" has only one path segment, parse_bw_reference returns None
            let result = BwBackend.resolve("KEY", Some("bw://invalid"), &ctx);
            assert!(result.is_err());
            let msg = format!("{}", result.unwrap_err());
            assert!(
                msg.contains("Invalid bw:// reference format"),
                "unexpected: {msg}"
            );
        });
    }

    #[test]
    fn backend_resolve_with_item_configured() {
        let item_json = r#"{"type":1,"name":"my-bw-item","fields":[{"name":"MY_KEY","value":"item-field-value"}],"login":{"password":""}}"#;
        let script = format!("#!/bin/sh\necho '{}'\n", item_json);
        with_mock_bw(&script, || {
            let config = Config {
                defaults: Defaults {
                    backend: "bw".to_string(),
                    bw: BwConfig {
                        item: Some("my-bw-item".to_string()),
                        ..Default::default()
                    },
                    ..Defaults::default()
                },
                log: LogConfig::default(),
                updates: UpdateConfig::default(),
                projects: vec![],
            };
            let ctx = make_resolve_context(&config, std::path::Path::new("/tmp"));
            let result = BwBackend.resolve("MY_KEY", None, &ctx);
            assert_eq!(result.unwrap(), "item-field-value");
        });
    }

    #[test]
    fn backend_resolve_exact_name_match_from_list() {
        // bw list items --search returns items; only the exact name match is used
        let items_json = r#"[{"name":"MY_KEY","login":{"password":"exact-password"},"fields":[]},{"name":"MY_KEY_EXTRA","login":{"password":"wrong"},"fields":[]}]"#;
        let script = format!("#!/bin/sh\necho '{}'\n", items_json);
        with_mock_bw(&script, || {
            let config = Config {
                defaults: Defaults::default(),
                log: LogConfig::default(),
                updates: UpdateConfig::default(),
                projects: vec![],
            };
            let ctx = make_resolve_context(&config, std::path::Path::new("/tmp"));
            let result = BwBackend.resolve("MY_KEY", None, &ctx);
            assert_eq!(result.unwrap(), "exact-password");
        });
    }

    #[test]
    fn backend_resolve_disambiguates_by_project_single_match() {
        let items_json =
            r#"[{"name":"MY_KEY","id":"abc123","login":{"password":"project-pw"},"fields":[]}]"#;
        let script = format!("#!/bin/sh\necho '{}'\n", items_json);
        with_mock_bw(&script, || {
            let config = Config {
                defaults: Defaults::default(),
                log: LogConfig::default(),
                updates: UpdateConfig::default(),
                projects: vec![],
            };
            let ctx = make_resolve_context(&config, std::path::Path::new("/tmp"));
            let result = BwBackend.resolve("MY_KEY", None, &ctx);
            assert_eq!(result.unwrap(), "project-pw");
        });
    }

    #[test]
    fn disambiguate_items_narrows_by_folder() {
        // Two items with same name but different folders; one matches the folder ID
        let items_json = r#"[
            {"name":"DB_PASS","folderId":"folder-abc","login":{"password":"folder-pw"},"fields":[]},
            {"name":"DB_PASS","folderId":"folder-other","login":{"password":"wrong-pw"},"fields":[]}
        ]"#;
        let script = format!("#!/bin/sh\necho '{}'\n", items_json.replace('\n', ""));
        with_mock_bw(&script, || {
            let result = BwBackend::disambiguate_items("DB_PASS", None, Some("folder-abc"), None);
            assert_eq!(result.unwrap(), "folder-pw");
        });
    }

    #[test]
    fn disambiguate_items_narrows_by_repository_before_folder() {
        let items_json = r#"[
            {"name":"DB_PASS","folderId":"folder-other","login":{"password":"repo-pw"},"fields":[{"name":"repository","value":"git@github.com:example/test-repo.git","type":0}]},
            {"name":"DB_PASS","folderId":"folder-abc","login":{"password":"folder-pw"},"fields":[]}
        ]"#;
        let script = format!("#!/bin/sh\necho '{}'\n", items_json.replace('\n', ""));
        with_mock_bw(&script, || {
            let result = BwBackend::disambiguate_items(
                "DB_PASS",
                Some("git@github.com:example/test-repo.git"),
                Some("folder-abc"),
                None,
            );
            assert_eq!(result.unwrap(), "repo-pw");
        });
    }

    #[test]
    fn disambiguate_items_narrows_by_project_after_folder() {
        // Three items: two share the same folder, only one has the right project
        let items_json = r#"[
            {"name":"DB_PASS","folderId":"folder-abc","login":{"password":"proj-pw"},"fields":[{"name":"project","value":"myapp","type":0}]},
            {"name":"DB_PASS","folderId":"folder-abc","login":{"password":"other-pw"},"fields":[{"name":"project","value":"otherapp","type":0}]},
            {"name":"DB_PASS","folderId":"folder-other","login":{"password":"wrong-pw"},"fields":[]}
        ]"#;
        let script = format!("#!/bin/sh\necho '{}'\n", items_json.replace('\n', ""));
        with_mock_bw(&script, || {
            let result =
                BwBackend::disambiguate_items("DB_PASS", None, Some("folder-abc"), Some("myapp"));
            assert_eq!(result.unwrap(), "proj-pw");
        });
    }

    #[test]
    fn disambiguate_items_returns_empty_when_ambiguous() {
        // Two items, same folder, same project (or no project) — cannot disambiguate
        let items_json = r#"[
            {"name":"DB_PASS","folderId":"folder-abc","login":{"password":"pw1"},"fields":[]},
            {"name":"DB_PASS","folderId":"folder-abc","login":{"password":"pw2"},"fields":[]}
        ]"#;
        let script = format!("#!/bin/sh\necho '{}'\n", items_json.replace('\n', ""));
        with_mock_bw(&script, || {
            let result =
                BwBackend::disambiguate_items("DB_PASS", None, Some("folder-abc"), Some("myapp"));
            assert_eq!(result.unwrap(), "", "should return empty when ambiguous");
        });
    }

    #[test]
    fn resolve_folder_id_persists_cache_to_disk() {
        let state_dir = tempfile::TempDir::new().unwrap();
        let call_log = state_dir.path().join("bw-calls.log");
        let script = format!(
            "#!/bin/sh\necho \"$@\" >> '{}'\necho '[{{\"name\":\"Engineering\",\"id\":\"folder-123\"}}]'\n",
            call_log.display()
        );

        with_mock_bw(&script, || {
            let cache_path = state_dir
                .path()
                .join("pw-env")
                .join(FOLDER_ID_CACHE_FILE_NAME);
            BwBackend::set_test_folder_cache_path(Some(cache_path.clone()));

            let first = BwBackend::resolve_folder_id("Engineering").unwrap();
            assert_eq!(first.as_deref(), Some("folder-123"));
            assert!(cache_path.exists(), "expected persisted folder cache file");

            BwBackend::invalidate_folder_cache();

            let second = BwBackend::resolve_folder_id("Engineering").unwrap();
            assert_eq!(second.as_deref(), Some("folder-123"));

            let log = std::fs::read_to_string(&call_log).unwrap();
            let lookup_calls = log
                .lines()
                .filter(|line| *line == "list folders --search Engineering")
                .count();
            assert_eq!(lookup_calls, 1, "expected only one bw folder lookup");

            let persisted: FolderIdCacheStore =
                serde_json::from_str(&std::fs::read_to_string(&cache_path).unwrap()).unwrap();
            assert_eq!(
                persisted.folder_ids.get("Engineering").map(String::as_str),
                Some("folder-123")
            );
        });
    }

    #[test]
    fn resolve_folder_id_logs_when_loading_nonempty_persisted_cache() {
        let state_dir = tempfile::TempDir::new().unwrap();
        let cache_path = state_dir
            .path()
            .join("pw-env")
            .join(FOLDER_ID_CACHE_FILE_NAME);
        std::fs::create_dir_all(cache_path.parent().unwrap()).unwrap();
        std::fs::write(
            &cache_path,
            r#"{"folder_ids":{"Engineering":"folder-123"}}"#,
        )
        .unwrap();

        with_mock_bw("#!/bin/sh\necho unexpected >&2\nexit 1\n", || {
            BwBackend::set_test_folder_cache_path(Some(cache_path.clone()));

            let output = capture_debug_output(|| {
                let result = BwBackend::resolve_folder_id("Engineering").unwrap();
                assert_eq!(result.as_deref(), Some("folder-123"));
            });

            assert!(output.contains("Loaded persisted Bitwarden folder cache"));
            assert!(output.contains("entry_count=1"));
        });
    }

    #[test]
    fn resolve_folder_id_skips_loaded_cache_log_for_empty_persisted_cache() {
        let state_dir = tempfile::TempDir::new().unwrap();
        let cache_path = state_dir
            .path()
            .join("pw-env")
            .join(FOLDER_ID_CACHE_FILE_NAME);
        std::fs::create_dir_all(cache_path.parent().unwrap()).unwrap();
        std::fs::write(&cache_path, r#"{"folder_ids":{}}"#).unwrap();

        with_mock_bw("#!/bin/sh\necho '[]'\n", || {
            BwBackend::set_test_folder_cache_path(Some(cache_path.clone()));

            let output = capture_debug_output(|| {
                let result = BwBackend::resolve_folder_id("Missing").unwrap();
                assert_eq!(result, None);
            });

            assert!(!output.contains("Loaded persisted Bitwarden folder cache"));
        });
    }

    #[test]
    fn clear_folder_cache_removes_persisted_file() {
        let state_dir = tempfile::TempDir::new().unwrap();
        let cache_path = state_dir
            .path()
            .join("pw-env")
            .join(FOLDER_ID_CACHE_FILE_NAME);

        std::fs::create_dir_all(cache_path.parent().unwrap()).unwrap();
        std::fs::write(
            &cache_path,
            "{\"folder_ids\":{\"Engineering\":\"folder-123\"}}",
        )
        .unwrap();

        with_mock_bw("#!/bin/sh\nexit 1\n", || {
            BwBackend::set_test_folder_cache_path(Some(cache_path.clone()));
            assert!(BwBackend::clear_folder_cache().unwrap());
            assert!(!cache_path.exists());
        });
    }

    #[test]
    fn clear_sync_state_removes_persisted_file() {
        let state_dir = tempfile::TempDir::new().unwrap();
        let sync_state_path = state_dir.path().join("pw-env").join(SYNC_STATE_FILE_NAME);

        std::fs::create_dir_all(sync_state_path.parent().unwrap()).unwrap();
        std::fs::write(&sync_state_path, "{\"last_sync_unix_secs\":123}").unwrap();

        with_mock_bw("#!/bin/sh\nexit 1\n", || {
            BwBackend::set_test_sync_state_path(Some(sync_state_path.clone()));
            assert!(BwBackend::clear_sync_state().unwrap());
            assert!(!sync_state_path.exists());
        });
    }

    #[test]
    fn sync_vault_skips_recent_syncs_from_persisted_state() {
        let state_dir = tempfile::TempDir::new().unwrap();
        let sync_state_path = state_dir.path().join("pw-env").join(SYNC_STATE_FILE_NAME);
        std::fs::create_dir_all(sync_state_path.parent().unwrap()).unwrap();
        let now = BwBackend::current_unix_secs().unwrap();
        std::fs::write(
            &sync_state_path,
            format!("{{\"last_sync_unix_secs\":{now}}}"),
        )
        .unwrap();

        let call_log = state_dir.path().join("bw-sync-calls.log");
        let script = format!(
            "#!/bin/sh\necho \"$@\" >> '{}'\nexit 0\n",
            call_log.display()
        );

        with_mock_bw(&script, || {
            BwBackend::set_test_sync_state_path(Some(sync_state_path.clone()));
            BwBackend::sync_vault();
            assert!(
                !call_log.exists()
                    || std::fs::read_to_string(&call_log)
                        .unwrap()
                        .trim()
                        .is_empty(),
                "expected no bw sync call when sync state is fresh"
            );
        });
    }

    #[test]
    fn sync_vault_clamps_configured_throttle_to_one_hour_minimum() {
        let state_dir = tempfile::TempDir::new().unwrap();
        let sync_state_path = state_dir.path().join("pw-env").join(SYNC_STATE_FILE_NAME);
        std::fs::create_dir_all(sync_state_path.parent().unwrap()).unwrap();
        let now = BwBackend::current_unix_secs().unwrap();
        std::fs::write(
            &sync_state_path,
            format!("{{\"last_sync_unix_secs\":{}}}", now.saturating_sub(600)),
        )
        .unwrap();

        let call_log = state_dir.path().join("bw-sync-calls.log");
        let script = format!(
            "#!/bin/sh\necho \"$@\" >> '{}'\nexit 0\n",
            call_log.display()
        );

        with_mock_bw(&script, || {
            BwBackend::set_test_sync_state_path(Some(sync_state_path.clone()));
            BwBackend::set_test_sync_throttle_override(Some(30));
            BwBackend::sync_vault();
            assert!(
                !call_log.exists()
                    || std::fs::read_to_string(&call_log)
                        .unwrap()
                        .trim()
                        .is_empty(),
                "expected no bw sync call when configured throttle is below the one-hour minimum"
            );
        });
    }

    #[test]
    fn backend_store_creates_new_item_when_no_item_config() {
        // Mock bw that accepts stdin, exits 0 for create
        with_mock_bw("#!/bin/sh\ncat >/dev/null\necho 'created'\n", || {
            let config = Config {
                defaults: Defaults::default(),
                log: LogConfig::default(),
                updates: UpdateConfig::default(),
                projects: vec![],
            };
            let ctx = StoreContext {
                dir: Path::new("/tmp"),
                config: &config,
                project: None,
                repository: None,
            };
            let result = BwBackend.store("NEW_KEY", "new-value", &ctx);
            assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        });
    }

    #[test]
    fn test_migration_metadata_fields_include_project_and_source_dir() {
        let fields = BwBackend::migration_metadata_fields(&test_store_context());

        assert!(fields.iter().any(|field| {
            field.get("name").and_then(|value| value.as_str()) == Some("migrated_from")
                && field.get("value").and_then(|value| value.as_str())
                    == Some("/tmp/example/service")
        }));
        assert!(fields.iter().any(|field| {
            field.get("name").and_then(|value| value.as_str()) == Some("created-with")
                && field.get("value").and_then(|value| value.as_str())
                    == Some(&format!("pw-env ({})", env!("CARGO_PKG_VERSION")))
        }));
        assert!(fields.iter().any(|field| {
            field.get("name").and_then(|value| value.as_str()) == Some("project")
                && field.get("value").and_then(|value| value.as_str()) == Some("example")
        }));
        assert!(fields.iter().any(|field| {
            field.get("name").and_then(|value| value.as_str()) == Some("repository")
                && field.get("value").and_then(|value| value.as_str())
                    == Some("git@github.com:example/example.git")
        }));
    }

    #[test]
    fn test_upsert_custom_field_replaces_existing_field() {
        let mut item = serde_json::json!({
            "fields": [
                {
                    "name": "project",
                    "value": "old",
                    "type": 0
                }
            ]
        });

        BwBackend::upsert_custom_field(&mut item, "project", "new", 0);

        let fields = item
            .get("fields")
            .and_then(|value| value.as_array())
            .unwrap();
        assert_eq!(fields.len(), 1);
        assert_eq!(
            fields[0].get("value").and_then(|value| value.as_str()),
            Some("new")
        );
    }

    #[test]
    fn backend_store_edits_existing_item_when_item_configured() {
        // Mock bw: "get item" returns existing item JSON; "edit item" reads stdin and exits 0
        let item_json = r#"{"id":"item123","name":"my-bw-item","type":1,"login":{"password":"old"},"fields":[]}"#;
        let script = format!(
            "#!/bin/sh\nif [ \"$1\" = \"get\" ] && [ \"$2\" = \"item\" ]; then\necho '{}'\nexit 0\nfi\ncat >/dev/null\necho 'edited'\nexit 0\n",
            item_json
        );
        with_mock_bw(&script, || {
            let config = Config {
                defaults: Defaults {
                    backend: "bw".to_string(),
                    bw: BwConfig {
                        item: Some("my-bw-item".to_string()),
                        ..Default::default()
                    },
                    ..Defaults::default()
                },
                log: LogConfig::default(),
                updates: UpdateConfig::default(),
                projects: vec![],
            };
            let ctx = StoreContext {
                dir: Path::new("/tmp"),
                config: &config,
                project: None,
                repository: None,
            };
            let result = BwBackend.store("MY_KEY", "my-value", &ctx);
            assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        });
    }
    #[test]
    fn test_extract_field_from_value_password_prefers_login_over_username() {
        // When both login.password and login.username are present, asking for
        // "password" must return the password field, not the username.
        let item = serde_json::json!({
            "login": { "password": "mypass", "username": "myuser" }
        });
        assert_eq!(
            BwBackend::extract_field_from_value(&item, "password").unwrap(),
            "mypass"
        );
    }

    #[test]
    fn test_extract_field_from_value_username_ignores_password() {
        // When both fields are present, asking for "username" must return the
        // username field, not the password field.
        let item = serde_json::json!({
            "login": { "password": "mypass", "username": "myuser" }
        });
        assert_eq!(
            BwBackend::extract_field_from_value(&item, "username").unwrap(),
            "myuser"
        );
    }

    #[test]
    fn test_extract_field_from_value_custom_field_second_item_selected() {
        // When multiple custom fields exist, the correct one (by name) is returned.
        let item = serde_json::json!({
            "fields": [
                { "name": "other_field", "value": "other_value" },
                { "name": "api_key", "value": "secret123" }
            ]
        });
        assert_eq!(
            BwBackend::extract_field_from_value(&item, "api_key").unwrap(),
            "secret123"
        );
    }

    #[test]
    fn test_base64_encode_one_byte() {
        assert_eq!(base64_encode(b"f"), "Zg==");
    }

    #[test]
    fn test_base64_encode_two_bytes() {
        assert_eq!(base64_encode(b"fo"), "Zm8=");
    }

    #[test]
    fn test_base64_encode_three_bytes() {
        assert_eq!(base64_encode(b"foo"), "Zm9v");
    }

    #[test]
    fn test_base64_encode_six_bytes() {
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn test_base64_encode_binary_bytes() {
        assert_eq!(base64_encode(&[0xff, 0x80, 0x01]), "/4AB");
        assert_eq!(base64_encode(&[0x00, 0xff, 0x10]), "AP8Q");
    }

    #[test]
    fn reference_url_with_folder_returns_bw_url() {
        let config = Box::leak(Box::new(Config {
            defaults: Defaults {
                bw: BwConfig {
                    folder: Some("my-folder".to_string()),
                    ..BwConfig::default()
                },
                ..Defaults::default()
            },
            log: LogConfig::default(),
            updates: UpdateConfig::default(),
            projects: vec![],
        }));

        let ctx = StoreContext {
            dir: Path::new("/tmp/project"),
            config,
            project: Some("proj".to_string()),
            repository: None,
        };

        let url = BwBackend.reference_url("API_KEY", &ctx);
        assert_eq!(url.as_deref(), Some("bw://my-folder/API_KEY/password"));
    }

    #[test]
    fn reference_url_without_folder_returns_bw_url() {
        let config = Box::leak(Box::new(Config {
            defaults: Defaults::default(),
            log: LogConfig::default(),
            updates: UpdateConfig::default(),
            projects: vec![],
        }));

        let ctx = StoreContext {
            dir: Path::new("/tmp/project"),
            config,
            project: None,
            repository: None,
        };

        let url = BwBackend.reference_url("DB_URL", &ctx);
        assert_eq!(url.as_deref(), Some("bw://DB_URL/password"));
    }

    #[test]
    fn reference_url_with_item_uses_item_name() {
        let config = Box::leak(Box::new(Config {
            defaults: Defaults {
                backend: "bw".to_string(),
                bw: BwConfig {
                    folder: Some("env".to_string()),
                    item: Some("shared-secrets".to_string()),
                    ..BwConfig::default()
                },
                ..Defaults::default()
            },
            log: LogConfig::default(),
            updates: UpdateConfig::default(),
            projects: vec![],
        }));

        let ctx = StoreContext {
            dir: Path::new("/tmp/project"),
            config,
            project: None,
            repository: None,
        };

        let url = BwBackend.reference_url("SECRET", &ctx);
        assert_eq!(url.as_deref(), Some("bw://env/shared-secrets/SECRET"));
    }
}
fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        let triple = u32::from_be_bytes([0, b0, b1, b2]);
        result.push(CHARS[((triple >> 18) & 0x3F) as usize] as char);
        result.push(CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            result.push(CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(CHARS[(triple & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}
