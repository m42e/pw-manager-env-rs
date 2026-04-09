use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Once;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{debug, warn};

use crate::config::{CacheConfig, state_dir, write_private_file};

const SECRET_CACHE_SERVICE: &str = "pw-env-resolved-secret-cache";
const SECRET_CACHE_FILE_NAME: &str = "resolved-secret-cache.json";

static KEYRING_DISABLED: AtomicBool = AtomicBool::new(false);
static KEYRING_WARNING_ONCE: Once = Once::new();

#[derive(Debug, Clone, Serialize)]
pub struct SecretCacheKey {
    pub env_path: String,
    pub backend: String,
    pub entry_key: String,
    pub entry_kind: String,
    pub raw_value: String,
    pub project: Option<String>,
    pub repository: Option<String>,
    pub effective_item: Option<String>,
    pub backend_config: String,
}

impl SecretCacheKey {
    pub fn fingerprint(&self) -> String {
        let serialized = serde_json::to_vec(self).unwrap_or_else(|_| {
            format!(
                "{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}",
                self.env_path,
                self.backend,
                self.entry_key,
                self.entry_kind,
                self.raw_value,
                self.project.as_deref().unwrap_or_default(),
                self.repository.as_deref().unwrap_or_default(),
                self.effective_item.as_deref().unwrap_or_default(),
                self.backend_config,
            )
            .into_bytes()
        });
        let digest = Sha256::digest(serialized);
        digest.iter().map(|b| format!("{b:02x}")).collect()
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct SecretCacheIndex {
    #[serde(default)]
    entries: BTreeMap<String, SecretCacheIndexEntry>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct SecretCacheIndexEntry {
    cached_at_unix_secs: u64,
}

pub struct SecretValueCache {
    enabled: bool,
    ttl_secs: u64,
    index: SecretCacheIndex,
}

#[derive(Debug, Default)]
pub struct ClearSecretCacheResult {
    pub cleared_index_file: bool,
    pub deleted_credentials: usize,
    pub keyring_delete_failures: usize,
    pub keyring_unavailable: bool,
}

impl SecretValueCache {
    pub fn load(config: &CacheConfig) -> Self {
        if !config.enabled {
            return Self {
                enabled: false,
                ttl_secs: 0,
                index: SecretCacheIndex::default(),
            };
        }

        let index = SecretCacheIndex::load().unwrap_or_else(|error| {
            warn!("Failed to load resolved-secret cache index: {error}");
            SecretCacheIndex::default()
        });

        Self {
            enabled: true,
            ttl_secs: config.ttl_hours.saturating_mul(3600),
            index,
        }
    }

    pub fn get(&mut self, key: &SecretCacheKey) -> Option<String> {
        if !self.enabled {
            return None;
        }

        let fingerprint = key.fingerprint();
        let entry = self.index.entries.get(&fingerprint).cloned()?;

        if self.is_expired(entry.cached_at_unix_secs) {
            debug!("Resolved-secret cache expired for {}", key.entry_key);
            self.index.entries.remove(&fingerprint);
            self.save_index();
            let _ = delete_secret(&fingerprint);
            return None;
        }

        match get_secret(&fingerprint) {
            Ok(Some(value)) => Some(value),
            Ok(None) => {
                self.index.entries.remove(&fingerprint);
                self.save_index();
                None
            }
            Err(KeyringAccess::Unavailable) => None,
        }
    }

    pub fn set(&mut self, key: &SecretCacheKey, value: &str) {
        if !self.enabled {
            return;
        }

        let fingerprint = key.fingerprint();
        match set_secret(&fingerprint, value) {
            Ok(()) => {
                self.index.entries.insert(
                    fingerprint.clone(),
                    SecretCacheIndexEntry {
                        cached_at_unix_secs: now_unix_secs(),
                    },
                );
                if let Err(error) = self.index.save() {
                    warn!("Failed to persist resolved-secret cache index: {error}");
                    self.index.entries.remove(&fingerprint);
                    let _ = delete_secret(&fingerprint);
                }
            }
            Err(KeyringAccess::Unavailable) => {}
        }
    }

    fn is_expired(&self, cached_at_unix_secs: u64) -> bool {
        let age_secs = now_unix_secs().saturating_sub(cached_at_unix_secs);
        age_secs >= self.ttl_secs
    }

    fn save_index(&self) {
        if let Err(error) = self.index.save() {
            warn!("Failed to persist resolved-secret cache index: {error}");
        }
    }
}

impl SecretCacheIndex {
    fn load() -> Result<Self> {
        let Some(path) = Self::path() else {
            return Ok(Self::default());
        };

        if !path.exists() {
            return Ok(Self::default());
        }

        let contents = std::fs::read_to_string(&path).with_context(|| {
            format!(
                "Failed to read resolved-secret cache index from {}",
                path.display()
            )
        })?;
        serde_json::from_str(&contents).with_context(|| {
            format!(
                "Failed to parse resolved-secret cache index from {}",
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
            .context("Failed to serialize resolved-secret cache index")?;
        write_private_file(&path, &contents).with_context(|| {
            format!(
                "Failed to write resolved-secret cache index to {}",
                path.display()
            )
        })
    }

    fn path() -> Option<PathBuf> {
        #[cfg(test)]
        if let Some(path) = TEST_SECRET_CACHE_INDEX_PATH.with(|value| value.borrow().clone()) {
            return Some(path);
        }

        Some(state_dir().join("pw-env").join(SECRET_CACHE_FILE_NAME))
    }
}

pub fn secret_cache_index_path() -> Option<PathBuf> {
    SecretCacheIndex::path()
}

pub fn clear_secret_cache() -> Result<ClearSecretCacheResult> {
    let mut result = ClearSecretCacheResult::default();
    let path = SecretCacheIndex::path();
    let index = SecretCacheIndex::load()?;

    for fingerprint in index.entries.keys() {
        match delete_secret(fingerprint) {
            Ok(true) => result.deleted_credentials += 1,
            Ok(false) => {}
            Err(KeyringAccess::Unavailable) => {
                result.keyring_unavailable = true;
                result.keyring_delete_failures += 1;
            }
        }
    }

    if let Some(path) = path
        && path.exists()
    {
        std::fs::remove_file(&path).with_context(|| {
            format!(
                "Failed to remove resolved-secret cache index {}",
                path.display()
            )
        })?;
        result.cleared_index_file = true;
    }

    Ok(result)
}

#[derive(Debug)]
enum KeyringAccess {
    Unavailable,
}

fn get_secret(fingerprint: &str) -> std::result::Result<Option<String>, KeyringAccess> {
    #[cfg(test)]
    if let Some(value) = test_keyring_get(fingerprint) {
        return value;
    }

    if KEYRING_DISABLED.load(Ordering::Relaxed) {
        return Err(KeyringAccess::Unavailable);
    }

    let entry =
        keyring::Entry::new(SECRET_CACHE_SERVICE, fingerprint).map_err(handle_keyring_error)?;
    match entry.get_password() {
        Ok(value) => Ok(Some(value)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(handle_keyring_error(error)),
    }
}

fn set_secret(fingerprint: &str, value: &str) -> std::result::Result<(), KeyringAccess> {
    #[cfg(test)]
    if let Some(result) = test_keyring_set(fingerprint, value) {
        return result;
    }

    if KEYRING_DISABLED.load(Ordering::Relaxed) {
        return Err(KeyringAccess::Unavailable);
    }

    let entry =
        keyring::Entry::new(SECRET_CACHE_SERVICE, fingerprint).map_err(handle_keyring_error)?;
    entry.set_password(value).map_err(handle_keyring_error)
}

fn delete_secret(fingerprint: &str) -> std::result::Result<bool, KeyringAccess> {
    #[cfg(test)]
    if let Some(result) = test_keyring_delete(fingerprint) {
        return result;
    }

    if KEYRING_DISABLED.load(Ordering::Relaxed) {
        return Err(KeyringAccess::Unavailable);
    }

    let entry =
        keyring::Entry::new(SECRET_CACHE_SERVICE, fingerprint).map_err(handle_keyring_error)?;
    match entry.delete_credential() {
        Ok(()) => Ok(true),
        Err(keyring::Error::NoEntry) => Ok(false),
        Err(error) => Err(handle_keyring_error(error)),
    }
}

fn handle_keyring_error(error: keyring::Error) -> KeyringAccess {
    KEYRING_DISABLED.store(true, Ordering::Relaxed);
    KEYRING_WARNING_ONCE.call_once(|| {
        warn!("OS keyring unavailable; resolved-secret caching is disabled for this run: {error}");
    });
    KeyringAccess::Unavailable
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
thread_local! {
    static TEST_SECRET_CACHE_INDEX_PATH: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
    static TEST_KEYRING_STATE: std::cell::RefCell<Option<TestKeyringState>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
#[derive(Debug, Clone, Default)]
enum TestKeyringState {
    #[default]
    Available,
    Unavailable,
}

#[cfg(test)]
thread_local! {
    static TEST_KEYRING_VALUES: std::cell::RefCell<BTreeMap<String, String>> = const { std::cell::RefCell::new(BTreeMap::new()) };
}

#[cfg(test)]
fn test_keyring_get(
    fingerprint: &str,
) -> Option<std::result::Result<Option<String>, KeyringAccess>> {
    TEST_KEYRING_STATE.with(|state| match state.borrow().clone() {
        None => None,
        Some(TestKeyringState::Unavailable) => Some(Err(KeyringAccess::Unavailable)),
        Some(TestKeyringState::Available) => Some(Ok(
            TEST_KEYRING_VALUES.with(|values| values.borrow().get(fingerprint).cloned())
        )),
    })
}

#[cfg(test)]
fn test_keyring_set(
    fingerprint: &str,
    value: &str,
) -> Option<std::result::Result<(), KeyringAccess>> {
    TEST_KEYRING_STATE.with(|state| match state.borrow().clone() {
        None => None,
        Some(TestKeyringState::Unavailable) => Some(Err(KeyringAccess::Unavailable)),
        Some(TestKeyringState::Available) => Some(Ok(TEST_KEYRING_VALUES.with(|values| {
            values
                .borrow_mut()
                .insert(fingerprint.to_string(), value.to_string());
        }))),
    })
}

#[cfg(test)]
fn test_keyring_delete(fingerprint: &str) -> Option<std::result::Result<bool, KeyringAccess>> {
    TEST_KEYRING_STATE.with(|state| match state.borrow().clone() {
        None => None,
        Some(TestKeyringState::Unavailable) => Some(Err(KeyringAccess::Unavailable)),
        Some(TestKeyringState::Available) => {
            Some(Ok(TEST_KEYRING_VALUES.with(|values| {
                values.borrow_mut().remove(fingerprint).is_some()
            })))
        }
    })
}

#[cfg(test)]
pub(crate) fn set_test_secret_cache_index_path(path: Option<PathBuf>) {
    TEST_SECRET_CACHE_INDEX_PATH.with(|value| *value.borrow_mut() = path);
}

#[cfg(test)]
pub(crate) fn set_test_keyring_available(available: bool) {
    TEST_KEYRING_STATE.with(|state| {
        *state.borrow_mut() = Some(if available {
            TestKeyringState::Available
        } else {
            TestKeyringState::Unavailable
        });
    });
}

#[cfg(test)]
pub(crate) fn reset_test_keyring() {
    TEST_KEYRING_STATE.with(|state| *state.borrow_mut() = None);
    TEST_KEYRING_VALUES.with(|values| values.borrow_mut().clear());
    KEYRING_DISABLED.store(false, Ordering::Relaxed);
}

#[cfg(test)]
pub(crate) fn test_keyring_contains(fingerprint: &str) -> bool {
    TEST_KEYRING_VALUES.with(|values| values.borrow().contains_key(fingerprint))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_key() -> SecretCacheKey {
        SecretCacheKey {
            env_path: "/tmp/project/.env".to_string(),
            backend: "op".to_string(),
            entry_key: "API_KEY".to_string(),
            entry_kind: "empty".to_string(),
            raw_value: "".to_string(),
            project: Some("project".to_string()),
            repository: Some("git@github.com:example/project.git".to_string()),
            effective_item: Some("project-env".to_string()),
            backend_config: "{\"vault\":\"Work\"}".to_string(),
        }
    }

    #[test]
    fn secret_cache_key_fingerprint_changes_with_backend_config() {
        let mut first = sample_key();
        let mut second = sample_key();
        second.backend_config = "{\"vault\":\"Other\"}".to_string();

        assert_ne!(first.fingerprint(), second.fingerprint());
        first.backend_config = second.backend_config.clone();
        assert_eq!(first.fingerprint(), second.fingerprint());
    }

    #[test]
    fn secret_value_cache_round_trips_with_test_keyring() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let index_path = temp_dir.path().join("pw-env").join(SECRET_CACHE_FILE_NAME);
        set_test_secret_cache_index_path(Some(index_path));
        set_test_keyring_available(true);

        let mut cache = SecretValueCache::load(&CacheConfig::default());
        let key = sample_key();
        cache.set(&key, "secret-value");

        let cached = cache.get(&key);

        reset_test_keyring();
        set_test_secret_cache_index_path(None);

        assert_eq!(cached.as_deref(), Some("secret-value"));
    }

    #[test]
    fn clear_secret_cache_removes_index_and_keyring_entries() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let index_path = temp_dir.path().join("pw-env").join(SECRET_CACHE_FILE_NAME);
        set_test_secret_cache_index_path(Some(index_path.clone()));
        set_test_keyring_available(true);

        let mut cache = SecretValueCache::load(&CacheConfig::default());
        let key = sample_key();
        let fingerprint = key.fingerprint();
        cache.set(&key, "secret-value");
        assert!(test_keyring_contains(&fingerprint));

        let result = clear_secret_cache().unwrap();

        assert!(!test_keyring_contains(&fingerprint));
        assert!(result.cleared_index_file);
        assert_eq!(result.deleted_credentials, 1);
        assert!(!index_path.exists());

        reset_test_keyring();
        set_test_secret_cache_index_path(None);
    }

    #[test]
    fn secret_value_cache_ignores_unavailable_keyring() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let index_path = temp_dir.path().join("pw-env").join(SECRET_CACHE_FILE_NAME);
        set_test_secret_cache_index_path(Some(index_path));
        set_test_keyring_available(false);

        let mut cache = SecretValueCache::load(&CacheConfig::default());
        let key = sample_key();
        cache.set(&key, "secret-value");

        let cached = cache.get(&key);

        reset_test_keyring();
        set_test_secret_cache_index_path(None);

        assert!(cached.is_none());
    }

    #[test]
    fn secret_value_cache_expired_entry_is_removed_and_persisted() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let index_path = temp_dir.path().join("pw-env").join(SECRET_CACHE_FILE_NAME);
        set_test_secret_cache_index_path(Some(index_path));
        set_test_keyring_available(true);

        let key = sample_key();
        let fingerprint = key.fingerprint();

        let mut index = SecretCacheIndex::default();
        index.entries.insert(
            fingerprint.clone(),
            SecretCacheIndexEntry {
                cached_at_unix_secs: 0,
            },
        );
        index.save().unwrap();

        TEST_KEYRING_VALUES.with(|values| {
            values
                .borrow_mut()
                .insert(fingerprint, "stale-secret".to_string());
        });

        let mut cache = SecretValueCache::load(&CacheConfig::default());
        let cached = cache.get(&key);
        let saved_index = SecretCacheIndex::load().unwrap();

        assert!(cached.is_none());
        assert!(saved_index.entries.is_empty());

        reset_test_keyring();
        set_test_secret_cache_index_path(None);
    }

    #[test]
    fn secret_cache_index_path_returns_configured_test_path() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let index_path = temp_dir.path().join("pw-env").join(SECRET_CACHE_FILE_NAME);
        set_test_secret_cache_index_path(Some(index_path.clone()));

        let resolved = secret_cache_index_path();

        assert_eq!(resolved, Some(index_path));

        set_test_secret_cache_index_path(None);
    }

    #[test]
    fn clear_secret_cache_counts_keyring_failures_per_entry() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let index_path = temp_dir.path().join("pw-env").join(SECRET_CACHE_FILE_NAME);
        set_test_secret_cache_index_path(Some(index_path));
        set_test_keyring_available(true);

        let mut cache = SecretValueCache::load(&CacheConfig::default());
        let first = sample_key();
        let mut second = sample_key();
        second.entry_key = "SECOND_KEY".to_string();

        cache.set(&first, "value-1");
        cache.set(&second, "value-2");

        set_test_keyring_available(false);
        let result = clear_secret_cache().unwrap();

        assert!(result.keyring_unavailable);
        assert_eq!(result.keyring_delete_failures, 2);

        reset_test_keyring();
        set_test_secret_cache_index_path(None);
    }

    #[test]
    fn now_unix_secs_returns_current_epoch_seconds() {
        assert!(now_unix_secs() > 1_000_000_000);
    }
}
