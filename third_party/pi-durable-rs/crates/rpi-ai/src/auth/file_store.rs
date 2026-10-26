//! Port of `packages/coding-agent/src/core/auth-storage.ts`
//! @ pi 0.82.1 (2efa728).
//!
//! [`FileCredentialStore`] — a [`CredentialStore`] backed by auth.json:
//! `Record<providerId, Credential>` serialized like
//! `JSON.stringify(data, null, 2)`, file mode 0600 (explicit permission bits
//! at creation plus a post-write chmod — never umask-dependent,
//! coding-standards §11.1), parent directory 0700.
//!
//! Intentional differences:
//! - Cross-process locking uses `fs2` flock (coding-standards §9.2 pins fs2).
//!   proper-lockfile's `stale` detection and `onCompromised` callback have no
//!   fs2 counterpart — a flock is released automatically on process exit, so
//!   there is no stale lock to detect. The retry policy mirrors upstream
//!   (`retries: 10, factor: 2, minTimeout: 100ms, maxTimeout: 10s,
//!   randomize: true`).
//! - No `rand` crate in the dependency baseline (appendix A): the
//!   `randomize` jitter derives a pseudo-random factor in [1, 2) from the
//!   high-resolution clock.
//! - The store takes an explicit path only (coding-standards §10.1): default
//!   path resolution (`~/.rpi/agent/auth.json`) belongs to the unified path
//!   module (T09).
//! - `AuthStorage` becomes [`FileCredentialStore`]; the sync/async lock pair
//!   collapses into one async `with_lock_async` (the fs2 lock lives on the
//!   file handle, which is `Send` and can be held across await points,
//!   unlike a mutex guard). The constructor's initial snapshot load keeps a
//!   short sync path.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use serde_json::{Map, Value};
use tokio::sync::{Mutex, RwLock};

use super::config_value::resolve_config_value;
use super::resolve::{ModelsError, ModelsErrorCode};
use super::types::{
    ApiKeyCredential, AuthOperationOptions, BoxFutureSend, Credential, CredentialInfo,
    CredentialStore, CredentialType, ModifyFn,
};

/// `AuthStorageData = Record<string, Credential>`.
///
/// Kept as parsed JSON (insertion-ordered `serde_json::Map`, the
/// `preserve_order` counterpart of a JS object) rather than a typed map:
/// write paths merge in JSON space, so unrelated entries — including shapes
/// newer rpi versions do not model — round-trip verbatim, and serialized
/// output preserves key order like upstream `JSON.stringify`.
type AuthStorageData = Map<String, Value>;

/// `LockResult<T>`.
struct LockResult<T> {
    result: T,
    next: Option<String>,
}

/// `parseStorageData` — empty/missing content parses as `{}`; malformed JSON
/// (or a non-object document) errors so a write path never overwrites an
/// unparseable file.
fn parse_storage_data(content: Option<&str>) -> Result<AuthStorageData, ModelsError> {
    match content {
        None | Some("") => Ok(Map::new()),
        // #8337 (`1355cd36e`): BOM stripped before parse (upstream strips at
        // each auth-storage read site; this is the single parse chokepoint).
        Some(content) => {
            match serde_json::from_str::<Value>(crate::utils::text::strip_bom(content)) {
                Ok(Value::Object(map)) => Ok(map),
                Ok(_) => Err(ModelsError::new(
                    ModelsErrorCode::Auth,
                    "Failed to parse auth storage data: top level is not a JSON object",
                )),
                Err(error) => Err(auth_error(
                    "Failed to parse auth storage data",
                    &error.to_string(),
                )),
            }
        }
    }
}

/// `JSON.stringify(data, null, 2)` — 2-space pretty, no trailing newline,
/// object keys in insertion order (serde_json `preserve_order`, matching JS
/// object key order). serde_json's pretty printer is byte-compatible with
/// `JSON.stringify` for the credential shapes (indent, string escaping,
/// non-ASCII emitted literally).
fn serialize_data(data: &AuthStorageData) -> Result<String, ModelsError> {
    serde_json::to_string_pretty(data)
        .map_err(|error| auth_error("Failed to serialize auth storage data", &error.to_string()))
}

/// Deserialize one entry to the typed `Credential` view. Unknown/newer
/// shapes error — write paths keep them verbatim in JSON space, but the
/// typed callback API cannot represent them (upstream passes raw objects).
fn parse_credential(value: &Value) -> Result<Credential, ModelsError> {
    serde_json::from_value(value.clone())
        .map_err(|error| auth_error("Failed to parse stored credential", &error.to_string()))
}

fn auth_error(message: impl Into<String>, cause: &str) -> ModelsError {
    ModelsError::with_cause(ModelsErrorCode::Auth, message, cause)
}

/// proper-lockfile retry policy, mirrored:
/// `{ retries: 10, factor: 2, minTimeout: 100, maxTimeout: 10000, randomize: true }`.
#[derive(Debug, Clone, Copy)]
struct RetryConfig {
    retries: u32,
    factor: u32,
    min_timeout: Duration,
    max_timeout: Duration,
    randomize: bool,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            retries: 10,
            factor: 2,
            min_timeout: Duration::from_millis(100),
            max_timeout: Duration::from_millis(10_000),
            randomize: true,
        }
    }
}

impl RetryConfig {
    /// Delay before retry `attempt` (0-based): exponential backoff capped at
    /// `max_timeout`, jittered into [1x, 2x) when `randomize` is set.
    fn delay(&self, attempt: u32) -> Duration {
        let base = self
            .min_timeout
            .saturating_mul(self.factor.saturating_pow(attempt))
            .min(self.max_timeout);
        if !self.randomize {
            return base;
        }
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        base.mul_f64(1.0 + f64::from(nanos % 1000) / 1000.0)
    }
}

/// `FileAuthStorageBackend` — auth.json file with fs2 cross-process locking.
pub struct FileAuthStorageBackend {
    auth_path: PathBuf,
    retry: RetryConfig,
}

impl FileAuthStorageBackend {
    pub fn new(auth_path: impl Into<PathBuf>) -> Self {
        Self {
            auth_path: auth_path.into(),
            retry: RetryConfig::default(),
        }
    }

