use anyhow::{Context, Result, bail};
use std::process::Command;
use tracing::{debug, info, warn};

use super::{
    Backend, MIGRATED_FROM_FIELD_NAME, PROJECT_FIELD_NAME, ResolveContext, StoreContext,
};

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

    fn upsert_custom_field(
        item: &mut serde_json::Value,
        name: &str,
        value: &str,
        field_type: u8,
    ) {
        let fields = item
            .as_object_mut()
            .and_then(|object| object.entry("fields").or_insert_with(|| serde_json::json!([])).as_array_mut())
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

    fn run_bw(args: &[&str]) -> Result<String> {
        let mut cmd = Command::new("bw");
        cmd.args(args);
        cmd.stdin(std::process::Stdio::null());
        debug!("Running: bw {}", args.join(" "));
        let output = cmd.output().context(
            "Failed to execute `bw` CLI. Is Bitwarden CLI installed? Is the vault unlocked (BW_SESSION)?",
        )?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("bw command failed: {stderr}");
        }
        let stdout = String::from_utf8(output.stdout)
            .context("bw output was not valid UTF-8")?;
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
        if field_name == "password" {
            if let Some(password) = item.get("login").and_then(|l| l.get("password")).and_then(|p| p.as_str()) {
                return Ok(password.to_string());
            }
        }
        if field_name == "username" {
            if let Some(username) = item.get("login").and_then(|l| l.get("username")).and_then(|u| u.as_str()) {
                return Ok(username.to_string());
            }
        }

        // Check custom fields
        if let Some(fields) = item.get("fields").and_then(|f| f.as_array()) {
            for f in fields {
                if f.get("name").and_then(|n| n.as_str()) == Some(field_name) {
                    if let Some(val) = f.get("value").and_then(|v| v.as_str()) {
                        return Ok(val.to_string());
                    }
                }
            }
        }

        // Check notes
        if field_name == "notes" {
            if let Some(notes) = item.get("notes").and_then(|n| n.as_str()) {
                return Ok(notes.to_string());
            }
        }

        bail!("Field '{field_name}' not found in Bitwarden item");
    }

    /// Resolve a key when multiple items share the same name, by checking
    /// the "project" custom field on each candidate item.
    fn resolve_by_project(key: &str, project: &str) -> Result<String> {
        let search_json = Self::run_bw(&["list", "items", "--search", key])?;
        let items: Vec<serde_json::Value> =
            serde_json::from_str(&search_json).context("Failed to parse bw list items JSON")?;

        // Filter by exact name match
        let matching: Vec<&serde_json::Value> = items
            .iter()
            .filter(|item| item.get("name").and_then(|n| n.as_str()) == Some(key))
            .collect();

        if matching.is_empty() {
            bail!("No Bitwarden items found with name '{key}'");
        }

        if matching.len() == 1 {
            return Self::extract_field_from_value(matching[0], "password");
        }

        // Multiple matches — check each item's "project" custom field
        info!(
            "Found {} items named '{key}', disambiguating by project '{project}'",
            matching.len()
        );
        for item in &matching {
            if let Some(fields) = item.get("fields").and_then(|f| f.as_array()) {
                for field in fields {
                    let name = field.get("name").and_then(|n| n.as_str());
                    if name == Some("project") || name == Some("Project") {
                        if field.get("value").and_then(|v| v.as_str()) == Some(project) {
                            debug!("Matched Bitwarden item by project field '{project}'");
                            return Self::extract_field_from_value(item, "password");
                        }
                    }
                }
            }
        }

        bail!(
            "Multiple Bitwarden items found for '{key}' but none have a 'project' field matching '{project}'"
        );
    }
}

