//! Port of `packages/ai/src/auth/resolve.ts` @ `abe508e1b` (v1.1.0,
//! `bde882c74`).
//!
//! Shared auth resolution: a stored credential owns the provider (hit stops),
//! ambient/env is consulted only when nothing is stored. No silent env
//! fallback after a failed refresh. OAuth refresh runs under the store lock
//! with double-checked expiry; the caller's signal cancels only the lock
//! wait, so a rotated refresh token is always persisted.

use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use super::types::{
    ApiKeyCredential, AuthContext, AuthOperationOptions, AuthResult, Credential, CredentialStore,
    OAuthCredential, ProviderAuth,
};
use crate::types::ProviderEnv;

/// `ModelsErrorCode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelsErrorCode {
    ModelSource,
    ModelValidation,
    Provider,
    Stream,
    Auth,
    Oauth,
    Aborted,
}

impl ModelsErrorCode {
    pub fn as_str(&self) -> &'static str {
        match self {
            ModelsErrorCode::ModelSource => "model_source",
            ModelsErrorCode::ModelValidation => "model_validation",
            ModelsErrorCode::Provider => "provider",
            ModelsErrorCode::Stream => "stream",
            ModelsErrorCode::Auth => "auth",
            ModelsErrorCode::Oauth => "oauth",
            ModelsErrorCode::Aborted => "aborted",
        }
    }
}

/// `ModelsError`.
#[derive(Debug, Clone)]
pub struct ModelsError {
    pub code: ModelsErrorCode,
    pub message: String,
}

impl ModelsError {
    pub fn new(code: ModelsErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// `signal.throwIfAborted()` equivalent — a cancelled operation rejects
    /// with this.
    pub fn aborted() -> Self {
        Self::new(ModelsErrorCode::Aborted, "Operation aborted")
    }

    /// `withCauseDetail`: callers surface `message` only, so keep the
    /// underlying reason in it.
    pub fn with_cause(code: ModelsErrorCode, message: impl Into<String>, cause: &str) -> Self {
        let message = message.into();
        let detail = cause.trim();
        let message = if detail.is_empty() || message.contains(detail) {
            message
        } else {
            format!("{message}: {detail}")
        };
        Self { code, message }
    }
}

impl std::fmt::Display for ModelsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ModelsError {}

/// `AuthResolutionOverrides` (auth/resolve.ts:18-24 @ 4181f66).
#[derive(Debug, Clone, Default)]
pub struct AuthResolutionOverrides {
    pub api_key: Option<String>,
    pub env: Option<ProviderEnv>,
    /// Require this much remaining OAuth-token validity; defaults to five
    /// minutes (upstream: callers can only *raise* the floor).
    pub min_oauth_validity_ms: Option<u64>,
    /// Cancels the wait for the credential-store lock; a refresh that has
    /// started completes and persists (auth/resolve.ts:24 @ `bde882c74`).
    pub signal: Option<CancellationToken>,
}

/// `DEFAULT_OAUTH_MINIMUM_VALIDITY_MS` (auth/resolve.ts:119 @ 4181f66).
const DEFAULT_OAUTH_MINIMUM_VALIDITY_MS: i64 = 5 * 60 * 1000;

/// `DEFAULT_OAUTH_REFRESH_TIMEOUT_MS` (auth/resolve.ts:120 @ 4181f66).
const DEFAULT_OAUTH_REFRESH_TIMEOUT_MS: std::time::Duration =
    std::time::Duration::from_millis(15_000);

/// `refreshStoredOAuthCredential` (auth/resolve.ts:105-155 @ `bde882c74`):
/// refresh a stored OAuth credential under the credential-store lock and
/// persist the result before the lock is released. `needs_refresh` is
/// re-checked under the lock, so concurrent callers and processes refresh
/// once.
///
/// `signal` cancels only the wait for the lock. Once a refresh starts, the
/// provider may already have rotated the refresh token, so the refresh and
/// its persistence ignore `signal` and are bounded only by
/// [`DEFAULT_OAUTH_REFRESH_TIMEOUT_MS`]. Otherwise a cancelled caller could
/// discard the only valid refresh token. Callers that must return promptly
/// on cancellation race this future with their signal.
pub async fn refresh_stored_oauth_credential(
    credentials: &Arc<dyn CredentialStore>,
    provider_id: &str,
    oauth: Arc<dyn super::types::OAuthAuth>,
    needs_refresh: Arc<dyn Fn(&OAuthCredential) -> bool + Send + Sync>,
    signal: &CancellationToken,
) -> Result<Option<OAuthCredential>, ModelsError> {
    refresh_stored_oauth_credential_with_timeout(
        credentials,
        provider_id,
        oauth,
        needs_refresh,
        signal,
        DEFAULT_OAUTH_REFRESH_TIMEOUT_MS,
    )
    .await
}

/// [`refresh_stored_oauth_credential`] with an injectable refresh timeout
/// (test seam; public callers use [`DEFAULT_OAUTH_REFRESH_TIMEOUT_MS`]).
async fn refresh_stored_oauth_credential_with_timeout(
    credentials: &Arc<dyn CredentialStore>,
    provider_id: &str,
    oauth: Arc<dyn super::types::OAuthAuth>,
    needs_refresh: Arc<dyn Fn(&OAuthCredential) -> bool + Send + Sync>,
    signal: &CancellationToken,
    refresh_timeout: std::time::Duration,
) -> Result<Option<OAuthCredential>, ModelsError> {
    // The store's modify is queue-cancellable through this token, but the
    // token is detached once the callback starts (upstream
    // `removeEventListener("abort", cancelLockWait)`), so a caller abort
    // during the refresh cannot stop the write.
    let lock_wait = CancellationToken::new();
    let detached = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let watcher = {
        let caller = signal.clone();
        let lock_wait = lock_wait.clone();
        let detached = detached.clone();
        tokio::spawn(async move {
            caller.cancelled().await;
            if !detached.load(std::sync::atomic::Ordering::SeqCst) {
                lock_wait.cancel();
            }
        })
    };
    let options = AuthOperationOptions::with_signal(lock_wait);
    let provider_id_owned = provider_id.to_owned();
    let caller_signal = signal.clone();
    let modify = {
        let credentials = credentials.clone();
        let provider_id = provider_id_owned.clone();
        async move {
            credentials
                .modify(
                    &provider_id,
                    Arc::new(move |current| {
                        let oauth = oauth.clone();
                        let needs_refresh = needs_refresh.clone();
                        let caller_signal = caller_signal.clone();
                        let detached = detached.clone();
                        let provider_id = provider_id_owned.clone();
                        Box::pin(async move {
                            detached.store(true, std::sync::atomic::Ordering::SeqCst);
                            // `signal.throwIfAborted()` after the detach
                            // (resolve.ts:136).
                            if caller_signal.is_cancelled() {
                                return Err(ModelsError::aborted());
                            }
                            let Some(Credential::OAuth(current)) = current else {
                                return Ok(None); // logged out meanwhile
                            };
                            if !needs_refresh(&current) {
                                return Ok(None); // another process/request refreshed
                            }
                            // The refresh ignores the caller signal: a rotated
                            // refresh token must be persisted.
                            match tokio::time::timeout(
                                refresh_timeout,
                                oauth.refresh(&current, None),
                            )
                            .await
                            {
                                Ok(Ok(refreshed)) => Ok(Some(Credential::OAuth(refreshed))),
                                Ok(Err(error)) => Err(ModelsError::with_cause(
                                    ModelsErrorCode::Oauth,
                                    format!("OAuth refresh failed for {provider_id}"),
                                    &error.message,
                                )),
                                Err(_elapsed) => Err(ModelsError::with_cause(
                                    ModelsErrorCode::Oauth,
                                    format!("OAuth refresh failed for {provider_id}"),
                                    "refresh timed out",
                                )),
                            }
                        })
                    }),
                    Some(&options),
                )
                .await
        }
    };
    // Spawn so a started refresh survives the caller returning/aborting; the
    // caller's signal only cancels the wait (upstream's outer promise race
    // leaves the in-flight refresh running, `bde882c74`).
    let join = tokio::spawn(modify);
    let result = tokio::select! {
        biased;
        _ = signal.cancelled() => {
            watcher.abort();
            // The detached task keeps running and persists the rotated
            // credential.
            return Err(ModelsError::aborted());
        }
        joined = join => match joined {
            Ok(inner) => inner,
            Err(error) => Err(ModelsError::with_cause(
                ModelsErrorCode::Auth,
                format!("Credential store modify failed for {provider_id}"),
                &error.to_string(),
            )),
        },
    };
    watcher.abort();
    match result {
        Ok(post) => Ok(post.and_then(|credential| match credential {
            Credential::OAuth(credential) => Some(credential),
            Credential::ApiKey(_) => None,
        })),
        Err(error) if error.code == ModelsErrorCode::Oauth => Err(error),
        Err(error) if error.code == ModelsErrorCode::Aborted => Err(error),
        Err(error) => Err(ModelsError::with_cause(
            ModelsErrorCode::Auth,
            format!("Credential store modify failed for {provider_id}"),
            &error.message,
        )),
    }
}

/// `overlayEnvAuthContext`: request-scoped env overrides win over the base.
struct OverlayEnvAuthContext<'a> {
    base: &'a dyn AuthContext,
    env: &'a ProviderEnv,
}

