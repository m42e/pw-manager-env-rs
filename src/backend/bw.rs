use anyhow::{Context, Result, bail};
use std::process::Command;
use std::sync::Mutex;
use tracing::{debug, info, trace, warn};

use super::{Backend, MIGRATED_FROM_FIELD_NAME, PROJECT_FIELD_NAME, ResolveContext, StoreContext};

/// Cached BW_SESSION key. `None` means not yet determined.
static SESSION: Mutex<Option<String>> = Mutex::new(None);

pub struct BwBackend;

impl BwBackend {
    fn migration_metadata_fields(ctx: &StoreContext) -> Vec<serde_json::Value> {
        let mut fields = vec![serde_json::json!({
            "name": MIGRATED_FROM_FIELD_NAME,
            "value": ctx.migrated_from(),
            "type": 0
        })];
        if let Some(project) = ctx.project.as_deref() {
            fields.push(serde_json::json!({
                "name": PROJECT_FIELD_NAME,
                "value": project,
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
            let output = cmd
                .output()
                .context("Failed to execute `bw` CLI. Is Bitwarden CLI installed?")?;
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
        debug!("Syncing Bitwarden vault");
        match Self::run_bw(&["sync"]) {
            Ok(_) => info!("Bitwarden vault synced"),
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
        let password = dialoguer::Password::new()
            .with_prompt("Bitwarden master password")
            .interact()
            .context("Failed to read master password")?;

        debug!("Running: bw unlock --raw --passwordenv ...");
        let output = Command::new("bw")
            .args(["unlock", "--raw", "--passwordenv", "BW_MASTER_PW"])
            .env("BW_MASTER_PW", &password)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .context("Failed to run `bw unlock`")?;

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

    /// Resolve a Bitwarden folder name to its UUID.
    fn resolve_folder_id(folder_name: &str) -> Result<Option<String>> {
        debug!(folder_name, "Looking up Bitwarden folder ID");
        let folders_json = Self::run_bw(&["list", "folders", "--search", folder_name])?;
        let folders: serde_json::Value = serde_json::from_str(&folders_json)?;
        if let Some(folder_arr) = folders.as_array() {
            // --search is a fuzzy/substring match; find the exact name match
            let exact = folder_arr.iter().find(|f| {
                f.get("name").and_then(|n| n.as_str()) == Some(folder_name)
            });
            if let Some(folder) = exact
                && let Some(id) = folder.get("id").and_then(|i| i.as_str())
            {
                debug!(folder_name, folder_id = %id, "Resolved Bitwarden folder");
                return Ok(Some(id.to_string()));
            }
        }
        warn!(folder_name, "Bitwarden folder not found");
        Ok(None)
    }

    fn run_bw(args: &[&str]) -> Result<String> {
        let mut cmd = Self::bw_command()?;
        cmd.args(args);
        cmd.stdin(std::process::Stdio::null());
        debug!("Running: bw {}", args.join(" "));
        let output = cmd
            .output()
            .context("Failed to execute `bw` CLI. Is Bitwarden CLI installed?")?;
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
                debug!("Retrying: bw {}", args.join(" "));
                let retry_output = retry_cmd
                    .output()
                    .context("Failed to execute `bw` CLI on retry")?;
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

    /// Resolve a key when multiple items share the same name, by narrowing
    /// candidates first by folder, then by the "project" custom field.
    ///
    /// If more than one candidate remains after all filters, a warning is
    /// printed and an empty string is returned so the caller can proceed
    /// without blocking the entire env resolution.
    fn disambiguate_items(
        key: &str,
        folder_id: Option<&str>,
        project: Option<&str>,
    ) -> Result<String> {
        let search_json = Self::run_bw(&["list", "items", "--search", key])?;
        let items: Vec<serde_json::Value> =
            serde_json::from_str(&search_json).context("Failed to parse bw list items JSON")?;

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
            return Self::extract_field_from_value(matching[0], "password");
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
                return Self::extract_field_from_value(matching[0], "password");
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
                .filter(|item| {
                    item.get("fields")
                        .and_then(|f| f.as_array())
                        .is_some_and(|fields| {
                            fields.iter().any(|field| {
                                let name = field.get("name").and_then(|n| n.as_str());
                                (name == Some("project") || name == Some("Project"))
                                    && field.get("value").and_then(|v| v.as_str()) == Some(proj)
                            })
                        })
                })
                .copied()
                .collect();
            if !project_filtered.is_empty() {
                matching = project_filtered;
            }
            if matching.len() == 1 {
                debug!("Disambiguated Bitwarden item by project field '{proj}'");
                return Self::extract_field_from_value(matching[0], "password");
            }
        }

        // Still ambiguous — notify user and leave blank
        warn!(
            "Multiple Bitwarden items found for '{key}' — could not disambiguate, leaving value blank"
        );
        eprintln!(
            "pw-env: multiple Bitwarden items found for '{key}'. \
             Configure defaults.bw.folder or add a 'project' field to disambiguate."
        );
        Ok(String::new())
    }
}

impl Backend for BwBackend {
    fn resolve(&self, key: &str, reference: Option<&str>, ctx: &ResolveContext) -> Result<String> {
        debug!(key, reference, "Resolving Bitwarden secret");
        let bw_config = ctx.config.effective_bw(ctx.dir);

        // Handle bw:// references
        if let Some(ref_str) = reference
            && ref_str.starts_with("bw://")
        {
            if let Some((_folder, item, field)) = Self::parse_bw_reference(ref_str) {
                debug!("Resolving Bitwarden reference: {ref_str}");
                let item_json = Self::run_bw(&["get", "item", item])?;
                return Self::extract_field_from_item(&item_json, field);
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
            Self::disambiguate_items(key, folder_id.as_deref(), ctx.project.as_deref())
        }
    }

    fn store(&self, key: &str, value: &str, ctx: &StoreContext) -> Result<()> {
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
                let mut child = cmd.spawn()?;
                if let Some(mut stdin) = child.stdin.take() {
                    use std::io::Write;
                    // bw expects base64-encoded JSON
                    let encoded_b64 = base64_encode(encoded.as_bytes());
                    stdin.write_all(encoded_b64.as_bytes())?;
                }
                let output = child.wait_with_output()?;
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
        let mut child = cmd.spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            stdin.write_all(encoded_b64.as_bytes())?;
        }
        let output = child.wait_with_output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("bw create failed: {stderr}");
        }
        info!(key, "Created new Bitwarden item");
        Ok(())
    }

    fn has(&self, key: &str, ctx: &ResolveContext) -> Result<bool> {
        match self.resolve(key, None, ctx) {
            Ok(_) => Ok(true),
            Err(_) => Ok(false),
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
    use std::path::Path;

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
        }
    }

    // ------- Mock-bw infrastructure -------

    fn with_mock_bw<F: FnOnce()>(script: &str, f: F) {
        let _guard = super::super::MOCK_PATH_MUTEX
            .lock()
            .unwrap_or_else(|p| p.into_inner());

        // Reset the cached session so each test starts fresh
        *super::SESSION.lock().unwrap() = None;

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
        // Reset session cache after test too
        *super::SESSION.lock().unwrap() = None;
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
    fn backend_resolve_with_bw_reference() {
        let item_json = make_bw_item_json("secret-pw");
        let script = format!("#!/bin/sh\necho '{}'\n", item_json);
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
    fn backend_resolve_direct_password_lookup() {
        let items_json = r#"[{"name":"MY_KEY","login":{"password":"direct-password"},"fields":[]}]"#;
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
            let result = BwBackend::disambiguate_items("DB_PASS", Some("folder-abc"), None);
            assert_eq!(result.unwrap(), "folder-pw");
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
                BwBackend::disambiguate_items("DB_PASS", Some("folder-abc"), Some("myapp"));
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
                BwBackend::disambiguate_items("DB_PASS", Some("folder-abc"), Some("myapp"));
            assert_eq!(result.unwrap(), "", "should return empty when ambiguous");
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
            field.get("name").and_then(|value| value.as_str()) == Some("project")
                && field.get("value").and_then(|value| value.as_str()) == Some("example")
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
}
fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;
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