impl Backend for BwBackend {
    fn resolve(&self, key: &str, reference: Option<&str>, ctx: &ResolveContext) -> Result<String> {
        let bw_config = ctx.config.effective_bw(ctx.dir);

        // Handle bw:// references
        if let Some(ref_str) = reference {
            if ref_str.starts_with("bw://") {
                if let Some((_folder, item, field)) = Self::parse_bw_reference(ref_str) {
                    debug!("Resolving Bitwarden reference: {ref_str}");
                    let item_json = Self::run_bw(&["get", "item", item])?;
                    return Self::extract_field_from_item(&item_json, field);
                } else {
                    bail!("Invalid bw:// reference format: {ref_str}. Expected bw://[folder/]item/field");
                }
            }
        }

        // Key-based lookup
        if let Some(item) = ctx.config.effective_item(ctx.dir) {
            // Look up key as a custom field on the configured item
            debug!("Resolving key '{key}' as field on Bitwarden item '{item}'");
            let item_json = Self::run_bw(&["get", "item", item])?;
            Self::extract_field_from_item(&item_json, key)
        } else {
            // Look up the key as a password item
            debug!("Resolving key '{key}' as Bitwarden item password");
            let result = Self::run_bw(&["get", "password", key]);
            match result {
                Ok(password) => return Ok(password),
                Err(e) => {
                    let err_msg = format!("{e}").to_lowercase();
                    if err_msg.contains("more than one") {
                        // Multiple matches — try disambiguation by project
                        if let Some(ref project) = ctx.project {
                            debug!("Multiple items match '{key}', disambiguating by project '{project}'");
                            return Self::resolve_by_project(key, project);
                        }
                        return Err(e);
                    }
                    // Fallback: try to get the item and check fields
                    warn!("Direct password lookup failed for '{key}', trying item lookup");
                }
            }
            let mut args = vec!["get", "item", key];
            let folder_id: String;
            if let Some(ref folder) = bw_config.folder {
                // First get the folder ID
                let folders_json = Self::run_bw(&["list", "folders", "--search", folder])?;
                let folders: serde_json::Value = serde_json::from_str(&folders_json)?;
                if let Some(folder_arr) = folders.as_array() {
                    if let Some(first) = folder_arr.first() {
                        if let Some(id) = first.get("id").and_then(|i| i.as_str()) {
                            folder_id = id.to_string();
                            args.extend_from_slice(&["--folderid", &folder_id]);
                        }
                    }
                }
            }
            let item_json = Self::run_bw(&args)?;
            Self::extract_field_from_item(&item_json, "password")
        }
    }

    fn store(&self, key: &str, value: &str, ctx: &StoreContext) -> Result<()> {
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
                        field.get("value").and_then(|field_value| field_value.as_str()),
                        field.get("type").and_then(|field_type| field_type.as_u64()),
                    ) {
                        Self::upsert_custom_field(&mut item, name, field_value, field_type as u8);
                    }
                }
                let encoded = serde_json::to_string(&item)?;
                // bw edit item expects base64-encoded JSON on stdin, but we use a simpler approach
                let mut cmd = Command::new("bw");
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
                return Ok(());
            }
        }

        // Create a new login item
        debug!("Creating new Bitwarden item for key '{key}'");
        let item_template = serde_json::json!({
            "type": 1,
            "name": key,
            "login": {
                "password": value
            },
            "folderId": bw_config.folder.as_deref(),
            "fields": metadata_fields
        });
        let encoded = serde_json::to_string(&item_template)?;
        let encoded_b64 = base64_encode(encoded.as_bytes());

        let mut cmd = Command::new("bw");
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, Defaults, LogConfig};
    use std::path::Path;

    fn test_store_context() -> StoreContext<'static> {
        let config = Box::leak(Box::new(Config {
            defaults: Defaults::default(),
            log: LogConfig::default(),
            projects: vec![],
        }));

        StoreContext {
            dir: Path::new("/tmp/example/service"),
            config,
            project: Some("example".to_string()),
        }
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

        let fields = item.get("fields").and_then(|value| value.as_array()).unwrap();
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].get("value").and_then(|value| value.as_str()), Some("new"));
    }
}

/// Simple base64 encoding (no padding issues) — avoids pulling in a crate for this one use.
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