    #[cfg(test)]
    fn with_retry_config(auth_path: impl Into<PathBuf>, retry: RetryConfig) -> Self {
        Self {
            auth_path: auth_path.into(),
            retry,
        }
    }

    /// `ensureParentDir` — recursive create, mode 0700.
    fn ensure_parent_dir(&self) -> std::io::Result<()> {
        let Some(dir) = self.auth_path.parent() else {
            return Ok(());
        };
        if !dir.exists() {
            std::fs::create_dir_all(dir)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
            }
        }
        Ok(())
    }

    /// Write helper — `AUTH_FILE_WRITE_OPTIONS = { mode: 0o600 }`: explicit
    /// permission bits at creation (never umask-dependent), matching
    /// upstream `writeFileSync` semantics for new files.
    ///
    /// P2-5 hardening (deliberate deviation from upstream's in-place
    /// `writeFileSync`): write to a sibling temp file, fsync, then `rename`
    /// over the target — a crash mid-write can no longer truncate the store
    /// to a partial file. The temp file inherits the EXISTING file's
    /// permission bits when one exists (admin-configured modes/ACLs must
    /// survive the rewrite, auth-storage.ts:24-25 @ c49906ec7, #7779); a
    /// new file is created 0o600.
    fn write_file(&self, content: &str) -> std::io::Result<()> {
        use std::io::Write;

        let parent = self
            .auth_path
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let file_name = self
            .auth_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "auth.json".to_string());
        let tmp_path = parent.join(format!(".{file_name}.tmp-{}", std::process::id()));

        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // 0o600 baseline for new stores; re-applied from the existing
            // file below when one is present.
            options.mode(0o600);
        }
        let mut file = options.open(&tmp_path)?;
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
        drop(file);

        #[cfg(unix)]
        {
            // Preserve an existing file's permission bits (see doc note).
            if let Ok(existing) = std::fs::metadata(&self.auth_path) {
                let _ = std::fs::set_permissions(&tmp_path, existing.permissions());
            }
        }

        // Rename is atomic within the same directory; the old contents
        // stay intact until the new ones are fully on disk.
        std::fs::rename(&tmp_path, &self.auth_path)?;

        // Best-effort directory fsync so the rename itself is durable
        // (directory fsync is unsupported via std on some platforms —
        // ignore failures).
        #[cfg(unix)]
        {
            if let Ok(dir) = std::fs::File::open(&parent) {
                let _ = dir.sync_all();
            }
        }
        #[cfg(not(unix))]
        {
            let _ = &parent;
        }
        Ok(())
    }

    /// `ensureFileExists` — seed an empty `{}` store.
    fn ensure_file_exists(&self) -> std::io::Result<()> {
        if !self.auth_path.exists() {
            self.write_file("{}")?;
        }
        Ok(())
    }

    fn open_lock_file(&self) -> std::io::Result<std::fs::File> {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.auth_path)
    }

    /// Async lock with the proper-lockfile retry policy (see [`RetryConfig`]).
    /// Returns the locked file handle; the flock releases on drop.
    async fn acquire_lock_with_retry(&self) -> Result<std::fs::File, ModelsError> {
        let mut last_error = None;
        for attempt in 0..=self.retry.retries {
            let file = self.open_lock_file().map_err(|error| {
                auth_error(
                    format!(
                        "Failed to open auth storage file {}",
                        self.auth_path.display()
                    ),
                    &error.to_string(),
                )
            })?;
            match file.try_lock_exclusive() {
                Ok(()) => return Ok(file),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    last_error = Some(error);
                    if attempt < self.retry.retries {
                        tokio::time::sleep(self.retry.delay(attempt)).await;
                    }
                }
                Err(error) => {
                    return Err(auth_error(
                        format!(
                            "Failed to lock auth storage file {}",
                            self.auth_path.display()
                        ),
                        &error.to_string(),
                    ));
                }
            }
        }
        Err(auth_error(
            format!(
                "Failed to acquire auth storage lock for {}",
                self.auth_path.display()
            ),
            &last_error.map(|e| e.to_string()).unwrap_or_default(),
        ))
    }

    /// `acquireLockSyncWithRetry` — 10 attempts, fixed 20ms sleep (upstream
    /// busy-waits; a thread sleep is the blocking equivalent).
    fn acquire_lock_sync_with_retry(&self) -> Result<std::fs::File, ModelsError> {
        const MAX_ATTEMPTS: u32 = 10;
        let mut last_error = None;
        for attempt in 1..=MAX_ATTEMPTS {
            let file = self.open_lock_file().map_err(|error| {
                auth_error(
                    format!(
                        "Failed to open auth storage file {}",
                        self.auth_path.display()
                    ),
                    &error.to_string(),
                )
            })?;
            match file.try_lock_exclusive() {
                Ok(()) => return Ok(file),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    last_error = Some(error);
                    if attempt < MAX_ATTEMPTS {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                }
                Err(error) => {
                    return Err(auth_error(
                        format!(
                            "Failed to lock auth storage file {}",
                            self.auth_path.display()
                        ),
                        &error.to_string(),
                    ));
                }
            }
        }
        Err(auth_error(
            "Failed to acquire auth storage lock",
            &last_error.map(|e| e.to_string()).unwrap_or_default(),
        ))
    }

    /// `withLockAsync` — the fs2 flock lives on the returned file handle: it
    /// is `Send`, can be held across `f`'s await points (unlike a mutex
    /// guard), and releases on drop, including error paths and process exit.
    async fn with_lock_async<T, F>(&self, f: F) -> Result<T, ModelsError>
    where
        F: FnOnce(Option<String>) -> BoxFutureSend<'static, Result<LockResult<T>, ModelsError>>
            + Send
            + 'static,
    {
        self.ensure_parent_dir().map_err(|error| {
            auth_error(
                format!(
                    "Failed to create auth storage directory for {}",
                    self.auth_path.display()
                ),
                &error.to_string(),
            )
        })?;
        self.ensure_file_exists().map_err(|error| {
            auth_error(
                format!(
                    "Failed to create auth storage file {}",
                    self.auth_path.display()
                ),
                &error.to_string(),
            )
        })?;

        let lock_file = self.acquire_lock_with_retry().await?;
        let current = match std::fs::read_to_string(&self.auth_path) {
            Ok(content) => Some(content),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(auth_error(
                    format!(
                        "Failed to read auth storage file {}",
                        self.auth_path.display()
                    ),
                    &error.to_string(),
                ));
            }
        };
        let LockResult { result, next } = f(current).await?;
        if let Some(next) = next {
            self.write_file(&next).map_err(|error| {
                auth_error(
                    format!(
                        "Failed to write auth storage file {}",
                        self.auth_path.display()
                    ),
                    &error.to_string(),
                )
            })?;
        }
        drop(lock_file);
        Ok(result)
    }

    /// Constructor-time initial snapshot load (`AuthStorage` constructor's
    /// `reload()`): creates the directory/file like upstream, then reads
    /// under the sync lock. Best-effort — any failure leaves an empty
    /// snapshot, exactly like upstream's catch-all.
    fn initial_load(&self) -> Option<String> {
        self.ensure_parent_dir().ok()?;
        self.ensure_file_exists().ok()?;
        let lock_file = self.acquire_lock_sync_with_retry().ok()?;
        let content = std::fs::read_to_string(&self.auth_path).ok();
        drop(lock_file);
        content
    }
}