#[async_trait::async_trait]
impl AuthContext for OverlayEnvAuthContext<'_> {
    async fn env(&self, name: &str) -> Option<String> {
        match self.env.get(name) {
            Some(value) => Some(value.clone()),
            None => self.base.env(name).await,
        }
    }

    async fn file_exists(&self, path: &str) -> bool {
        self.base.file_exists(path).await
    }
}

/// `resolveProviderAuth` (auth/resolve.ts:31-93 @ `bde882c74`) — a stored
/// credential owns the provider (hit stops); ambient/env is consulted only
/// when nothing is stored. `overrides.signal` cancels the wait but a
/// started OAuth refresh completes and persists.
pub async fn resolve_provider_auth(
    provider_id: &str,
    auth: &ProviderAuth,
    credentials: &Arc<dyn CredentialStore>,
    auth_context: &Arc<dyn AuthContext>,
    overrides: Option<&AuthResolutionOverrides>,
) -> Result<Option<AuthResult>, ModelsError> {
    let overlay;
    let request_auth_context: &dyn AuthContext = match overrides.and_then(|o| o.env.as_ref()) {
        Some(env) => {
            overlay = OverlayEnvAuthContext {
                base: auth_context.as_ref(),
                env,
            };
            &overlay
        }
        None => auth_context.as_ref(),
    };

    if let Some(api_key) = overrides.and_then(|o| o.api_key.clone())
        && let Some(api_key_auth) = &auth.api_key
    {
        let credential = ApiKeyCredential {
            key: Some(api_key),
            env: overrides.and_then(|o| o.env.clone()),
        };
        return resolve_api_key(
            request_auth_context,
            api_key_auth.as_ref(),
            provider_id,
            Some(&credential),
        )
        .await;
    }

    // `operationSignal(overrides?.signal)` (resolve.ts:39): the caller can
    // cancel the lock wait; a started refresh still persists.
    let signal = overrides.and_then(|o| o.signal.clone()).unwrap_or_default();

    let stored = read_credential(credentials, provider_id, &signal).await?;
    if let Some(stored) = stored {
        match (&stored, &auth.oauth, &auth.api_key) {
            (Credential::OAuth(oauth_credential), Some(oauth), _) => {
                return resolve_stored_oauth(
                    credentials,
                    provider_id,
                    oauth.clone(),
                    oauth_credential,
                    &signal,
                    overrides.and_then(|o| o.min_oauth_validity_ms),
                    DEFAULT_OAUTH_REFRESH_TIMEOUT_MS,
                )
                .await;
            }
            (Credential::ApiKey(api_key_credential), _, Some(api_key_auth)) => {
                let credential = match overrides.and_then(|o| o.env.as_ref()) {
                    Some(env) => {
                        let mut merged = api_key_credential.clone();
                        let mut base = merged.env.unwrap_or_default();
                        base.extend(env.clone());
                        merged.env = Some(base);
                        merged
                    }
                    None => api_key_credential.clone(),
                };
                return resolve_api_key(
                    request_auth_context,
                    api_key_auth.as_ref(),
                    provider_id,
                    Some(&credential),
                )
                .await;
            }
            _ => return Ok(None),
        }
    }

    // Ambient (env vars, AWS profiles, ADC files).
    match &auth.api_key {
        Some(api_key_auth) => {
            resolve_api_key(
                request_auth_context,
                api_key_auth.as_ref(),
                provider_id,
                None,
            )
            .await
        }
        None => Ok(None),
    }
}

/// `resolveStoredOAuth` (auth/resolve.ts:127-179 @ 4181f66): double-checked
/// locking — tokens with more than five minutes remaining cost zero locks;
/// expiring tokens lock, re-check expiry under the lock, refresh once under
/// a hard timeout, persist the rotated credential before release.
///
/// `refresh_timeout` is a private injection point for tests; public callers
/// pass [`DEFAULT_OAUTH_REFRESH_TIMEOUT_MS`].
async fn resolve_stored_oauth(
    credentials: &Arc<dyn CredentialStore>,
    provider_id: &str,
    oauth: Arc<dyn super::types::OAuthAuth>,
    stored: &OAuthCredential,
    signal: &CancellationToken,
    min_oauth_validity_ms: Option<u64>,
    refresh_timeout: std::time::Duration,
) -> Result<Option<AuthResult>, ModelsError> {
    // Upstream: `minimumValidityMs = max(DEFAULT, minOAuthValidityMs ?? 0)` —
    // callers can only *raise* the floor (auth/resolve.ts:135).
    let minimum_validity_ms =
        DEFAULT_OAUTH_MINIMUM_VALIDITY_MS.max(min_oauth_validity_ms.unwrap_or(0) as i64);

    // auth/resolve.ts:136 — `expiresSoon(c) = Date.now() + minimumValidityMs >= c.expires`.
    fn expires_soon(credential: &OAuthCredential, minimum_validity_ms: i64) -> bool {
        now_ms() + minimum_validity_ms >= credential.expires
    }

    let mut credential = stored.clone();

    // Valid token → straight to `to_auth`, zero modify/locks
    // (auth/resolve.ts:139 "not expiresSoon" fast path).
    if expires_soon(&credential, minimum_validity_ms) {
        // Optimistic check said expiring; the authoritative check runs under
        // the lock (auth/resolve.ts:142-163). The caller's signal cancels
        // only the lock wait; a started refresh persists (`bde882c74`).
        let minimum = minimum_validity_ms;
        let needs_refresh =
            Arc::new(move |current: &OAuthCredential| expires_soon(current, minimum));
        let Some(post) = refresh_stored_oauth_credential_with_timeout(
            credentials,
            provider_id,
            oauth.clone(),
            needs_refresh,
            signal,
            refresh_timeout,
        )
        .await?
        else {
            return Ok(None); // logged out meanwhile
        };
        credential = post;

        // Explicit callers (e.g. bearer-token export) require the requested
        // minimum after the refresh; the default five-minute path does not
        // impose a contract (auth/resolve.ts:169-171).
        if min_oauth_validity_ms.is_some() && expires_soon(&credential, minimum_validity_ms) {
            return Err(ModelsError::new(
                ModelsErrorCode::Oauth,
                format!("OAuth refresh returned a token that expires too soon for {provider_id}"),
            ));
        }
    }

    to_auth_result(&oauth, &credential, provider_id)
        .await
        .map(Some)
}

