//! Port of `packages/ai/src/models-store.ts` @ 4181f66 (v0.84.1+) — T21a
//! updated the trait to accept `AuthOperationOptions` (cancellation) on every
//! operation, matching the upstream v0.84.0 signature.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use super::error::AiError;
use crate::auth::types::AuthOperationOptions;
use crate::types::AnyModel;

/// `ModelsStoreEntry`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsStoreEntry {
    /// Schema v6: the stored overlay carries every model type; chat entries
    /// deserialize as [`AnyModel::Chat`] (legacy chat-only files load
    /// unchanged).
    pub models: Vec<AnyModel>,
    /// Unix timestamp from the remote catalog's Last-Modified header.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<i64>,
    /// Unix timestamp of the last completed remote check.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<i64>,
    /// Opaque validator from the remote catalog's ETag header, stored verbatim
    /// (quotes included) and echoed back as If-None-Match.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
}

/// `ModelsStore` — persistent model catalogs keyed by provider ID
/// (models-store.ts @ 4181f66). Operations accept optional cancellation
/// matching the upstream `AuthOperationOptions` signature.
#[async_trait::async_trait]
pub trait ModelsStore: Send + Sync {
    async fn read(
        &self,
        provider_id: &str,
        options: Option<&AuthOperationOptions>,
    ) -> Result<Option<ModelsStoreEntry>, AiError>;
    async fn write(
        &self,
        provider_id: &str,
        entry: ModelsStoreEntry,
        options: Option<&AuthOperationOptions>,
    ) -> Result<(), AiError>;
    async fn delete(
        &self,
        provider_id: &str,
        options: Option<&AuthOperationOptions>,
    ) -> Result<(), AiError>;
}

/// `ProviderModelsStore` — store scoped to one provider. Kept for backward
/// compat with test helpers and external callers; the new `RefreshModelsContext`
/// uses `ModelsStore` directly.
#[async_trait::async_trait]
pub trait ProviderModelsStore: Send + Sync {
    async fn read(
        &self,
        options: Option<&AuthOperationOptions>,
    ) -> Result<Option<ModelsStoreEntry>, AiError>;
    async fn write(
        &self,
        entry: ModelsStoreEntry,
        options: Option<&AuthOperationOptions>,
    ) -> Result<(), AiError>;
    async fn delete(&self, options: Option<&AuthOperationOptions>) -> Result<(), AiError>;
}

/// `InMemoryModelsStore`.
#[derive(Debug, Default)]
pub struct InMemoryModelsStore {
    entries: Mutex<HashMap<String, ModelsStoreEntry>>,
}

impl InMemoryModelsStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait::async_trait]
impl ModelsStore for InMemoryModelsStore {
    async fn read(
        &self,
        provider_id: &str,
        _options: Option<&AuthOperationOptions>,
    ) -> Result<Option<ModelsStoreEntry>, AiError> {
        Ok(self
            .entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(provider_id)
            .cloned())
    }

    async fn write(
        &self,
        provider_id: &str,
        entry: ModelsStoreEntry,
        _options: Option<&AuthOperationOptions>,
    ) -> Result<(), AiError> {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(provider_id.to_owned(), entry);
        Ok(())
    }

    async fn delete(
        &self,
        provider_id: &str,
        _options: Option<&AuthOperationOptions>,
    ) -> Result<(), AiError> {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(provider_id);
        Ok(())
    }
}

/// JSON-file persistence skeleton. Layout: `{ "providers": { "<id>":
/// ModelsStoreEntry } }` in a single file, written atomically (tmp + rename).
///
/// The upstream package defines only the interface (apps provide the store);
/// this is the rpi-side persistent implementation used by the remote catalog
/// overlay in T13. The on-disk shape is internal to rpi (not a Pi wire
/// format).
pub struct JsonFileModelsStore {
    path: PathBuf,
    state: Mutex<HashMap<String, ModelsStoreEntry>>,
    /// Serializes `persist` (snapshot → tmp write → rename) so a stale
    /// snapshot can never win the atomic rename over a newer one.
    persist_lock: tokio::sync::Mutex<()>,
}