/// `InMemoryAuthStorageBackend` — test backend, same lock semantics.
#[derive(Default)]
pub struct InMemoryAuthStorageBackend {
    value: Mutex<Option<String>>,
}

impl InMemoryAuthStorageBackend {
    pub fn new() -> Self {
        Self::default()
    }

    fn with_initial(content: String) -> Self {
        Self {
            value: Mutex::new(Some(content)),
        }
    }

    async fn with_lock_async<T, F>(&self, f: F) -> Result<T, ModelsError>
    where
        F: FnOnce(Option<String>) -> BoxFutureSend<'static, Result<LockResult<T>, ModelsError>>
            + Send
            + 'static,
    {
        let mut guard = self.value.lock().await;
        let LockResult { result, next } = f(guard.clone()).await?;
        if let Some(next) = next {
            *guard = Some(next);
        }
        Ok(result)
    }
}

/// `AuthStorageBackend` — static dispatch over the two backends.
pub enum Backend {
    File(FileAuthStorageBackend),
    Memory(InMemoryAuthStorageBackend),
}

impl Backend {
    async fn with_lock_async<T, F>(&self, f: F) -> Result<T, ModelsError>
    where
        F: FnOnce(Option<String>) -> BoxFutureSend<'static, Result<LockResult<T>, ModelsError>>
            + Send
            + 'static,
    {
        match self {
            Backend::File(backend) => backend.with_lock_async(f).await,
            Backend::Memory(backend) => backend.with_lock_async(f).await,
        }
    }
}

/// `AuthStorage` — credential storage backed by a JSON file. Writes are
/// serialized per provider (in-process async mutex) and cross-process (fs2
/// file lock); the in-memory snapshot serves `read`/`list`.
pub struct FileCredentialStore {
    backend: Backend,
    data: RwLock<AuthStorageData>,
    /// Serialize `modify`/`delete` per provider id.
    locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl FileCredentialStore {
    fn with_backend(backend: Backend, initial_content: Option<String>) -> Self {
        let data = parse_storage_data(initial_content.as_deref()).unwrap_or_default();
        Self {
            backend,
            data: RwLock::new(data),
            locks: Mutex::new(HashMap::new()),
        }
    }

    /// `AuthStorage.create(authPath)` — explicit path only; default path
    /// resolution belongs to the unified path module (T09), see module docs.
    pub fn new(auth_path: impl Into<PathBuf>) -> Self {
        let backend = FileAuthStorageBackend::new(auth_path);
        let initial = backend.initial_load();
        Self::with_backend(Backend::File(backend), initial)
    }

    #[cfg(test)]
    fn with_backend_for_tests(backend: Backend, initial_content: Option<String>) -> Self {
        Self::with_backend(backend, initial_content)
    }

    /// `AuthStorage.inMemory(data)`.
    pub fn in_memory(data: HashMap<String, Credential>) -> Self {
        let mut map = AuthStorageData::new();
        for (provider, credential) in data {
            // Credential serialization cannot fail on valid data; skip rather
            // than panic (no unwrap in non-test code).
            if let Ok(value) = serde_json::to_value(&credential) {
                map.insert(provider, value);
            }
        }
        let serialized = serialize_data(&map).unwrap_or_else(|_| "{}".to_owned());
        let backend = InMemoryAuthStorageBackend::with_initial(serialized.clone());
        Self::with_backend(Backend::Memory(backend), Some(serialized))
    }

    async fn provider_lock(&self, provider_id: &str) -> Arc<Mutex<()>> {
        self.locks
            .lock()
            .await
            .entry(provider_id.to_owned())
            .or_default()
            .clone()
    }

    /// `reload()` — refresh the in-memory snapshot from storage. On any
    /// failure (lock, IO, malformed JSON) the last valid snapshot is kept.
    pub async fn reload(&self) {
        let content = self
            .backend
            .with_lock_async(
                |current| -> BoxFutureSend<'static, Result<LockResult<Option<String>>, ModelsError>> {
                    Box::pin(async move {
                        Ok(LockResult {
                            result: current,
                            next: None,
                        })
                    })
                },
            )
            .await;
        let Ok(content) = content else {
            return;
        };
        if let Ok(data) = parse_storage_data(content.as_deref()) {
            *self.data.write().await = data;
        }
    }
}