/// Derive request auth from a valid OAuth credential (auth/resolve.ts:174-178).
async fn to_auth_result(
    oauth: &Arc<dyn super::types::OAuthAuth>,
    credential: &OAuthCredential,
    provider_id: &str,
) -> Result<AuthResult, ModelsError> {
    oauth
        .to_auth(credential)
        .await
        .map(|auth| AuthResult {
            auth,
            env: None,
            source: Some("OAuth".to_owned()),
        })
        .map_err(|error| {
            ModelsError::with_cause(
                ModelsErrorCode::Oauth,
                format!("OAuth auth derivation failed for {provider_id}"),
                &error.message,
            )
        })
}

async fn resolve_api_key(
    _auth_context: &dyn AuthContext,
    api_key: &dyn super::types::ApiKeyAuth,
    provider_id: &str,
    credential: Option<&ApiKeyCredential>,
) -> Result<Option<AuthResult>, ModelsError> {
    api_key
        .resolve(_auth_context, credential)
        .await
        .map_err(|error| {
            ModelsError::with_cause(
                ModelsErrorCode::Auth,
                format!("API key auth failed for provider {provider_id}"),
                &error.message,
            )
        })
}

async fn read_credential(
    credentials: &Arc<dyn CredentialStore>,
    provider_id: &str,
    signal: &CancellationToken,
) -> Result<Option<Credential>, ModelsError> {
    let options = AuthOperationOptions::with_signal(signal.clone());
    credentials
        .read(provider_id, Some(&options))
        .await
        .map_err(|error| {
            ModelsError::with_cause(
                ModelsErrorCode::Auth,
                format!("Credential store read failed for {provider_id}"),
                &error.message,
            )
        })
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    //! Self-check list (docs/plan/v0.1/T04-rpi-ai-auth.md) resolution-chain assertions;
    //! expected semantics compared item by item against
    //! `packages/ai/src/auth/resolve.ts` @ pi 0.82.1 (2efa728):
    //! an explicit apiKey returns before `readCredential`; a stored credential owns its
    //! provider (no matching handler → undefined, never falls back to ambient); OAuth
    //! expiry is double-checked inside the modify lock, and a failed refresh throws
    //! `ModelsError("oauth")` without falling back
    //! env。

    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use serde_json::Map;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::auth::credential_store::InMemoryCredentialStore;
    use crate::auth::types::{
        ApiKeyAuth, AuthOperationOptions, CredentialInfo, CredentialStore, ModelAuth, ModifyFn,
        OAuthAuth,
    };

    const ENV_VAR: &str = "TEST_RESOLVE_API_KEY";

    // ------------------------------------------------------------------
    // Fakes
    // ------------------------------------------------------------------

    /// Records `env()` calls; `env_values` stands in for the process env.
    struct RecordingAuthContext {
        env_values: HashMap<String, String>,
        env_calls: Mutex<Vec<String>>,
    }

    impl RecordingAuthContext {
        fn with(entries: &[(&str, &str)]) -> Self {
            Self {
                env_values: entries
                    .iter()
                    .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                    .collect(),
                env_calls: Mutex::new(Vec::new()),
            }
        }

        fn empty() -> Self {
            Self::with(&[])
        }

        fn env_calls(&self) -> Vec<String> {
            self.env_calls.lock().expect("env calls").clone()
        }
    }

    #[async_trait::async_trait]
    impl AuthContext for RecordingAuthContext {
        async fn env(&self, name: &str) -> Option<String> {
            self.env_calls
                .lock()
                .expect("env calls")
                .push(name.to_owned());
            self.env_values.get(name).cloned()
        }

        async fn file_exists(&self, _path: &str) -> bool {
            false
        }
    }

    /// Mirrors upstream `envApiKeyAuth.resolve` (auth/helpers.ts): a stored
    /// key wins, otherwise the configured env var resolves through `ctx.env`.
    /// Records whether it was invoked with a stored credential.
    struct FakeApiKeyAuth {
        received_credential: Mutex<Vec<bool>>,
    }

    impl FakeApiKeyAuth {
        fn new() -> Self {
            Self {
                received_credential: Mutex::new(Vec::new()),
            }
        }

        fn received_credential(&self) -> Vec<bool> {
            self.received_credential.lock().expect("received").clone()
        }
    }

    #[async_trait::async_trait]
    impl ApiKeyAuth for FakeApiKeyAuth {
        fn name(&self) -> &str {
            "Test API key"
        }

        async fn resolve(
            &self,
            ctx: &dyn AuthContext,
            credential: Option<&ApiKeyCredential>,
        ) -> Result<Option<AuthResult>, ModelsError> {
            self.received_credential
                .lock()
                .expect("received")
                .push(credential.is_some());
            if let Some(credential) = credential
                && let Some(key) = credential.key.clone().filter(|key| !key.is_empty())
            {
                return Ok(Some(AuthResult {
                    auth: ModelAuth {
                        api_key: Some(key),
                        headers: None,
                        base_url: None,
                    },
                    env: credential.env.clone(),
                    source: Some("stored credential".to_owned()),
                }));
            }
            if let Some(value) = ctx.env(ENV_VAR).await.filter(|value| !value.is_empty()) {
                return Ok(Some(AuthResult {
                    auth: ModelAuth {
                        api_key: Some(value),
                        headers: None,
                        base_url: None,
                    },
                    env: None,
                    source: Some(ENV_VAR.to_owned()),
                }));
            }
            Ok(None)
        }
    }

    /// Counts `refresh` calls; `to_auth` derives `apiKey = access` like the
    /// upstream anthropic OAuth handler.
    ///
    /// `refresh_expires_delta_ms` controls the expiry of the refreshed token
    /// (default: 1 hour, well above the five-minute window).
    ///
    /// When `block_on_refresh` is `true`, refresh signals start then blocks
    /// until [`FakeOAuthAuth::release`] is called — used by timeout/concurrency
    /// tests.
    struct FakeOAuthAuth {
        refresh_calls: AtomicUsize,
        refresh_error: Option<String>,
        refresh_expires_delta_ms: i64,
        block_on_refresh: AtomicBool,
        refresh_started: tokio::sync::Notify,
        refresh_release: tokio::sync::Notify,
        /// Cancelled from inside `refresh` to simulate the caller aborting
        /// while the refresh is in flight (`bde882c74`).
        cancel_on_refresh: Option<CancellationToken>,
    }

    impl FakeOAuthAuth {
        fn succeeding() -> Self {
            Self {
                refresh_calls: AtomicUsize::new(0),
                refresh_error: None,
                refresh_expires_delta_ms: 60 * 60_000,
                block_on_refresh: AtomicBool::new(false),
                refresh_started: tokio::sync::Notify::new(),
                refresh_release: tokio::sync::Notify::new(),
                cancel_on_refresh: None,
            }
        }

        fn failing(message: &str) -> Self {
            Self {
                refresh_calls: AtomicUsize::new(0),
                refresh_error: Some(message.to_owned()),
                refresh_expires_delta_ms: 60 * 60_000,
                block_on_refresh: AtomicBool::new(false),
                refresh_started: tokio::sync::Notify::new(),
                refresh_release: tokio::sync::Notify::new(),
                cancel_on_refresh: None,
            }
        }

        /// Cancels `token` from inside `refresh`, as if the caller aborted
        /// while the refresh was in flight.
        fn cancelling(mut self, token: CancellationToken) -> Self {
            self.cancel_on_refresh = Some(token);
            self
        }

        /// Refreshed token expires in `delta_ms` from now.
        fn with_refresh_expiry(mut self, delta_ms: i64) -> Self {
            self.refresh_expires_delta_ms = delta_ms;
            self
        }

        /// When enabled, refresh signals start then blocks until released.
        fn blocking(self) -> Self {
            self.block_on_refresh.store(true, Ordering::SeqCst);
            self
        }

        fn refresh_calls(&self) -> usize {
            self.refresh_calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl OAuthAuth for FakeOAuthAuth {
        fn name(&self) -> &str {
            "Test OAuth"
        }

        async fn refresh(
            &self,
            credential: &OAuthCredential,
            _signal: Option<&CancellationToken>,
        ) -> Result<OAuthCredential, ModelsError> {
            self.refresh_calls.fetch_add(1, Ordering::SeqCst);
            self.refresh_started.notify_one();
            if self.block_on_refresh.load(Ordering::SeqCst) {
                self.refresh_release.notified().await;
            }
            if let Some(token) = &self.cancel_on_refresh {
                token.cancel();
            }
            match &self.refresh_error {
                Some(message) => Err(ModelsError::new(ModelsErrorCode::Oauth, message.clone())),
                None => Ok(OAuthCredential {
                    refresh: credential.refresh.clone(),
                    access: "refreshed-access".to_owned(),
                    expires: now_ms() + self.refresh_expires_delta_ms,
                    extra: Map::new(),
                }),
            }
        }

        async fn to_auth(&self, credential: &OAuthCredential) -> Result<ModelAuth, ModelsError> {
            Ok(ModelAuth {
                api_key: Some(credential.access.clone()),
                headers: None,
                base_url: None,
            })
        }
    }

    /// `CredentialStore` wrapper counting `read` calls (the explicit-apiKey
    /// path must return before any store read, resolve.ts:54-60).
    struct CountingStore {
        inner: InMemoryCredentialStore,
        reads: AtomicUsize,
    }

    impl CountingStore {
        fn new(inner: InMemoryCredentialStore) -> Self {
            Self {
                inner,
                reads: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl CredentialStore for CountingStore {
        async fn read(
            &self,
            provider_id: &str,
            options: Option<&AuthOperationOptions>,
        ) -> Result<Option<Credential>, ModelsError> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            self.inner.read(provider_id, options).await
        }

        async fn list(
            &self,
            options: Option<&AuthOperationOptions>,
        ) -> Result<Vec<CredentialInfo>, ModelsError> {
            self.inner.list(options).await
        }

        async fn modify(
            &self,
            provider_id: &str,
            f: ModifyFn,
            options: Option<&AuthOperationOptions>,
        ) -> Result<Option<Credential>, ModelsError> {
            self.inner.modify(provider_id, f, options).await
        }

        async fn delete(
            &self,
            provider_id: &str,
            options: Option<&AuthOperationOptions>,
        ) -> Result<(), ModelsError> {
            self.inner.delete(provider_id, options).await
        }
    }

    /// `CredentialStore` wrapper counting `modify` calls (valid tokens must
    /// resolve without touching modify, resolve.ts:965-988).
    struct ModifyCountingStore {
        inner: InMemoryCredentialStore,
        modifies: AtomicUsize,
    }

    impl ModifyCountingStore {
        fn new(inner: InMemoryCredentialStore) -> Self {
            Self {
                inner,
                modifies: AtomicUsize::new(0),
            }
        }

        fn modifies(&self) -> usize {
            self.modifies.load(Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl CredentialStore for ModifyCountingStore {
        async fn read(
            &self,
            provider_id: &str,
            options: Option<&AuthOperationOptions>,
        ) -> Result<Option<Credential>, ModelsError> {
            self.inner.read(provider_id, options).await
        }

        async fn list(
            &self,
            options: Option<&AuthOperationOptions>,
        ) -> Result<Vec<CredentialInfo>, ModelsError> {
            self.inner.list(options).await
        }

        async fn modify(
            &self,
            provider_id: &str,
            f: ModifyFn,
            options: Option<&AuthOperationOptions>,
        ) -> Result<Option<Credential>, ModelsError> {
            self.modifies.fetch_add(1, Ordering::SeqCst);
            self.inner.modify(provider_id, f, options).await
        }

        async fn delete(
            &self,
            provider_id: &str,
            options: Option<&AuthOperationOptions>,
        ) -> Result<(), ModelsError> {
            self.inner.delete(provider_id, options).await
        }
    }

    /// A store whose read/modify always fail — for wrapping failures test.
    struct FailingStore;

    #[async_trait::async_trait]
    impl CredentialStore for FailingStore {
        async fn read(
            &self,
            _provider_id: &str,
            _options: Option<&AuthOperationOptions>,
        ) -> Result<Option<Credential>, ModelsError> {
            Err(ModelsError::new(ModelsErrorCode::Auth, "disk on fire"))
        }

        async fn list(
            &self,
            _options: Option<&AuthOperationOptions>,
        ) -> Result<Vec<CredentialInfo>, ModelsError> {
            Ok(Vec::new())
        }

        async fn modify(
            &self,
            _provider_id: &str,
            _f: ModifyFn,
            _options: Option<&AuthOperationOptions>,
        ) -> Result<Option<Credential>, ModelsError> {
            Err(ModelsError::new(ModelsErrorCode::Auth, "disk on fire"))
        }

        async fn delete(
            &self,
            _provider_id: &str,
            _options: Option<&AuthOperationOptions>,
        ) -> Result<(), ModelsError> {
            Ok(())
        }
    }

    /// Simulates "another process refreshed meanwhile": `modify` swaps in a
    /// fresh credential *before* invoking the callback, so the callback's
    /// authoritative re-check sees an unexpired token (resolve.ts:107).
    struct SwapOnModifyStore {
        inner: InMemoryCredentialStore,
        swap: Mutex<Option<Credential>>,
    }

    impl SwapOnModifyStore {
        fn new(inner: InMemoryCredentialStore, swap: Credential) -> Self {
            Self {
                inner,
                swap: Mutex::new(Some(swap)),
            }
        }
    }

    #[async_trait::async_trait]
    impl CredentialStore for SwapOnModifyStore {
        async fn read(
            &self,
            provider_id: &str,
            options: Option<&AuthOperationOptions>,
        ) -> Result<Option<Credential>, ModelsError> {
            self.inner.read(provider_id, options).await
        }

        async fn list(
            &self,
            options: Option<&AuthOperationOptions>,
        ) -> Result<Vec<CredentialInfo>, ModelsError> {
            self.inner.list(options).await
        }

        async fn modify(
            &self,
            provider_id: &str,
            f: ModifyFn,
            options: Option<&AuthOperationOptions>,
        ) -> Result<Option<Credential>, ModelsError> {
            let swap = self.swap.lock().expect("swap").take();
            if let Some(credential) = swap {
                let swapped = credential.clone();
                self.inner
                    .modify(
                        provider_id,
                        Arc::new(move |_| {
                            let swapped = swapped.clone();
                            Box::pin(async move { Ok(Some(swapped)) })
                        }),
                        options,
                    )
                    .await?;
            }
            self.inner.modify(provider_id, f, options).await
        }

        async fn delete(
            &self,
            provider_id: &str,
            options: Option<&AuthOperationOptions>,
        ) -> Result<(), ModelsError> {
            self.inner.delete(provider_id, options).await
        }
    }

    // ------------------------------------------------------------------
    // Helpers
    // ------------------------------------------------------------------

    fn api_key_credential(key: Option<&str>) -> Credential {
        Credential::ApiKey(ApiKeyCredential {
            key: key.map(str::to_owned),
            env: None,
        })
    }

    fn oauth_credential(access: &str, expires: i64) -> Credential {
        Credential::OAuth(OAuthCredential {
            refresh: "refresh-token".to_owned(),
            access: access.to_owned(),
            expires,
            extra: Map::new(),
        })
    }

    async fn store_with(provider_id: &str, credential: Credential) -> InMemoryCredentialStore {
        let store = InMemoryCredentialStore::new();
        let stored = credential.clone();
        store
            .modify(
                provider_id,
                Arc::new(move |_| {
                    let stored = stored.clone();
                    Box::pin(async move { Ok(Some(stored)) })
                }),
                None,
            )
            .await
            .expect("seed store");
        store
    }

    fn provider_auth(
        api_key: Option<Arc<dyn ApiKeyAuth>>,
        oauth: Option<Arc<dyn OAuthAuth>>,
    ) -> ProviderAuth {
        ProviderAuth { api_key, oauth }
    }

    // ------------------------------------------------------------------
    // Tests
    // ------------------------------------------------------------------

    /// Self-check: explicit key > store. overrides.apiKey returns before
    /// readCredential (resolve.ts:54-60); the stored entry's key shape differs so the
    /// two are distinguishable.
    #[tokio::test]
    async fn explicit_api_key_wins_over_stored_credential() {
        let store = Arc::new(CountingStore::new(
            store_with("test", api_key_credential(Some("stored-key"))).await,
        ));
        let credentials: Arc<dyn CredentialStore> = store.clone();
        let auth_context: Arc<dyn AuthContext> = Arc::new(RecordingAuthContext::empty());
        let auth = provider_auth(Some(Arc::new(FakeApiKeyAuth::new())), None);

        let result = resolve_provider_auth(
            "test",
            &auth,
            &credentials,
            &auth_context,
            Some(&AuthResolutionOverrides {
                api_key: Some("explicit-key".to_owned()),
                ..Default::default()
            }),
        )
        .await
        .expect("resolve")
        .expect("configured");

        assert_eq!(result.auth.api_key.as_deref(), Some("explicit-key"));
        assert_eq!(
            store.reads.load(Ordering::SeqCst),
            0,
            "explicit key path must return before any store read"
        );
    }

    /// Self-check: store hit stops the chain. When a stored key exists, env is not
    /// consulted (the recording AuthContext asserts zero calls); the result comes from
    /// the stored credential.
    #[tokio::test]
    async fn stored_key_wins_and_env_is_never_consulted() {
        let credentials: Arc<dyn CredentialStore> =
            Arc::new(store_with("test", api_key_credential(Some("stored-key"))).await);
        let ctx = Arc::new(RecordingAuthContext::with(&[(ENV_VAR, "env-key")]));
        let auth_context: Arc<dyn AuthContext> = ctx.clone();
        let auth = provider_auth(Some(Arc::new(FakeApiKeyAuth::new())), None);

        let result = resolve_provider_auth("test", &auth, &credentials, &auth_context, None)
            .await
            .expect("resolve")
            .expect("configured");

        assert_eq!(result.auth.api_key.as_deref(), Some("stored-key"));
        assert_eq!(result.source.as_deref(), Some("stored credential"));
        assert!(
            ctx.env_calls().is_empty(),
            "stored key hit must not consult env: {:?}",
            ctx.env_calls()
        );
    }

    /// "Hit stops the chain" boundary: when a stored credential has no key, the handler
    /// still reaches `credential: Some(..)` through the stored path and resolves via
    /// `ctx.env` (upstream envApiKeyAuth semantics) — the bottom ambient branch is not
    /// taken (credential `None` is what denotes ambient).
    #[tokio::test]
    async fn keyless_stored_credential_still_resolves_env_through_handler() {
        let credentials: Arc<dyn CredentialStore> =
            Arc::new(store_with("test", api_key_credential(None)).await);
        let auth_context: Arc<dyn AuthContext> =
            Arc::new(RecordingAuthContext::with(&[(ENV_VAR, "env-key")]));
        let api_key = Arc::new(FakeApiKeyAuth::new());
        let auth = provider_auth(Some(api_key.clone()), None);

        let result = resolve_provider_auth("test", &auth, &credentials, &auth_context, None)
            .await
            .expect("resolve")
            .expect("configured");

        assert_eq!(result.auth.api_key.as_deref(), Some("env-key"));
        assert_eq!(result.source.as_deref(), Some(ENV_VAR));
        assert_eq!(
            api_key.received_credential(),
            vec![true],
            "handler must be invoked through the stored path (credential present)"
        );
    }

    /// Upstream definition of "hit stops the chain" (resolve.ts:71): a stored
    /// credential type with no matching handler → `undefined`, never falls back to
    /// ambient/env.
    #[tokio::test]
    async fn stored_credential_without_matching_handler_returns_none_no_env_fallback() {
        let credentials: Arc<dyn CredentialStore> =
            Arc::new(store_with("test", api_key_credential(Some("stored-key"))).await);
        let ctx = Arc::new(RecordingAuthContext::with(&[(ENV_VAR, "env-key")]));
        let auth_context: Arc<dyn AuthContext> = ctx.clone();
        // Provider has no api_key handler for the stored api_key credential.
        let auth = provider_auth(None, None);

        let result = resolve_provider_auth("test", &auth, &credentials, &auth_context, None)
            .await
            .expect("resolve");

        assert_eq!(result, None);
        assert!(
            ctx.env_calls().is_empty(),
            "no ambient fallback after a store hit: {:?}",
            ctx.env_calls()
        );
    }

    /// Self-check: ambient fallback. Store empty → env variable hit, source is the
    /// variable name.
    #[tokio::test]
    async fn ambient_env_resolves_when_store_is_empty() {
        let credentials: Arc<dyn CredentialStore> = Arc::new(InMemoryCredentialStore::new());
        let auth_context: Arc<dyn AuthContext> =
            Arc::new(RecordingAuthContext::with(&[(ENV_VAR, "env-key")]));
        let api_key = Arc::new(FakeApiKeyAuth::new());
        let auth = provider_auth(Some(api_key.clone()), None);

        let result = resolve_provider_auth("test", &auth, &credentials, &auth_context, None)
            .await
            .expect("resolve")
            .expect("configured");

        assert_eq!(result.auth.api_key.as_deref(), Some("env-key"));
        assert_eq!(result.source.as_deref(), Some(ENV_VAR));
        assert_eq!(
            api_key.received_credential(),
            vec![false],
            "ambient path passes no credential"
        );
    }

    /// Self-check: a failed OAuth refresh throws `ModelsError("oauth")` and never
    /// silently falls back to the env key; the credential stays in the store for
    /// re-login.
    #[tokio::test]
    async fn oauth_refresh_failure_errors_without_env_fallback() {
        let expired = oauth_credential("expired-access", 0);
        let store = store_with("test", expired.clone()).await;
        let credentials: Arc<dyn CredentialStore> = Arc::new(store);
        let auth_context: Arc<dyn AuthContext> =
            Arc::new(RecordingAuthContext::with(&[(ENV_VAR, "env-key")]));
        let oauth = Arc::new(FakeOAuthAuth::failing("invalid_grant"));
        let auth = provider_auth(Some(Arc::new(FakeApiKeyAuth::new())), Some(oauth.clone()));

        let error = resolve_provider_auth("test", &auth, &credentials, &auth_context, None)
            .await
            .expect_err("refresh failure must error");

        assert_eq!(error.code, ModelsErrorCode::Oauth);
        assert!(
            error.message.contains("OAuth refresh failed for test"),
            "unexpected message: {}",
            error.message
        );
        assert!(
            !error.message.contains("env-key"),
            "no env fallback after a failed refresh"
        );
        assert_eq!(oauth.refresh_calls(), 1);
        // Credential preserved (the modify callback error does not write back), for re-login.
        assert_eq!(
            credentials.read("test", None).await.expect("read"),
            Some(expired)
        );
    }

    /// Self-check: double check inside the modify lock — if the lock finds the
    /// credential already refreshed by another process (not expired), refresh is not
    /// called; the in-lock credential is used directly.
    #[tokio::test]
    async fn oauth_refresh_is_double_checked_under_the_store_lock() {
        let expired = oauth_credential("expired-access", 0);
        let fresh = oauth_credential("fresh-access", now_ms() + 600_000); // > 5 min window
        let store = SwapOnModifyStore::new(store_with("test", expired).await, fresh);
        let credentials: Arc<dyn CredentialStore> = Arc::new(store);
        let auth_context: Arc<dyn AuthContext> = Arc::new(RecordingAuthContext::empty());
        let oauth = Arc::new(FakeOAuthAuth::succeeding());
        let auth = provider_auth(None, Some(oauth.clone()));

        let result = resolve_provider_auth("test", &auth, &credentials, &auth_context, None)
            .await
            .expect("resolve")
            .expect("configured");

        assert_eq!(result.auth.api_key.as_deref(), Some("fresh-access"));
        assert_eq!(result.source.as_deref(), Some("OAuth"));
        assert_eq!(
            oauth.refresh_calls(),
            0,
            "double check under the lock must skip refresh"
        );
    }

    // ------------------------------------------------------------------
    // T21b required tests (blueprint: models-runtime.test.ts L887-1038)
    // ------------------------------------------------------------------

    /// `refreshes oauth credentials with less than five minutes remaining`
    /// (models-runtime.test.ts:887): 1 minute remaining → trigger refresh
    /// exactly once.
    #[tokio::test]
    async fn refreshes_oauth_credentials_with_less_than_five_minutes_remaining() {
        let credentials: Arc<dyn CredentialStore> = Arc::new(
            store_with(
                "p1",
                oauth_credential("old-token", now_ms() + 60_000), // 1 min remaining
            )
            .await,
        );
        let auth_context: Arc<dyn AuthContext> = Arc::new(RecordingAuthContext::empty());
        let oauth = Arc::new(FakeOAuthAuth::succeeding());
        let auth = provider_auth(None, Some(oauth.clone()));

        let result = resolve_provider_auth("p1", &auth, &credentials, &auth_context, None)
            .await
            .expect("resolve")
            .expect("configured");

        assert_eq!(result.auth.api_key.as_deref(), Some("refreshed-access"));
        assert_eq!(oauth.refresh_calls(), 1);
    }

    /// `honors a caller's longer OAuth minimum validity` (L907):
    /// min_oauth_validity_ms=30min + 10min remaining → trigger refresh.
    #[tokio::test]
    async fn honors_a_callers_longer_oauth_minimum_validity() {
        let credentials: Arc<dyn CredentialStore> = Arc::new(
            store_with(
                "p1",
                oauth_credential("old-token", now_ms() + 10 * 60_000), // 10 min remaining
            )
            .await,
        );
        let auth_context: Arc<dyn AuthContext> = Arc::new(RecordingAuthContext::empty());
        let oauth = Arc::new(FakeOAuthAuth::succeeding());
        let auth = provider_auth(None, Some(oauth.clone()));

        let result = resolve_provider_auth(
            "p1",
            &auth,
            &credentials,
            &auth_context,
            Some(&AuthResolutionOverrides {
                min_oauth_validity_ms: Some(30 * 60_000), // 30 min
                ..Default::default()
            }),
        )
        .await
        .expect("resolve")
        .expect("configured");

        assert_eq!(result.auth.api_key.as_deref(), Some("refreshed-access"));
        assert_eq!(oauth.refresh_calls(), 1);
    }

    /// `rejects_with_code_oauth_when_refresh_fails_preserving_the_stored_credential`
    /// (L927): refresh error → code=oauth; stored credential preserved.
    #[tokio::test]
    async fn rejects_with_code_oauth_when_refresh_fails_preserving_the_stored_credential() {
        let expired = oauth_credential("old", 0);
        let credentials: Arc<dyn CredentialStore> =
            Arc::new(store_with("p1", expired.clone()).await);
        let auth_context: Arc<dyn AuthContext> = Arc::new(RecordingAuthContext::empty());
        let oauth = Arc::new(FakeOAuthAuth::failing("invalid_grant"));
        let auth = provider_auth(None, Some(oauth.clone()));

        let error = resolve_provider_auth("p1", &auth, &credentials, &auth_context, None)
            .await
            .expect_err("refresh failure must error");

        assert_eq!(error.code, ModelsErrorCode::Oauth);
        assert!(
            error.message.contains("OAuth refresh failed for p1"),
            "unexpected message: {error}"
        );
        assert_eq!(oauth.refresh_calls(), 1);
        // Credential preserved for retry / re-login
        assert_eq!(
            credentials.read("p1", None).await.expect("read"),
            Some(expired)
        );
    }

    /// `serializes_concurrent_oauth_refreshes_through_store_modify_no_double_refresh`
    /// (L943): two concurrent resolutions → refresh only once.
    #[tokio::test]
    async fn serializes_concurrent_oauth_refreshes_through_store_modify_no_double_refresh() {
        let credentials: Arc<dyn CredentialStore> =
            Arc::new(store_with("p1", oauth_credential("old", 0)).await);
        let auth_context: Arc<dyn AuthContext> = Arc::new(RecordingAuthContext::empty());
        let oauth = Arc::new(FakeOAuthAuth::succeeding());
        let auth = provider_auth(None, Some(oauth.clone()));

        let (a, b) = tokio::join!(
            resolve_provider_auth("p1", &auth, &credentials, &auth_context, None),
            resolve_provider_auth("p1", &auth, &credentials, &auth_context, None),
        );

        let a = a.expect("resolve a").expect("configured");
        let b = b.expect("resolve b").expect("configured");
        assert_eq!(a.auth.api_key.as_deref(), Some("refreshed-access"));
        assert_eq!(b.auth.api_key.as_deref(), Some("refreshed-access"));
        assert_eq!(oauth.refresh_calls(), 1, "no double refresh");
    }

    /// `valid_oauth_tokens_resolve_without_touching_modify` (L965):
    /// >5min validity → modify 0 times (counting store).
    #[tokio::test]
    async fn valid_oauth_tokens_resolve_without_touching_modify() {
        let store = store_with(
            "p1",
            oauth_credential("valid", now_ms() + 10 * 60_000), // 10 min
        )
        .await;
        let counting = Arc::new(ModifyCountingStore::new(store));
        let credentials: Arc<dyn CredentialStore> = counting.clone();
        let auth_context: Arc<dyn AuthContext> = Arc::new(RecordingAuthContext::empty());
        let oauth = Arc::new(FakeOAuthAuth::succeeding());
        let auth = provider_auth(None, Some(oauth.clone()));

        let result = resolve_provider_auth("p1", &auth, &credentials, &auth_context, None)
            .await
            .expect("resolve")
            .expect("configured");

        assert_eq!(result.auth.api_key.as_deref(), Some("valid"));
        assert_eq!(counting.modifies(), 0, "valid token must not touch modify");
    }

    /// `keeps_the_underlying_reason_in_wrapped_oauth_refresh_errors`
    /// (L1018): message contains the underlying cause.
    #[tokio::test]
    async fn keeps_the_underlying_reason_in_wrapped_oauth_refresh_errors() {
        let credentials: Arc<dyn CredentialStore> =
            Arc::new(store_with("p1", oauth_credential("old", 0)).await);
        let auth_context: Arc<dyn AuthContext> = Arc::new(RecordingAuthContext::empty());
        let oauth = Arc::new(FakeOAuthAuth::failing(
            "token refresh failed (400): invalid_grant",
        ));
        let auth = provider_auth(None, Some(oauth.clone()));

        let error = resolve_provider_auth("p1", &auth, &credentials, &auth_context, None)
            .await
            .expect_err("must error");

        assert!(
            error
                .message
                .contains("token refresh failed (400): invalid_grant"),
            "message must keep underlying reason: {error}"
        );
    }

    /// `wraps_credential_store_failures_in_models_error` (L990):
    /// read/modify failure → code=auth.
    #[tokio::test]
    async fn wraps_credential_store_failures_in_models_error() {
        let credentials: Arc<dyn CredentialStore> = Arc::new(FailingStore);
        let auth_context: Arc<dyn AuthContext> = Arc::new(RecordingAuthContext::empty());
        let auth = provider_auth(
            Some(Arc::new(FakeApiKeyAuth::new())), // triggers readCredential
            None,
        );

        // read failure → code=auth
        let error = resolve_provider_auth("p1", &auth, &credentials, &auth_context, None)
            .await
            .expect_err("read failure must error");
        assert_eq!(error.code, ModelsErrorCode::Auth);

        // modify failure during refresh → code=auth
        let oauth = Arc::new(FakeOAuthAuth::succeeding());
        let oauth_auth = provider_auth(None, Some(oauth.clone()));
        let error = resolve_provider_auth("p1", &oauth_auth, &credentials, &auth_context, None)
            .await
            .expect_err("modify failure must error");
        assert_eq!(error.code, ModelsErrorCode::Auth);
    }

    /// `persists an OAuth refresh that started before the request was
    /// cancelled` (models-runtime.test.ts @ `bde882c74`): the caller rejects
    /// with the abort, but the rotated credential is still written.
    #[tokio::test]
    async fn persists_an_oauth_refresh_that_started_before_cancellation() {
        let credentials: Arc<dyn CredentialStore> =
            Arc::new(store_with("p1", oauth_credential("old-access", 0)).await);
        let token = CancellationToken::new();
        let oauth = Arc::new(FakeOAuthAuth::succeeding().cancelling(token.clone()));
        let auth = provider_auth(None, Some(oauth.clone()));
        let auth_context: Arc<dyn AuthContext> = Arc::new(RecordingAuthContext::empty());
        let overrides = AuthResolutionOverrides {
            signal: Some(token.clone()),
            ..Default::default()
        };

        let error =
            resolve_provider_auth("p1", &auth, &credentials, &auth_context, Some(&overrides))
                .await
                .expect_err("a cancelled caller rejects");
        assert_eq!(error.code, ModelsErrorCode::Aborted);
        assert_eq!(oauth.refresh_calls(), 1);

        // The detached refresh persists the rotated credential.
        for _ in 0..200 {
            if let Some(Credential::OAuth(stored)) =
                credentials.read("p1", None).await.expect("read")
                && stored.access == "refreshed-access"
            {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        panic!("rotated credential was not persisted");
    }

    /// `bounds_oauth_refreshes_and_releases_the_credential_store_lock_after_timeout`:
    /// inject 50ms timeout, refresh hangs → reject code=oauth; a subsequent
    /// modify completes immediately (lock released).
    #[tokio::test]
    async fn bounds_oauth_refreshes_and_releases_the_credential_store_lock_after_timeout() {
        // We must call resolve_stored_oauth directly with an injected timeout
        // since resolve_provider_auth hardcodes DEFAULT_OAUTH_REFRESH_TIMEOUT_MS.
        let credentials: Arc<dyn CredentialStore> =
            Arc::new(store_with("p1", oauth_credential("old", 0)).await);
        let oauth: Arc<dyn OAuthAuth> = Arc::new(FakeOAuthAuth::succeeding().blocking());

        // 50ms timeout — the refresh hangs, so this must time out.
        let stored_cred = oauth_credential("old", 0);
        let Credential::OAuth(stored_oauth) = stored_cred else {
            panic!("expected OAuth credential");
        };
        let error = resolve_stored_oauth(
            &credentials,
            "p1",
            oauth.clone(),
            &stored_oauth,
            &CancellationToken::new(),
            None,
            std::time::Duration::from_millis(50),
        )
        .await
        .expect_err("refresh must time out");

        assert_eq!(error.code, ModelsErrorCode::Oauth);
        assert!(error.message.contains("OAuth refresh failed for p1"));

        // Lock released: a subsequent modify completes immediately.
        let result = credentials
            .modify(
                "p1",
                Arc::new(|_| {
                    Box::pin(async {
                        Ok(Some(Credential::ApiKey(ApiKeyCredential {
                            key: Some("post-timeout".to_owned()),
                            env: None,
                        })))
                    })
                }),
                None,
            )
            .await
            .expect("modify after timeout");
        assert!(result.is_some());
    }

    /// `explicit_min_oauth_validity_rejects_a_refreshed_token_that_expires_too_soon`:
    /// explicit min_oauth_validity_ms, refresh returns a token still below
    /// the minimum → "expires too soon".
    #[tokio::test]
    async fn explicit_min_oauth_validity_rejects_a_refreshed_token_that_expires_too_soon() {
        let credentials: Arc<dyn CredentialStore> =
            Arc::new(store_with("p1", oauth_credential("old", 0)).await);
        let auth_context: Arc<dyn AuthContext> = Arc::new(RecordingAuthContext::empty());
        // Refresh returns a token expiring in 1 min — below the explicit 30 min floor.
        let oauth = Arc::new(FakeOAuthAuth::succeeding().with_refresh_expiry(60_000));
        let auth = provider_auth(None, Some(oauth.clone()));

        let error = resolve_provider_auth(
            "p1",
            &auth,
            &credentials,
            &auth_context,
            Some(&AuthResolutionOverrides {
                min_oauth_validity_ms: Some(30 * 60_000),
                ..Default::default()
            }),
        )
        .await
        .expect_err("must reject");

        assert_eq!(error.code, ModelsErrorCode::Oauth);
        assert!(error.message.contains("expires too soon"));
    }
}

#[cfg(test)]
mod tests_credential_queue {
    //! T21b test 10 & 11: credential store queue cancellation + login started.

    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use tokio_util::sync::CancellationToken;

    use super::super::credential_store::InMemoryCredentialStore;
    use super::super::types::{AuthOperationOptions, Credential, CredentialStore};

    /// `cancels_queued_credential_mutations_without_running_them_later`
    /// (L704): first modify holds the lock (blocked), second modify queued
    /// then cancelled → second's task never runs; first completes normally.
    #[tokio::test]
    async fn cancels_queued_credential_mutations_without_running_them_later() {
        let store = Arc::new(InMemoryCredentialStore::new());

        // First modify: holds the lock until released.
        let release_first = Arc::new(tokio::sync::Notify::new());
        let first_started = Arc::new(tokio::sync::Notify::new());

        let first_started_inner = first_started.clone();
        let release_first_inner = release_first.clone();
        let first = {
            let store = store.clone();
            tokio::spawn(async move {
                store
                    .modify(
                        "p1",
                        Arc::new(move |_| {
                            let release = release_first_inner.clone();
                            let started = first_started_inner.clone();
                            Box::pin(async move {
                                started.notify_one();
                                release.notified().await;
                                Ok(Some(Credential::ApiKey(
                                    super::super::types::ApiKeyCredential {
                                        key: Some("first".to_owned()),
                                        env: None,
                                    },
                                )))
                            })
                        }),
                        None,
                    )
                    .await
            })
        };

        // Wait until the first modify has the lock and is blocked.
        first_started.notified().await;

        // Second modify: queued behind the first, then cancelled.
        let second_ran = Arc::new(AtomicBool::new(false));
        let second_ran_inner = second_ran.clone();
        let token = CancellationToken::new();
        let second = {
            let store = store.clone();
            let token = token.clone();
            tokio::spawn(async move {
                store
                    .modify(
                        "p1",
                        Arc::new(move |_| {
                            let ran = second_ran_inner.clone();
                            Box::pin(async move {
                                ran.store(true, Ordering::SeqCst);
                                Ok(Some(Credential::ApiKey(
                                    super::super::types::ApiKeyCredential {
                                        key: Some("second".to_owned()),
                                        env: None,
                                    },
                                )))
                            })
                        }),
                        Some(&AuthOperationOptions::with_signal(token)),
                    )
                    .await
            })
        };

        // Let the second task's cancel future get polled.
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;

        token.cancel();

        // Second must reject with aborted.
        let second_result = second.await.expect("join");
        assert!(
            second_result.is_err(),
            "second modify must reject after cancellation"
        );
        let err = second_result.unwrap_err();
        assert_eq!(err.code, super::ModelsErrorCode::Aborted);

        // Release the first; it should complete.
        release_first.notify_one();
        first.await.expect("join").expect("first modify");

        // Give the task queue a chance to drain.
        tokio::task::yield_now().await;

        // Second's task never ran.
        assert!(
            !second_ran.load(Ordering::SeqCst),
            "second modify task must never run"
        );

        // Store has the first credential.
        let stored = store.read("p1", None).await.expect("read");
        assert!(stored.is_some());
    }
}

#[cfg(test)]
mod tests_file_queue {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use tokio_util::sync::CancellationToken;

    use super::super::file_store::FileCredentialStore;
    use super::super::types::{
        ApiKeyCredential, AuthOperationOptions, Credential, CredentialStore,
    };

    #[tokio::test]
    async fn cancels_queued_credential_mutations_without_running_them_later_file_store() {
        let store = Arc::new(FileCredentialStore::in_memory(HashMap::new()));

        let release_first = Arc::new(tokio::sync::Notify::new());
        let first_started = Arc::new(tokio::sync::Notify::new());

        let release_first_inner = release_first.clone();
        let first_started_inner = first_started.clone();
        let first = {
            let store = store.clone();
            tokio::spawn(async move {
                store
                    .modify(
                        "p1",
                        {
                            let release = release_first_inner.clone();
                            let started = first_started_inner.clone();
                            Arc::new(move |_| {
                                let release = release.clone();
                                let started = started.clone();
                                Box::pin(async move {
                                    started.notify_one();
                                    release.notified().await;
                                    Ok(Some(Credential::ApiKey(ApiKeyCredential {
                                        key: Some("first".to_owned()),
                                        env: None,
                                    })))
                                })
                            })
                        },
                        None,
                    )
                    .await
            })
        };

        first_started.notified().await;

        let second_ran = Arc::new(AtomicBool::new(false));
        let second_ran_inner = second_ran.clone();
        let token = CancellationToken::new();
        let second = {
            let store = store.clone();
            let token = token.clone();
            tokio::spawn(async move {
                store
                    .modify(
                        "p1",
                        {
                            let ran = second_ran_inner.clone();
                            Arc::new(move |_| {
                                let ran = ran.clone();
                                Box::pin(async move {
                                    ran.store(true, Ordering::SeqCst);
                                    Ok(Some(Credential::ApiKey(ApiKeyCredential {
                                        key: Some("second".to_owned()),
                                        env: None,
                                    })))
                                })
                            })
                        },
                        Some(&AuthOperationOptions::with_signal(token)),
                    )
                    .await
            })
        };

        tokio::task::yield_now().await;
        tokio::task::yield_now().await;

        token.cancel();

        let second_result = second.await.expect("join");
        assert!(second_result.is_err());
        assert_eq!(
            second_result.unwrap_err().code,
            super::ModelsErrorCode::Aborted
        );

        release_first.notify_one();
        first.await.expect("join").expect("first");

        tokio::task::yield_now().await;
        assert!(!second_ran.load(Ordering::SeqCst));
    }
}
