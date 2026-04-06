use anyhow::{Context, Result, bail};
use std::process::Command;
use tracing::{debug, info, warn};

use super::{Backend, MIGRATED_FROM_FIELD_NAME, PROJECT_FIELD_NAME, ResolveContext, StoreContext};

pub struct OpBackend;

impl OpBackend {
    fn text_field_assignment(field_name: &str, value: &str) -> String {
        format!("{field_name}[text]={value}")
    }

    fn migration_field_assignments(ctx: &StoreContext) -> Vec<String> {
        let mut assignments = vec![Self::text_field_assignment(
            MIGRATED_FROM_FIELD_NAME,
            &ctx.migrated_from(),
        )];
        if let Some(project) = ctx.project.as_deref() {
            assignments.push(Self::text_field_assignment(PROJECT_FIELD_NAME, project));
        }
        assignments
    }

    /// Run `op` with the given arguments, optionally scoped to an account.
    fn run_op(args: &[&str], account: Option<&str>) -> Result<String> {
        let mut cmd = Command::new("op");
        cmd.args(args);
        if let Some(acct) = account {
            cmd.arg("--account").arg(acct);
        }
        // Ensure no interactive prompts corrupt our stdout
        cmd.stdin(std::process::Stdio::null());
        debug!("Running: op {}", args.join(" "));
        let output = cmd
            .output()
            .context("Failed to execute `op` CLI. Is 1Password CLI installed?")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("op command failed: {stderr}");
        }
        let stdout = String::from_utf8(output.stdout).context("op output was not valid UTF-8")?;
        Ok(stdout.trim().to_string())
    }

    fn get_item_field(
        item: &str,
        field: &str,
        vault: Option<&str>,
        account: Option<&str>,
    ) -> Result<String> {
        let mut args = vec!["item", "get", item, "--fields", field, "--reveal"];
        let vault_arg;
        if let Some(v) = vault {
            vault_arg = format!("--vault={v}");
            args.push(&vault_arg);
        }
        Self::run_op(&args, account)
    }

    /// Resolve a key when multiple items share the same name, by checking
    /// the "project" custom field on each candidate item.
    fn resolve_by_project(
        key: &str,
        project: &str,
        vault: Option<&str>,
        account: Option<&str>,
    ) -> Result<String> {
        // List all items (optionally filtered by vault) as JSON
        let mut args = vec!["item", "list", "--format=json"];
        let vault_arg;
        if let Some(v) = vault {
            vault_arg = format!("--vault={v}");
            args.push(&vault_arg);
        }
        let list_json = Self::run_op(&args, account)?;
        let items: Vec<serde_json::Value> =
            serde_json::from_str(&list_json).context("Failed to parse op item list JSON")?;

        // Filter by items whose title matches the key
        let matching: Vec<&serde_json::Value> = items
            .iter()
            .filter(|item| item.get("title").and_then(|t| t.as_str()) == Some(key))
            .collect();

        if matching.is_empty() {
            bail!("No 1Password items found with title '{key}'");
        }

        if matching.len() == 1 {
            let id = matching[0]
                .get("id")
                .and_then(|i| i.as_str())
                .ok_or_else(|| anyhow::anyhow!("1Password item missing id"))?;
            return Self::get_item_field(id, "label=password", vault, account);
        }

        // Multiple matches — check each item's "project" field
        info!(
            "Found {} items named '{key}', disambiguating by project '{project}'",
            matching.len()
        );
        for item_summary in &matching {
            let id = item_summary
                .get("id")
                .and_then(|i| i.as_str())
                .ok_or_else(|| anyhow::anyhow!("1Password item missing id"))?;
            let full_json = Self::run_op(&["item", "get", id, "--format=json"], account)?;
            let full_item: serde_json::Value =
                serde_json::from_str(&full_json).context("Failed to parse op item get JSON")?;

            if let Some(fields) = full_item.get("fields").and_then(|f| f.as_array()) {
                for field in fields {
                    let label = field.get("label").and_then(|l| l.as_str());
                    if label == Some("project") || label == Some("Project") {
                        if field.get("value").and_then(|v| v.as_str()) == Some(project) {
                            debug!("Matched item '{id}' by project field '{project}'");
                            return Self::get_item_field(id, "label=password", vault, account);
                        }
                    }
                }
            }
        }

        bail!(
            "Multiple 1Password items found for '{key}' but none have a 'project' field matching '{project}'"
        );
    }
}