#[async_trait::async_trait]
impl CredentialStore for FileCredentialStore {
    /// `read` — serves the snapshot; `api_key` entries with a configured key
    /// resolve it through the config-value DSL (command results cached
    /// process-wide, see `config_value.rs`). OAuth entries return unchanged.
    async fn read(
        &self,
        provider_id: &str,
        options: Option<&AuthOperationOptions>,
    ) -> Result<Option<Credential>, ModelsError> {
        // auth-storage.ts:444 `options?.signal?.throwIfAborted()` (via
        // `readLatestData`, auth-storage.ts:403).
        AuthOperationOptions::throw_if_cancelled(options)?;
        let data = self.data.read().await;
        let Some(value) = data.get(provider_id) else {
            return Ok(None);
        };
        let credential = parse_credential(value)?;
        let Credential::ApiKey(api_key) = &credential else {
            return Ok(Some(credential));
        };
        let Some(key) = &api_key.key else {
            return Ok(Some(credential));
        };
        // `{ ...credential, key: resolveConfigValue(key, credential.env) }` —
        // a failed resolution stores `None` (upstream: `undefined`).
        let resolved = resolve_config_value(key, api_key.env.as_ref());
        Ok(Some(Credential::ApiKey(ApiKeyCredential {
            key: resolved,
            env: api_key.env.clone(),
        })))
    }

    /// `list` — metadata only; never resolves keys or executes commands.
    /// Entries whose `type` tag is not a known credential type (newer
    /// shapes) are skipped: the closed [`CredentialType`] enum cannot
    /// represent them.
    async fn list(
        &self,
        options: Option<&AuthOperationOptions>,
    ) -> Result<Vec<CredentialInfo>, ModelsError> {
        // auth-storage.ts:488 `options?.signal?.throwIfAborted()`.
        AuthOperationOptions::throw_if_cancelled(options)?;
        Ok(self
            .data
            .read()
            .await
            .iter()
            .filter_map(|(provider_id, value)| {
                let credential_type = match value.get("type").and_then(Value::as_str) {
                    Some("api_key") => CredentialType::ApiKey,
                    Some("oauth") => CredentialType::Oauth,
                    _ => return None,
                };
                Some(CredentialInfo {
                    provider_id: provider_id.clone(),
                    credential_type,
                })
            })
            .collect())
    }

    /// `modify` — serialized read-modify-write under (per-provider in-process
    /// mutex + cross-process file lock). The callback returning `None`
    /// leaves the entry unchanged and resolves with the current credential
    /// (upstream `next ?? current`); malformed on-disk JSON propagates the
    /// parse error without writing. Merging happens in JSON space, so
    /// unrelated entries (including unknown shapes) survive verbatim and key
    /// order follows JS object semantics (existing keys keep position, new
    /// keys append).
    async fn modify(
        &self,
        provider_id: &str,
        f: ModifyFn,
        options: Option<&AuthOperationOptions>,
    ) -> Result<Option<Credential>, ModelsError> {
        AuthOperationOptions::throw_if_cancelled(options)?;
        let lock = self.provider_lock(provider_id).await;
        // T21b: queue-cancellable — cancel during lock-wait rejects without
        // running the task; after lock acquisition, re-check before executing
        // (credential-store.ts:14-28 @ 4181f66).
        let _guard = tokio::select! {
            biased;
            _ = super::credential_store::cancel_future(options) => return Err(ModelsError::aborted()),
            guard = lock.lock() => guard,
        };
        AuthOperationOptions::throw_if_cancelled(options)?;
        let provider = provider_id.to_owned();
        // Owned clone so the cancellation token can move into the `'static`
        // lock callback (auth-storage.ts:185 re-checks after `fn` returns).
        let options = options.cloned();
        let (snapshot, credential) = self
            .backend
            .with_lock_async(
                move |content| -> BoxFutureSend<
                    'static,
                    Result<LockResult<(AuthStorageData, Option<Credential>)>, ModelsError>,
                > {
                    let provider = provider.clone();
                    let options = options.clone();
                    Box::pin(async move {
                        let current_data = parse_storage_data(content.as_deref())?;
                        let current = match current_data.get(&provider) {
                            Some(value) => Some(parse_credential(value)?),
                            None => None,
                        };
                        let next = f(current).await?;
                        // auth-storage.ts:185: re-check after the callback
                        // returns, before writing — a cancelled modify must
                        // not persist its result.
                        AuthOperationOptions::throw_if_cancelled(options.as_ref())?;
                        match next {
                            None => {
                                let current = match current_data.get(&provider) {
                                    Some(value) => Some(parse_credential(value)?),
                                    None => None,
                                };
                                Ok(LockResult {
                                    result: (current_data, current),
                                    next: None,
                                })
                            }
                            Some(next) => {
                                let value = serde_json::to_value(&next).map_err(|error| {
                                    auth_error(
                                        "Failed to serialize stored credential",
                                        &error.to_string(),
                                    )
                                })?;
                                let mut merged = current_data;
                                merged.insert(provider, value);
                                let serialized = serialize_data(&merged)?;
                                Ok(LockResult {
                                    result: (merged, Some(next)),
                                    next: Some(serialized),
                                })
                            }
                        }
                    })
                },
            )
            .await?;
        *self.data.write().await = snapshot;
        Ok(credential)
    }

    /// `delete` — serialized against `modify` (same locks).
    async fn delete(
        &self,
        provider_id: &str,
        options: Option<&AuthOperationOptions>,
    ) -> Result<(), ModelsError> {
        AuthOperationOptions::throw_if_cancelled(options)?;
        let lock = self.provider_lock(provider_id).await;
        // T21b: queue-cancellable — cancel during lock-wait rejects without
        // running the task; after lock acquisition, re-check before executing
        // (credential-store.ts:14-28 @ 4181f66).
        let _guard = tokio::select! {
            biased;
            _ = super::credential_store::cancel_future(options) => return Err(ModelsError::aborted()),
            guard = lock.lock() => guard,
        };
        AuthOperationOptions::throw_if_cancelled(options)?;
        let provider = provider_id.to_owned();
        let snapshot = self
            .backend
            .with_lock_async(
                move |content| -> BoxFutureSend<
                    'static,
                    Result<LockResult<AuthStorageData>, ModelsError>,
                > {
                    let provider = provider.clone();
                    Box::pin(async move {
                        let mut current_data = parse_storage_data(content.as_deref())?;
                        current_data.remove(&provider);
                        let serialized = serialize_data(&current_data)?;
                        Ok(LockResult {
                            result: current_data,
                            next: Some(serialized),
                        })
                    })
                },
            )
            .await?;
        *self.data.write().await = snapshot;
        Ok(())
    }
}