impl JsonFileModelsStore {
    /// Loads the store file; a missing file starts empty.
    pub async fn load(path: PathBuf) -> Result<Self, AiError> {
        let state = match tokio::fs::read_to_string(&path).await {
            Ok(content) => {
                // #8337: BOM-stripped before parse (models-store.ts:63).
                let content = crate::utils::text::strip_bom(&content);
                let file: JsonFileModelsStoreFile = serde_json::from_str(content)?;
                file.providers
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
            Err(error) => {
                return Err(AiError::ModelCatalog(format!(
                    "failed to read models store {}: {error}",
                    path.display()
                )));
            }
        };
        Ok(Self {
            path,
            state: Mutex::new(state),
            persist_lock: tokio::sync::Mutex::new(()),
        })
    }

    /// Writes the full state via a unique tmp file + atomic rename. Persists
    /// are serialized on `persist_lock` and the snapshot is taken after
    /// acquiring it, so the final rename always carries every entry written
    /// before the persist started (upstream serializes its read-modify-write
    /// through a file lock; the cross-process case is out of scope here).
    /// The tmp name is unique per call so two store instances sharing one
    /// path cannot clobber each other's tmp file.
    async fn persist(&self) -> Result<(), AiError> {
        let _persist_guard = self.persist_lock.lock().await;
        let snapshot = self.state.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let content = serde_json::to_string_pretty(&JsonFileModelsStoreFile {
            providers: snapshot,
        })?;
        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|error| {
                AiError::ModelCatalog(format!(
                    "failed to create models store directory {}: {error}",
                    parent.display()
                ))
            })?;
        }
        static TMP_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let tmp = self.path.with_extension(format!(
            "tmp-{}-{}",
            std::process::id(),
            TMP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let write_result = tokio::fs::write(&tmp, content).await;
        // #7779 (`c49906ec7`): the tmp+rename replacement must preserve the
        // existing target file's permissions/ACLs — copy them onto the tmp
        // file before the rename (a fresh file keeps the default mode).
        #[cfg(unix)]
        if write_result.is_ok()
            && let Ok(existing) = tokio::fs::metadata(&self.path).await
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = existing.permissions().mode();
            let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode));
        }
        let result = match write_result {
            Ok(()) => tokio::fs::rename(&tmp, &self.path).await,
            Err(error) => Err(error),
        };
        if let Err(error) = result {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(AiError::ModelCatalog(format!(
                "failed to persist models store {}: {error}",
                self.path.display()
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct JsonFileModelsStoreFile {
    #[serde(default)]
    providers: HashMap<String, ModelsStoreEntry>,
}

#[async_trait::async_trait]
impl ModelsStore for JsonFileModelsStore {
    async fn read(
        &self,
        provider_id: &str,
        _options: Option<&AuthOperationOptions>,
    ) -> Result<Option<ModelsStoreEntry>, AiError> {
        Ok(self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(provider_id)
            .cloned())
    }

    async fn write(
        &self,
        provider_id: &str,
        entry: ModelsStoreEntry,
        _options: Option<&AuthOperationOptions>,
    ) -> Result<(), AiError> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(provider_id.to_owned(), entry);
        self.persist().await
    }

    async fn delete(
        &self,
        provider_id: &str,
        _options: Option<&AuthOperationOptions>,
    ) -> Result<(), AiError> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(provider_id);
        self.persist().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_json_file_models_store_roundtrip() {
        let dir = std::env::temp_dir().join(format!("rpi-models-store-{}", std::process::id()));
        let path = dir.join("models-store.json");
        let store = JsonFileModelsStore::load(path.clone())
            .await
            .expect("load empty");
        let entry = ModelsStoreEntry {
            models: vec![],
            last_modified: Some(1),
            checked_at: Some(2),
            etag: Some("\"abc\"".to_owned()),
        };
        store
            .write("openai", entry.clone(), None)
            .await
            .expect("write");
        let loaded = JsonFileModelsStore::load(path.clone())
            .await
            .expect("reload");
        assert_eq!(
            loaded.read("openai", None).await.expect("read"),
            Some(entry)
        );
        loaded.delete("openai", None).await.expect("delete");
        assert_eq!(loaded.read("openai", None).await.expect("read"), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Concurrent writes must not clobber each other's tmp file (unique tmp
    /// name per persist); every entry survives and no tmp file is left.
    #[tokio::test]
    async fn test_json_file_models_store_concurrent_writes() {
        let dir = std::env::temp_dir().join(format!(
            "rpi-models-store-concurrent-{}",
            std::process::id()
        ));
        let path = dir.join("models-store.json");
        let store = std::sync::Arc::new(
            JsonFileModelsStore::load(path.clone())
                .await
                .expect("load empty"),
        );
        let mut tasks = Vec::new();
        for index in 0..16 {
            let store = store.clone();
            tasks.push(tokio::spawn(async move {
                store
                    .write(
                        &format!("provider-{index}"),
                        ModelsStoreEntry {
                            models: vec![],
                            last_modified: None,
                            checked_at: Some(index),
                            etag: None,
                        },
                        None,
                    )
                    .await
            }));
        }
        for task in tasks {
            task.await.expect("join").expect("write");
        }
        let loaded = JsonFileModelsStore::load(path.clone())
            .await
            .expect("reload");
        for index in 0..16 {
            assert!(
                loaded
                    .read(&format!("provider-{index}"), None)
                    .await
                    .expect("read")
                    .is_some(),
                "provider-{index} missing"
            );
        }
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .expect("read dir")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name != "models-store.json")
            .collect();
        assert!(leftovers.is_empty(), "tmp leftovers: {leftovers:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    // V14-12 FR-G: BOM normalization (#8337) + permission inheritance
    // (#7779, `c49906ec7`): the tmp+rename replacement preserves the
    // existing target's mode.

    #[cfg(unix)]
    fn mode_of(path: &std::path::Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777
    }

    #[tokio::test]
    async fn test_bom_prefixed_models_store_loads() {
        let dir = std::env::temp_dir().join(format!("rpi-models-store-bom-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("models-store.json");
        let clean = serde_json::to_string(&serde_json::json!({
            "providers": { "openai": { "models": [], "lastModified": 1 } }
        }))
        .expect("serialize");
        std::fs::write(&path, format!("\u{FEFF}{clean}")).expect("write bom");
        let store = JsonFileModelsStore::load(path.clone()).await.expect("load");
        assert!(
            store.read("openai", None).await.expect("read").is_some(),
            "BOM-stripped parse"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_persist_inherits_existing_file_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir =
            std::env::temp_dir().join(format!("rpi-models-store-perm-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("models-store.json");
        // Seed a store, then tighten the mode (admin-set ACL stand-in).
        let store = JsonFileModelsStore::load(path.clone()).await.expect("load");
        store
            .write(
                "openai",
                ModelsStoreEntry {
                    models: vec![],
                    last_modified: Some(1),
                    checked_at: None,
                    etag: None,
                },
                None,
            )
            .await
            .expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).expect("chmod");
        // A second write goes through tmp+rename; the mode must survive.
        store
            .write(
                "openai",
                ModelsStoreEntry {
                    models: vec![],
                    last_modified: Some(2),
                    checked_at: None,
                    etag: None,
                },
                None,
            )
            .await
            .expect("rewrite");
        assert_eq!(mode_of(&path), 0o640, "mode inherited across rename");
        let _ = std::fs::remove_dir_all(dir);
    }
}