impl Backend for OpBackend {
    fn resolve(&self, key: &str, reference: Option<&str>, ctx: &ResolveContext) -> Result<String> {
        let op_config = ctx.config.effective_op(ctx.dir);
        let account = op_config.account.as_deref();

        if let Some(ref_str) = reference {
            // Direct op:// reference
            if ref_str.starts_with("op://") {
                debug!("Resolving 1Password reference: {ref_str}");
                return Self::run_op(&["read", ref_str], account);
            }
        }

        // Key-based lookup: look up as a field on the configured item, or as an item name
        if let Some(item) = ctx.config.effective_item(ctx.dir) {
            debug!("Resolving key '{key}' as field on item '{item}'");
            let label_arg = format!("label={key}");
            Self::get_item_field(
                item,
                label_arg.as_str(),
                op_config.vault.as_deref(),
                account,
            )
        } else if let Some(ref vault) = op_config.vault {
            // Search for an item named after the key in the configured vault
            debug!("Resolving key '{key}' as item in vault '{vault}'");
            let result = Self::get_item_field(key, "label=password", Some(vault), account);
            match result {
                Ok(value) => Ok(value),
                Err(e) if format!("{e}").to_lowercase().contains("more than 1 item") => {
                    if let Some(ref project) = ctx.project {
                        debug!(
                            "Multiple items match '{key}', disambiguating by project '{project}'"
                        );
                        Self::resolve_by_project(key, project, Some(vault), account)
                    } else {
                        Err(e)
                    }
                }
                Err(e) => Err(e),
            }
        } else {
            debug!("Resolving key '{key}' as item (no vault configured)");
            let result = Self::get_item_field(key, "label=password", None, account);
            match result {
                Ok(value) => Ok(value),
                Err(e) if format!("{e}").to_lowercase().contains("more than 1 item") => {
                    if let Some(ref project) = ctx.project {
                        debug!(
                            "Multiple items match '{key}', disambiguating by project '{project}'"
                        );
                        Self::resolve_by_project(key, project, None, account)
                    } else {
                        Err(e)
                    }
                }
                Err(e) => Err(e),
            }
        }
    }

    fn store(&self, key: &str, value: &str, ctx: &StoreContext) -> Result<()> {
        let op_config = ctx.config.effective_op(ctx.dir);
        let account = op_config.account.as_deref();
        let metadata_assignments = Self::migration_field_assignments(ctx);
        let metadata_refs: Vec<&str> = metadata_assignments
            .iter()
            .map(|assignment| assignment.as_str())
            .collect();

        if let Some(item) = ctx.config.effective_item(ctx.dir) {
            // Try to edit the existing item first, adding/updating the field
            debug!("Storing key '{key}' as field on item '{item}'");
            let field_assignment = format!("{key}={value}");
            let mut args = vec!["item", "edit", item, field_assignment.as_str()];
            args.extend_from_slice(&metadata_refs);
            let result = Self::run_op(&args, account);
            if result.is_ok() {
                return Ok(());
            }
            warn!("Failed to edit item '{item}', trying to create new item");
        }

        // Create a new item with the key as the item name
        let vault_args: Vec<String> = op_config
            .vault
            .as_ref()
            .map(|v| vec![format!("--vault={v}")])
            .unwrap_or_default();
        let vault_refs: Vec<&str> = vault_args.iter().map(|s| s.as_str()).collect();

        let field_assignment = format!("password={value}");
        let title_arg = format!("--title={key}");
        let mut args = vec![
            "item",
            "create",
            "--category=login",
            title_arg.as_str(),
            field_assignment.as_str(),
        ];
        args.extend_from_slice(&metadata_refs);
        args.extend_from_slice(&vault_refs);
        Self::run_op(&args, account)?;
        Ok(())
    }

    fn has(&self, key: &str, ctx: &ResolveContext) -> Result<bool> {
        match self.resolve(key, None, ctx) {
            Ok(_) => Ok(true),
            Err(_) => Ok(false),
        }
    }