/// `readStoredCredential` — one-off synchronous read from an auth.json file,
/// without instantiating a store or resolving configured key values.
pub fn read_stored_credential(
    provider_id: &str,
    auth_path: impl AsRef<Path>,
) -> Option<Credential> {
    let content = std::fs::read_to_string(auth_path).ok()?;
    let data = parse_storage_data(Some(&content)).ok()?;
    parse_credential(data.get(provider_id)?).ok()
}

/// V17-01 R2.2.2 (rpi-own [VARIANT]): diagnostics for credential entries
/// still keyed by the renamed Azure provider id. Read-only — the file is
/// never rewritten and the old key is never aliased. Missing or malformed
/// files yield no diagnostics (the store's own error path reports those).
pub fn legacy_azure_provider_diagnostics(auth_path: impl AsRef<Path>) -> Vec<String> {
    let file = auth_path.as_ref().display().to_string();
    let Ok(content) = std::fs::read_to_string(auth_path.as_ref()) else {
        return Vec::new();
    };
    let Ok(data) = parse_storage_data(Some(&content)) else {
        return Vec::new();
    };
    crate::azure_provider_rename::auth_json_diagnostics(&file, &data)
}

#[cfg(test)]
mod tests {
    //! Test intents ported from
    //! `packages/coding-agent/test/auth-storage.test.ts`
    //! @ pi 0.82.1 (2efa728), same names in snake_case.
    //!
    //! Not ported: "surfaces a compromised OAuth refresh lock and allows a
    //! later retry" exercises proper-lockfile's `onCompromised`, which has no
    //! fs2 counterpart (a flock cannot be compromised; see module docs).

    use std::sync::atomic::{AtomicU64, Ordering};

    use serde_json::{Map, Value, json};

    use super::*;
    use crate::types::ProviderEnv;

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_dir() -> PathBuf {
        let id = TEMP_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("rpi-test-auth-storage-{}-{id}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn write_auth_json(path: &Path, data: Value) {
        // Upstream `writeAuthJson` writes compact JSON (JSON.stringify(data)).
        std::fs::write(path, serde_json::to_string(&data).expect("serialize")).expect("write");
    }

    fn modify_fn<F>(f: F) -> ModifyFn
    where
        F: Fn(Option<Credential>) -> Result<Option<Credential>, ModelsError>
            + Send
            + Sync
            + 'static,
    {
        Arc::new(move |current| {
            let result = f(current);
            Box::pin(async move { result })
        })
    }

    fn api_key_credential(key: &str) -> Credential {
        Credential::ApiKey(ApiKeyCredential {
            key: Some(key.to_owned()),
            env: None,
        })
    }

    fn read_file_json(path: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(path).expect("read")).expect("parse")
    }