    fn name(&self) -> &str {
        "1Password"
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::config::{Config, Defaults, LogConfig, UpdateConfig};
    use std::path::Path;

    #[test]
    fn test_text_field_assignment_format() {
        let result = OpBackend::text_field_assignment("api_key", "myvalue");
        assert_eq!(result, "api_key[text]=myvalue");
    }

    #[test]
    fn test_migration_field_assignments_with_project() {
        let config = Config {
            defaults: Defaults::default(),
            log: LogConfig::default(),
            updates: UpdateConfig::default(),
            projects: vec![],
        };
        let ctx = StoreContext {
            dir: Path::new("/work/project"),
            config: &config,
            project: Some("my-project".to_string()),
        };
        let assignments = OpBackend::migration_field_assignments(&ctx);
        assert!(assignments.contains(&"migrated_from[text]=/work/project".to_string()));
        assert!(assignments.contains(&"project[text]=my-project".to_string()));
    }

    #[test]
    fn test_migration_field_assignments_without_project() {
        let config = Config {
            defaults: Defaults::default(),
            log: LogConfig::default(),
            updates: UpdateConfig::default(),
            projects: vec![],
        };
        let ctx = StoreContext {
            dir: Path::new("/work/project"),
            config: &config,
            project: None,
        };
        let assignments = OpBackend::migration_field_assignments(&ctx);
        assert!(assignments.contains(&"migrated_from[text]=/work/project".to_string()));
        assert_eq!(assignments.len(), 1);
    }

    #[test]
    fn test_migration_field_assignments_include_project_and_source_dir() {
        let config = Config {
            defaults: Defaults::default(),
            log: LogConfig::default(),
            updates: UpdateConfig::default(),
            projects: vec![],
        };
        let ctx = StoreContext {
            dir: Path::new("/tmp/example/service"),
            config: &config,
            project: Some("example".to_string()),
        };

        let assignments = OpBackend::migration_field_assignments(&ctx);

        assert!(assignments.contains(&"migrated_from[text]=/tmp/example/service".to_string()));
        assert!(assignments.contains(&"project[text]=example".to_string()));
    }

    // ------- Mock-op infrastructure -------

    fn with_mock_op<F: FnOnce()>(script: &str, f: F) {
        let _guard = super::super::MOCK_PATH_MUTEX.lock().unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::TempDir::new().unwrap();
        let script_path = dir.path().join("op");
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
            std::iter::once(dir.path().to_path_buf())
                .chain(std::env::split_paths(&old_path)),
        )
        .unwrap();
        // SAFETY: guarded by MOCK_PATH_MUTEX, single-threaded access to PATH
        unsafe { std::env::set_var("PATH", &new_path) };
        f();
        unsafe { std::env::set_var("PATH", &old_path) };
    }

    fn make_op_resolve_context<'a>(config: &'a Config, dir: &'a Path) -> super::super::ResolveContext<'a> {
        super::super::ResolveContext {
            dir,
            config,
            project: Some("test-project".to_string()),
        }
    }

    #[test]
    fn run_op_returns_stdout_on_success() {
        with_mock_op("#!/bin/sh\necho 'op-value'\n", || {
            let result = OpBackend::run_op(&["any", "arg"], None);
            assert_eq!(result.unwrap(), "op-value");
        });
    }

    #[test]
    fn run_op_returns_err_on_non_zero_exit() {
        with_mock_op("#!/bin/sh\necho 'error' >&2\nexit 1\n", || {
            let result = OpBackend::run_op(&["any", "arg"], None);
            assert!(result.is_err());
        });
    }

    #[test]
    fn backend_resolve_with_op_reference() {
        with_mock_op("#!/bin/sh\necho 'op-secret'\n", || {
            let config = Config {
                defaults: Defaults::default(),
                log: LogConfig::default(),
                updates: UpdateConfig::default(),
                projects: vec![],
            };
            let dir = Path::new("/tmp");
            let ctx = make_op_resolve_context(&config, dir);
            let backend = OpBackend;
            let result = backend.resolve("API_KEY", Some("op://vault/item/field"), &ctx);
            assert_eq!(result.unwrap(), "op-secret");
        });
    }

    #[test]
    fn backend_has_returns_true_when_resolve_succeeds() {
        with_mock_op("#!/bin/sh\necho 'some-value'\n", || {
            let config = Config {
                defaults: Defaults::default(),
                log: LogConfig::default(),
                updates: UpdateConfig::default(),
                projects: vec![],
            };
            let dir = Path::new("/tmp");
            let ctx = make_op_resolve_context(&config, dir);
            let backend = OpBackend;
            assert_eq!(backend.has("MY_KEY", &ctx).unwrap(), true);
        });
    }

    #[test]
    fn backend_has_returns_false_when_resolve_fails() {
        with_mock_op("#!/bin/sh\necho 'error' >&2\nexit 1\n", || {
            let config = Config {
                defaults: Defaults::default(),
                log: LogConfig::default(),
                updates: UpdateConfig::default(),
                projects: vec![],
            };
            let dir = Path::new("/tmp");
            let ctx = make_op_resolve_context(&config, dir);
            let backend = OpBackend;
            assert_eq!(backend.has("MY_KEY", &ctx).unwrap(), false);
        });
    }

    #[test]
    fn backend_name_is_1password() {
        assert_eq!(OpBackend.name(), "1Password");
    }

    #[test]
    fn run_op_with_account_argument() {
        with_mock_op("#!/bin/sh\necho 'acct-value'\n", || {
            let result = OpBackend::run_op(&["item", "get", "test"], Some("my-account"));
            assert_eq!(result.unwrap(), "acct-value");
        });
    }