    struct EnvGuard(&'static str);

    impl EnvGuard {
        fn set(name: &'static str, value: &str) -> Self {
            rpi_test_env::set_var(name, value);
            Self(name)
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            rpi_test_env::remove_var(self.0);
        }
    }

    #[tokio::test]
    async fn reads_and_resolves_stored_api_key_credentials() {
        let _guard = EnvGuard::set("TEST_AUTH_STORAGE_KEY", "environment-key");
        let dir = temp_dir();
        let auth_path = dir.join("auth.json");
        write_auth_json(
            &auth_path,
            json!({ "anthropic": { "type": "api_key", "key": "$TEST_AUTH_STORAGE_KEY" } }),
        );
        let storage = FileCredentialStore::new(&auth_path);
        assert_eq!(
            storage.read("anthropic", None).await.expect("read"),
            Some(api_key_credential("environment-key"))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn resolves_command_backed_api_key_credentials() {
        let dir = temp_dir();
        let auth_path = dir.join("auth.json");
        write_auth_json(
            &auth_path,
            json!({ "anthropic": { "type": "api_key", "key": "!printf 'command-key'" } }),
        );
        let storage = FileCredentialStore::new(&auth_path);
        assert_eq!(
            storage.read("anthropic", None).await.expect("read"),
            Some(api_key_credential("command-key"))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn returns_oauth_credentials_unchanged() {
        let expires = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_millis() as i64
            + 60_000;
        let credential = Credential::OAuth(super::super::types::OAuthCredential {
            refresh: "refresh-token".to_owned(),
            access: "access-token".to_owned(),
            expires,
            extra: Map::new(),
        });
        let storage = FileCredentialStore::in_memory(HashMap::from([(
            "anthropic".to_owned(),
            credential.clone(),
        )]));
        assert_eq!(
            storage.read("anthropic", None).await.expect("read"),
            Some(credential)
        );
    }

    #[tokio::test]
    async fn credential_scoped_env_takes_precedence_and_remains_inspectable() {
        let dir = temp_dir();
        let auth_path = dir.join("auth.json");
        write_auth_json(
            &auth_path,
            json!({
                "anthropic": {
                    "type": "api_key",
                    "key": "$SCOPED_KEY",
                    "env": { "SCOPED_KEY": "scoped-value", "REGION": "test-region" }
                }
            }),
        );
        let storage = FileCredentialStore::new(&auth_path);
        let credential = match storage.read("anthropic", None).await.expect("read") {
            Some(Credential::ApiKey(credential)) => credential,
            other => panic!("expected api_key credential, got {other:?}"),
        };
        assert_eq!(credential.key.as_deref(), Some("scoped-value"));
        assert_eq!(
            credential.env,
            Some(ProviderEnv::from([
                ("SCOPED_KEY".to_owned(), "scoped-value".to_owned()),
                ("REGION".to_owned(), "test-region".to_owned()),
            ]))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn modify_persists_a_credential_while_preserving_unrelated_external_edits() {
        let dir = temp_dir();
        let auth_path = dir.join("auth.json");
        write_auth_json(
            &auth_path,
            json!({ "anthropic": { "type": "api_key", "key": "old" } }),
        );
        let storage = FileCredentialStore::new(&auth_path);
        write_auth_json(
            &auth_path,
            json!({
                "anthropic": { "type": "api_key", "key": "old" },
                "openai": { "type": "api_key", "key": "external" }
            }),
        );

        storage
            .modify(
                "anthropic",
                modify_fn(|_| Ok(Some(api_key_credential("new")))),
                None,
            )
            .await
            .expect("modify");

        assert_eq!(
            read_file_json(&auth_path),
            json!({
                "anthropic": { "type": "api_key", "key": "new" },
                "openai": { "type": "api_key", "key": "external" }
            })
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn modify_with_undefined_leaves_the_current_credential_unchanged() {
        let dir = temp_dir();
        let auth_path = dir.join("auth.json");
        write_auth_json(
            &auth_path,
            json!({ "anthropic": { "type": "api_key", "key": "stored" } }),
        );
        let storage = FileCredentialStore::new(&auth_path);
        assert_eq!(
            storage
                .modify("anthropic", modify_fn(|_| Ok(None)), None)
                .await
                .expect("modify"),
            Some(api_key_credential("stored"))
        );
        assert_eq!(
            storage.read("anthropic", None).await.expect("read"),
            Some(api_key_credential("stored"))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn serializes_concurrent_modifications() {
        let dir = temp_dir();
        let auth_path = dir.join("auth.json");
        write_auth_json(&auth_path, json!({}));
        let first = FileCredentialStore::new(&auth_path);
        let second = FileCredentialStore::new(&auth_path);
        let (one, two) = tokio::join!(
            first.modify(
                "anthropic",
                modify_fn(|_| Ok(Some(api_key_credential("anthropic-key")))),
                None,
            ),
            second.modify(
                "openai",
                modify_fn(|_| Ok(Some(api_key_credential("openai-key")))),
                None,
            ),
        );
        one.expect("first modify");
        two.expect("second modify");
        assert_eq!(
            read_file_json(&auth_path),
            json!({
                "anthropic": { "type": "api_key", "key": "anthropic-key" },
                "openai": { "type": "api_key", "key": "openai-key" }
            })
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn delete_removes_one_credential_while_preserving_others() {
        let dir = temp_dir();
        let auth_path = dir.join("auth.json");
        write_auth_json(
            &auth_path,
            json!({
                "anthropic": { "type": "api_key", "key": "anthropic-key" },
                "openai": { "type": "api_key", "key": "openai-key" }
            }),
        );
        let storage = FileCredentialStore::new(&auth_path);
        write_auth_json(
            &auth_path,
            json!({
                "anthropic": { "type": "api_key", "key": "anthropic-key" },
                "openai": { "type": "api_key", "key": "openai-key" },
                "google": { "type": "api_key", "key": "external-key" }
            }),
        );
        storage.delete("anthropic", None).await.expect("delete");
        let mut list = storage.list(None).await.expect("list");
        list.sort_by(|a, b| a.provider_id.cmp(&b.provider_id));
        assert_eq!(
            list,
            vec![
                CredentialInfo {
                    provider_id: "google".to_owned(),
                    credential_type: super::super::types::CredentialType::ApiKey,
                },
                CredentialInfo {
                    provider_id: "openai".to_owned(),
                    credential_type: super::super::types::CredentialType::ApiKey,
                },
            ]
        );
        assert_eq!(storage.read("anthropic", None).await.expect("read"), None);
        assert_eq!(
            storage.read("openai", None).await.expect("read"),
            Some(api_key_credential("openai-key"))
        );
        assert_eq!(
            storage.read("google", None).await.expect("read"),
            Some(api_key_credential("external-key"))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn in_memory_storage_implements_the_same_credential_store_behavior() {
        let storage = FileCredentialStore::in_memory(HashMap::from([(
            "anthropic".to_owned(),
            api_key_credential("initial"),
        )]));
        assert_eq!(
            storage.read("anthropic", None).await.expect("read"),
            Some(api_key_credential("initial"))
        );
        storage
            .modify(
                "anthropic",
                modify_fn(|_| Ok(Some(api_key_credential("updated")))),
                None,
            )
            .await
            .expect("modify");
        assert_eq!(
            storage.read("anthropic", None).await.expect("read"),
            Some(api_key_credential("updated"))
        );
        storage.delete("anthropic", None).await.expect("delete");
        assert!(storage.list(None).await.expect("list").is_empty());
    }

    #[tokio::test]
    async fn does_not_write_after_lock_acquisition_failure_and_recovers_on_retry() {
        let dir = temp_dir();
        let auth_path = dir.join("auth.json");
        write_auth_json(
            &auth_path,
            json!({ "anthropic": { "type": "api_key", "key": "stored" } }),
        );
        // Tiny retry budget: the upstream default (up to ~30s of backoff)
        // would make this test too slow; the retry policy itself is pinned
        // by `RetryConfig::default`.
        let backend = FileAuthStorageBackend::with_retry_config(
            &auth_path,
            RetryConfig {
                retries: 2,
                factor: 2,
                min_timeout: Duration::from_millis(1),
                max_timeout: Duration::from_millis(5),
                randomize: false,
            },
        );
        let storage = FileCredentialStore::with_backend_for_tests(Backend::File(backend), None);

        // Hold the flock on a separate file description: same-process locks
        // on different fds still conflict (upstream mocks the lock failure).
        let blocker = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&auth_path)
            .expect("open");
        blocker.try_lock_exclusive().expect("lock");

        let error = storage
            .modify(
                "openai",
                modify_fn(|_| Ok(Some(api_key_credential("new")))),
                None,
            )
            .await
            .expect_err("lock unavailable");
        assert!(
            error
                .message
                .contains("Failed to acquire auth storage lock")
        );
        assert_eq!(
            read_file_json(&auth_path),
            json!({ "anthropic": { "type": "api_key", "key": "stored" } })
        );

        blocker.unlock().expect("unlock");
        drop(blocker);
        storage
            .modify(
                "openai",
                modify_fn(|_| Ok(Some(api_key_credential("new")))),
                None,
            )
            .await
            .expect("modify after release");
        assert_eq!(
            read_file_json(&auth_path),
            json!({
                "anthropic": { "type": "api_key", "key": "stored" },
                "openai": { "type": "api_key", "key": "new" }
            })
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn does_not_overwrite_malformed_auth_files() {
        let dir = temp_dir();
        let auth_path = dir.join("auth.json");
        write_auth_json(
            &auth_path,
            json!({ "anthropic": { "type": "api_key", "key": "stored" } }),
        );
        let storage = FileCredentialStore::new(&auth_path);
        std::fs::write(&auth_path, "{invalid-json").expect("corrupt");
        storage
            .modify(
                "openai",
                modify_fn(|_| Ok(Some(api_key_credential("new")))),
                None,
            )
            .await
            .expect_err("malformed JSON must fail");
        assert_eq!(
            std::fs::read_to_string(&auth_path).expect("read"),
            "{invalid-json"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ------------------------------------------------------------------
    // Self-check coverage beyond the upstream test file.
    // ------------------------------------------------------------------

    /// Self-check: a created credential file has 0600 permissions, its parent 0700,
    /// and stays 0600 after writes.
    #[cfg(unix)]
    #[tokio::test]
    async fn auth_file_is_created_with_0600_and_parent_dir_with_0700() {
        use std::os::unix::fs::PermissionsExt;

        let dir = temp_dir();
        let nested = dir.join("agent");
        let auth_path = nested.join("auth.json");
        let storage = FileCredentialStore::new(&auth_path);
        assert_eq!(
            std::fs::metadata(&auth_path)
                .expect("stat")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(&nested)
                .expect("stat")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );

        storage
            .modify(
                "anthropic",
                modify_fn(|_| Ok(Some(api_key_credential("key")))),
                None,
            )
            .await
            .expect("modify");
        assert_eq!(
            std::fs::metadata(&auth_path)
                .expect("stat")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Self-check: `list()` returns only metadata, never parses or executes commands.
    #[tokio::test]
    async fn list_never_executes_configured_commands() {
        let dir = temp_dir();
        let marker = dir.join("marker");
        let auth_path = dir.join("auth.json");
        write_auth_json(
            &auth_path,
            json!({
                "anthropic": {
                    "type": "api_key",
                    "key": format!("!sh -c 'touch \"{}\"; echo key'", marker.display())
                }
            }),
        );
        let storage = FileCredentialStore::new(&auth_path);
        let list = storage.list(None).await.expect("list");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].provider_id, "anthropic");
        assert!(!marker.exists(), "list() must not execute commands");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Self-check: concurrent modify calls on the same provider from multiple tasks
    /// never tear (strictly serialized).
    #[tokio::test]
    async fn concurrent_same_provider_modifies_are_serialized() {
        let dir = temp_dir();
        let auth_path = dir.join("auth.json");
        write_auth_json(
            &auth_path,
            json!({ "anthropic": { "type": "api_key", "key": "v0" } }),
        );
        let storage = Arc::new(FileCredentialStore::new(&auth_path));

        let mut handles = Vec::new();
        for _ in 0..10 {
            let storage = storage.clone();
            handles.push(tokio::spawn(async move {
                storage
                    .modify(
                        "anthropic",
                        Arc::new(|current| {
                            Box::pin(async move {
                                let n = match &current {
                                    Some(Credential::ApiKey(api_key)) => api_key
                                        .key
                                        .as_deref()
                                        .and_then(|key| key.strip_prefix('v'))
                                        .and_then(|n| n.parse::<u32>().ok())
                                        .unwrap_or(0),
                                    _ => 0,
                                };
                                Ok(Some(api_key_credential(&format!("v{}", n + 1))))
                            })
                        }),
                        None,
                    )
                    .await
            }));
        }
        for handle in handles {
            handle.await.expect("join").expect("modify");
        }
        assert_eq!(
            read_file_json(&auth_path),
            json!({ "anthropic": { "type": "api_key", "key": "v10" } })
        );
        assert_eq!(
            storage.read("anthropic", None).await.expect("read"),
            Some(api_key_credential("v10"))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `reload()`: refresh the snapshot after external disk changes; a corrupted file
    /// keeps the old snapshot.
    #[tokio::test]
    async fn reload_refreshes_snapshot_and_keeps_it_on_failure() {
        let dir = temp_dir();
        let auth_path = dir.join("auth.json");
        write_auth_json(
            &auth_path,
            json!({ "anthropic": { "type": "api_key", "key": "old" } }),
        );
        let storage = FileCredentialStore::new(&auth_path);

        write_auth_json(
            &auth_path,
            json!({ "anthropic": { "type": "api_key", "key": "new" } }),
        );
        storage.reload().await;
        assert_eq!(
            storage.read("anthropic", None).await.expect("read"),
            Some(api_key_credential("new"))
        );

        std::fs::write(&auth_path, "{invalid-json").expect("corrupt");
        storage.reload().await;
        assert_eq!(
            storage.read("anthropic", None).await.expect("read"),
            Some(api_key_credential("new")),
            "reload failure must preserve the last valid snapshot"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `readStoredCredential`: does not parse the DSL and does not instantiate a store.
    #[tokio::test]
    async fn read_stored_credential_reads_without_resolving() {
        let dir = temp_dir();
        let auth_path = dir.join("auth.json");
        write_auth_json(
            &auth_path,
            json!({ "anthropic": { "type": "api_key", "key": "$TEST_READ_STORED_MISSING" } }),
        );
        assert_eq!(
            read_stored_credential("anthropic", &auth_path),
            Some(api_key_credential("$TEST_READ_STORED_MISSING"))
        );
        assert_eq!(read_stored_credential("openai", &auth_path), None);
        assert_eq!(
            read_stored_credential("anthropic", dir.join("missing.json")),
            None
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ------------------------------------------------------------------
    // Fixture parity (coding standards §12.3 / requirements §1.2.3):
    // the byte shape of `fixtures/generated/auth/auth.json` matches upstream's
    // `JSON.stringify(data, null, 2)` output (no trailing newline).
    // ------------------------------------------------------------------

    const AUTH_FIXTURE: &str = include_str!("../../../../fixtures/generated/auth/auth.json");

    const AUTH_DSL_FIXTURE: &str =
        include_str!("../../../../fixtures/generated/auth/auth-dsl.json");

    #[test]
    fn pi_auth_json_fixture_parses() {
        let data = parse_storage_data(Some(AUTH_FIXTURE)).expect("fixture parses");
        assert_eq!(data.len(), 2);
        let anthropic = match data
            .get("anthropic")
            .map(|v| parse_credential(v).expect("typed"))
        {
            Some(Credential::ApiKey(credential)) => credential,
            other => panic!("expected api_key credential, got {other:?}"),
        };
        assert_eq!(anthropic.key.as_deref(), Some("sk-ant-fixture-api-key"));
        let openai = match data
            .get("openai")
            .map(|v| parse_credential(v).expect("typed"))
        {
            Some(Credential::OAuth(credential)) => credential,
            other => panic!("expected oauth credential, got {other:?}"),
        };
        assert_eq!(openai.refresh, "fixture-refresh-token");
        assert_eq!(openai.access, "fixture-access-token");
        assert_eq!(openai.expires, 1_893_456_000_000);
        // Extra fields (`[key: string]: unknown` upstream) survive.
        assert_eq!(
            openai.extra.get("accountId"),
            Some(&json!("fixture-account-id"))
        );
    }

    #[test]
    fn serialization_matches_fixture_bytes() {
        let data = parse_storage_data(Some(AUTH_FIXTURE)).expect("fixture parses");
        assert_eq!(
            serialize_data(&data).expect("serialize"),
            AUTH_FIXTURE,
            "serialized output must byte-match JSON.stringify(data, null, 2)"
        );
    }

    #[tokio::test]
    async fn dsl_fixture_keys_resolve_on_read() {
        let _guard = EnvGuard::set("RPI_FIXTURE_API_KEY", "fixture-env-key");
        let dir = temp_dir();
        let auth_path = dir.join("auth.json");
        std::fs::write(&auth_path, AUTH_DSL_FIXTURE).expect("write fixture");
        let storage = FileCredentialStore::new(&auth_path);
        assert_eq!(
            storage.read("anthropic", None).await.expect("read"),
            Some(api_key_credential("fixture-env-key"))
        );
        assert_eq!(
            storage.read("openai", None).await.expect("read"),
            Some(api_key_credential("fixture-command-key"))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// read/list honor the entry `throwIfAborted`
    /// (auth-storage.ts:444/488 @ 4181f66).
    #[tokio::test]
    async fn read_and_list_reject_a_cancelled_signal() {
        let dir = temp_dir();
        let auth_path = dir.join("auth.json");
        write_auth_json(
            &auth_path,
            json!({ "anthropic": { "type": "api_key", "key": "k" } }),
        );
        let storage = FileCredentialStore::new(&auth_path);
        let token = tokio_util::sync::CancellationToken::new();
        token.cancel();
        let options = AuthOperationOptions::with_signal(token);
        let error = storage
            .read("anthropic", Some(&options))
            .await
            .expect_err("read must reject");
        assert_eq!(error.code, ModelsErrorCode::Aborted);
        let error = storage
            .list(Some(&options))
            .await
            .expect_err("list must reject");
        assert_eq!(error.code, ModelsErrorCode::Aborted);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Cancellation during the modify callback rejects before the write
    /// (auth-storage.ts:185 @ 4181f66): the new credential is not persisted.
    #[tokio::test]
    async fn modify_cancelled_after_callback_does_not_write() {
        let dir = temp_dir();
        let auth_path = dir.join("auth.json");
        let storage = FileCredentialStore::new(&auth_path);
        let token = tokio_util::sync::CancellationToken::new();
        let cancel_in_fn = token.clone();
        let f: ModifyFn = Arc::new(move |_| {
            let token = cancel_in_fn.clone();
            Box::pin(async move {
                token.cancel();
                Ok(Some(api_key_credential("new")))
            })
        });
        let options = AuthOperationOptions::with_signal(token);
        let error = storage
            .modify("anthropic", f, Some(&options))
            .await
            .expect_err("modify must reject");
        assert_eq!(error.code, ModelsErrorCode::Aborted);
        assert!(
            storage
                .read("anthropic", None)
                .await
                .expect("read")
                .is_none()
        );
        assert_eq!(read_file_json(&auth_path), json!({}));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // V14-12 FR-G: BOM normalization + permission preservation.

    #[cfg(unix)]
    fn mode_of(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777
    }

    // #8337 (`1355cd36e`): a BOM-prefixed auth.json parses like a clean one.
    #[tokio::test]
    async fn bom_prefixed_auth_json_loads() {
        let dir = temp_dir();
        let auth_path = dir.join("auth.json");
        let clean = serde_json::to_string(&json!({
            "anthropic": { "type": "api_key", "key": "sk-bom" }
        }))
        .expect("serialize");
        std::fs::write(&auth_path, format!("\u{FEFF}{clean}")).expect("write bom");

        let credential =
            read_stored_credential("anthropic", &auth_path).expect("credential parses");
        assert!(
            matches!(&credential, Credential::ApiKey(key) if key.key.as_deref() == Some("sk-bom")),
            "BOM-stripped parse"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // #7779 (`c49906ec7`): overwriting an existing file preserves its
    // permissions (no post-write chmod); creation still uses 0o600.
    #[cfg(unix)]
    #[tokio::test]
    async fn modify_preserves_existing_file_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir();
        let auth_path = dir.join("auth.json");
        write_auth_json(
            &auth_path,
            json!({ "anthropic": { "type": "api_key", "key": "old" } }),
        );
        // Admin-tightened mode (e.g. 0o640 group-readable).
        std::fs::set_permissions(&auth_path, std::fs::Permissions::from_mode(0o640))
            .expect("chmod");

        let storage = FileCredentialStore::new(&auth_path);
        storage
            .modify(
                "anthropic",
                modify_fn(|_| Ok(Some(api_key_credential("new")))),
                None,
            )
            .await
            .expect("modify");

        assert_eq!(mode_of(&auth_path), 0o640, "existing mode preserved");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fresh_auth_json_created_with_0600() {
        let dir = temp_dir();
        let auth_path = dir.join("auth.json");
        let storage = FileCredentialStore::new(&auth_path);
        storage
            .modify(
                "anthropic",
                modify_fn(|_| Ok(Some(api_key_credential("new")))),
                None,
            )
            .await
            .expect("modify");
        assert_eq!(mode_of(&auth_path), 0o600, "creation mode");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