    #[test]
    fn get_item_field_with_vault_arg() {
        with_mock_op("#!/bin/sh\necho 'vault-secret'\n", || {
            let result = OpBackend::get_item_field("my-item", "label=password", Some("MyVault"), None);
            assert_eq!(result.unwrap(), "vault-secret");
        });
    }

    #[test]
    fn backend_resolve_with_item_configured() {
        with_mock_op("#!/bin/sh\necho 'item-field-value'\n", || {
            let config = Config {
                defaults: Defaults {
                    op: crate::config::OpConfig {
                        item: Some("my-op-item".to_string()),
                        ..Default::default()
                    },
                    ..Defaults::default()
                },
                log: LogConfig::default(),
                updates: UpdateConfig::default(),
                projects: vec![],
            };
            let ctx = make_op_resolve_context(&config, Path::new("/tmp"));
            let result = OpBackend.resolve("MY_KEY", None, &ctx);
            assert_eq!(result.unwrap(), "item-field-value");
        });
    }

    #[test]
    fn backend_resolve_with_vault_configured() {
        with_mock_op("#!/bin/sh\necho 'vault-item-value'\n", || {
            let config = Config {
                defaults: Defaults {
                    op: crate::config::OpConfig {
                        vault: Some("MyVault".to_string()),
                        ..Default::default()
                    },
                    ..Defaults::default()
                },
                log: LogConfig::default(),
                updates: UpdateConfig::default(),
                projects: vec![],
            };
            let ctx = make_op_resolve_context(&config, Path::new("/tmp"));
            let result = OpBackend.resolve("MY_KEY", None, &ctx);
            assert_eq!(result.unwrap(), "vault-item-value");
        });
    }

    #[test]
    fn backend_resolve_with_vault_disambiguates_by_project_single_match() {
        // First call: op item get MY_KEY --fields ... → "more than 1 item found" (triggers disambiguation)
        // Second call: op item list → single-item list
        // Third call: op item get abc123 --fields ... (by id) → "resolved-value"
        let script =
            "#!/bin/sh\nif [ \"$2\" = \"get\" ] && [ \"$3\" = \"MY_KEY\" ]; then\necho 'more than 1 item found' >&2\nexit 1\nfi\nif [ \"$2\" = \"list\" ]; then\necho '[{\"id\":\"abc123\",\"title\":\"MY_KEY\"}]'\nexit 0\nfi\necho 'resolved-value'\n";
        with_mock_op(script, || {
            let config = Config {
                defaults: Defaults {
                    op: crate::config::OpConfig {
                        vault: Some("MyVault".to_string()),
                        ..Default::default()
                    },
                    ..Defaults::default()
                },
                log: LogConfig::default(),
                updates: UpdateConfig::default(),
                projects: vec![],
            };
            let ctx = make_op_resolve_context(&config, Path::new("/tmp"));
            let result = OpBackend.resolve("MY_KEY", None, &ctx);
            assert_eq!(result.unwrap(), "resolved-value");
        });
    }

    #[test]
    fn backend_resolve_no_item_no_vault() {
        with_mock_op("#!/bin/sh\necho 'no-vault-value'\n", || {
            let config = Config {
                defaults: Defaults::default(), // no item, no vault
                log: LogConfig::default(),
                updates: UpdateConfig::default(),
                projects: vec![],
            };
            let ctx = make_op_resolve_context(&config, Path::new("/tmp"));
            let result = OpBackend.resolve("MY_KEY", None, &ctx);
            assert_eq!(result.unwrap(), "no-vault-value");
        });
    }

    #[test]
    fn backend_store_with_item_configured_succeeds() {
        with_mock_op("#!/bin/sh\necho 'edited'\n", || {
            let config = Config {
                defaults: Defaults {
                    op: crate::config::OpConfig {
                        item: Some("my-op-item".to_string()),
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
            let result = OpBackend.store("MY_KEY", "my-value", &ctx);
            assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        });
    }

    #[test]
    fn backend_store_creates_new_item_when_no_item_config() {
        with_mock_op("#!/bin/sh\necho 'created'\n", || {
            let config = Config {
                defaults: Defaults::default(), // no item configured
                log: LogConfig::default(),
                updates: UpdateConfig::default(),
                projects: vec![],
            };
            let ctx = StoreContext {
                dir: Path::new("/tmp"),
                config: &config,
                project: None,
            };
            let result = OpBackend.store("NEW_KEY", "new-value", &ctx);
            assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        });
    }

    #[test]
    fn get_item_field_calls_run_op_with_item_args() {
        with_mock_op("#!/bin/sh\necho 'field-value'\n", || {
            let result = OpBackend::get_item_field("my-item", "label=password", None, None);
            assert_eq!(result.unwrap(), "field-value");
        });
    }
}
