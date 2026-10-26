//! Port of the Anthropic SDK workload-identity-federation credential provider
//! (`@anthropic-ai/sdk` `lib/credentials/oidc-federation.js` +
//! `lib/credentials/token-cache.js` + `lib/credentials/types.js`), wired into
//! the provider auth chain by `packages/ai/src/providers/anthropic.ts` @ pi
//! abe508e1b (v1.0.0, `a9424cd43`).
//!
//! rpi has no Anthropic SDK, so the RFC 7523 jwt-bearer exchange, the
//! two-tier token cache (120s advisory / 30s mandatory) and the 401
//! invalidation are ported here. Intentional differences: the SDK's optional
//! on-disk credential cache (`credentials_path`) is not ported — upstream pi
//! configures no path; error-text formatting is kept verbatim where it is
//! asserted (assertion size, 401 hint, response validation).
//!
//! The assertion is re-read from its file on **every** exchange, so rotated
//! projected tokens (Kubernetes service accounts) keep working.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use futures::FutureExt;
use futures::future::Shared;
use serde_json::{Map, Value, json};
use tokio::sync::Mutex as AsyncMutex;

use super::env_keys::{
    ANTHROPIC_FEDERATION_RULE_ID_ENV, ANTHROPIC_IDENTITY_TOKEN_FILE_ENV,
    ANTHROPIC_ORGANIZATION_ID_ENV, ANTHROPIC_SERVICE_ACCOUNT_ID_ENV, ANTHROPIC_WORKSPACE_ID_ENV,
};
use super::resolve::{ModelsError, ModelsErrorCode};
use super::types::BoxFutureSend;
use crate::types::{ProviderEnv, ProviderHeaders};
use crate::utils::provider_env::get_provider_env_value;

/// `GRANT_TYPE_JWT_BEARER`.
const GRANT_TYPE_JWT_BEARER: &str = "urn:ietf:params:oauth:grant-type:jwt-bearer";
/// `TOKEN_ENDPOINT`.
const TOKEN_ENDPOINT: &str = "/v1/oauth/token";
/// `OAUTH_API_BETA_HEADER` — also appended to federated API requests.
pub const OAUTH_API_BETA_HEADER: &str = "oauth-2025-04-20";
/// `FEDERATION_BETA_HEADER` — routes the exchange to the federation service.
const FEDERATION_BETA_HEADER: &str = "oidc-federation-2026-04-01";
/// `ADVISORY_REFRESH_THRESHOLD_IN_SECONDS`.
const ADVISORY_REFRESH_THRESHOLD_SECS: i64 = 120;
/// `MANDATORY_REFRESH_THRESHOLD_IN_SECONDS`.
const MANDATORY_REFRESH_THRESHOLD_SECS: i64 = 30;
/// `ADVISORY_REFRESH_BACKOFF_IN_SECONDS`.
const ADVISORY_REFRESH_BACKOFF_SECS: i64 = 5;
/// The token endpoint enforces a 16 KiB assertion limit.
const MAX_IDENTITY_TOKEN_BYTES: usize = 16 * 1024;
/// `MAX_TOKEN_RESPONSE_BYTES`.
const MAX_TOKEN_RESPONSE_BYTES: usize = 1 << 20;
/// `MAX_ERROR_BODY_CHARS`.
const MAX_ERROR_BODY_CHARS: usize = 2000;
/// HTTP timeout for the exchange (the SDK's fetch has no explicit timeout;
/// rpi bounds it so a hung federation endpoint cannot stall a request).
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(30);

fn error(message: impl Into<String>) -> ModelsError {
    ModelsError::new(ModelsErrorCode::Oauth, message.into())
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

/// Workload identity federation config resolved from the provider
/// environment (`getAnthropicFederation`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnthropicFederationConfig {
    /// Model base URL the exchange and API requests go to.
    pub base_url: String,
    pub federation_rule_id: String,
    pub organization_id: String,
    pub identity_token_file: String,
    /// Optional `ANTHROPIC_SERVICE_ACCOUNT_ID`.
    pub service_account_id: Option<String>,
    /// Optional `ANTHROPIC_WORKSPACE_ID`.
    pub workspace_id: Option<String>,
}

impl AnthropicFederationConfig {
    fn cache_key(&self) -> String {
        format!(
            "{}\u{0}{}\u{0}{}\u{0}{}\u{0}{}\u{0}{}",
            self.base_url,
            self.federation_rule_id,
            self.organization_id,
            self.identity_token_file,
            self.service_account_id.as_deref().unwrap_or_default(),
            self.workspace_id.as_deref().unwrap_or_default(),
        )
    }
}

/// `hasRequestAuth(apiKey, headers)`.
fn has_request_auth(api_key: Option<&str>, headers: Option<&ProviderHeaders>) -> bool {
    if api_key.is_some() {
        return true;
    }
    let Some(headers) = headers else {
        return false;
    };
    headers.iter().any(|(key, value)| {
        matches!(
            key.to_ascii_lowercase().as_str(),
            "authorization" | "x-api-key" | "cf-aig-authorization"
        ) && value.as_ref().is_some_and(|value| !value.trim().is_empty())
    })
}

/// `getAnthropicFederation` — only for the `anthropic` provider and only when
/// no key or auth header was resolved; all three required variables must be
/// present.
pub fn get_anthropic_federation(
    model_provider: &str,
    base_url: &str,
    api_key: Option<&str>,
    headers: Option<&ProviderHeaders>,
    env: Option<&ProviderEnv>,
) -> Option<AnthropicFederationConfig> {
    if model_provider != "anthropic" || has_request_auth(api_key, headers) {
        return None;
    }
    let federation_rule_id = get_provider_env_value(ANTHROPIC_FEDERATION_RULE_ID_ENV, env)?;
    let organization_id = get_provider_env_value(ANTHROPIC_ORGANIZATION_ID_ENV, env)?;
    let identity_token_file = get_provider_env_value(ANTHROPIC_IDENTITY_TOKEN_FILE_ENV, env)?;
    Some(AnthropicFederationConfig {
        base_url: base_url.to_owned(),
        federation_rule_id,
        organization_id,
        identity_token_file,
        service_account_id: get_provider_env_value(ANTHROPIC_SERVICE_ACCOUNT_ID_ENV, env),
        workspace_id: get_provider_env_value(ANTHROPIC_WORKSPACE_ID_ENV, env),
    })
}

/// `requireSecureTokenEndpoint` — rejects non-https endpoints except loopback.
pub fn require_secure_token_endpoint(base_url: &str) -> Result<(), ModelsError> {
    if base_url.is_empty() {
        return Ok(());
    }
    let parsed = url::Url::parse(base_url).map_err(|parse_error| {
        error(format!(
            "Invalid token endpoint base URL \"{base_url}\": {parse_error}"
        ))
    })?;
    if parsed.scheme() == "https" {
        return Ok(());
    }
    let host = parsed
        .host_str()
        .unwrap_or_default()
        .to_ascii_lowercase()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_owned();
    if parsed.scheme() == "http" && matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1") {
        return Ok(());
    }
    Err(error(format!(
        "Refusing to send credential over non-https token endpoint \"{base_url}\""
    )))
}

/// `readSensitiveField` truncation for raw error strings.
fn truncate_with_suffix(text: &str) -> String {
    if text.chars().count() <= MAX_ERROR_BODY_CHARS {
        return text.to_owned();
    }
    let kept: String = text.chars().take(MAX_ERROR_BODY_CHARS).collect();
    let remaining = text.chars().count() - MAX_ERROR_BODY_CHARS;
    format!("{kept}... <{remaining} more chars>")
}

/// `redactSensitive` — RFC 6749 §5.2 error fields for objects, truncation for
/// raw strings, `null` otherwise.
fn redact_sensitive(body: &Value) -> Value {
    match body {
        Value::Null => Value::Null,
        Value::String(text) => match serde_json::from_str::<Value>(text) {
            Ok(parsed) => {
                Value::String(serde_json::to_string(&redact_sensitive(&parsed)).unwrap_or_default())
            }
            Err(_) => Value::String(truncate_with_suffix(text)),
        },
        Value::Object(map) => {
            let mut out = Map::new();
            for key in ["error", "error_description", "error_uri"] {
                if let Some(value) = map.get(key) {
                    out.insert(key.to_owned(), value.clone());
                }
            }
            Value::Object(out)
        }
        _ => Value::Null,
    }
}

/// `redactSensitive` applied to an error body read as text.
fn redact_token_body(text: &str) -> String {
    match serde_json::from_str::<Value>(text) {
        Ok(parsed) => serde_json::to_string(&redact_sensitive(&parsed)).unwrap_or_default(),
        Err(_) => {
            serde_json::to_string(&Value::String(truncate_with_suffix(text))).unwrap_or_default()
        }
    }
}

/// `identityTokenFromFile` — read the JWT, trim, reject empty.
async fn read_identity_token(path: &str) -> Result<String, ModelsError> {
    if path.is_empty() {
        return Err(error("Identity token file path is empty"));
    }
    let content = tokio::fs::read_to_string(path)
        .await
        .map_err(|read_error| {
            error(format!(
                "Failed to read identity token file at {path}: {read_error}"
            ))
        })?;
    let token = content.trim();
    if token.is_empty() {
        return Err(error(format!("Identity token file at {path} is empty")));
    }
    Ok(token.to_owned())
}

/// `AccessToken` — `expiresAt` is unix epoch seconds (always known for
/// federation grants).
#[derive(Debug, Clone)]
struct AccessToken {
    token: String,
    expires_at: i64,
}

/// One jwt-bearer exchange (`oidcFederationProvider` body).
async fn exchange_identity_token(
    config: &AnthropicFederationConfig,
) -> Result<AccessToken, ModelsError> {
    require_secure_token_endpoint(&config.base_url)?;
    let assertion = read_identity_token(&config.identity_token_file).await?;
    if assertion.len() > MAX_IDENTITY_TOKEN_BYTES {
        return Err(error(format!(
            "Identity token is {} KiB, exceeds the 16 KiB assertion limit",
            assertion.len().div_ceil(1024)
        )));
    }
    let mut body = json!({
        "grant_type": GRANT_TYPE_JWT_BEARER,
        "assertion": assertion,
        "federation_rule_id": config.federation_rule_id,
        "organization_id": config.organization_id,
    });
    if let Some(service_account_id) = config
        .service_account_id
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        body["service_account_id"] = json!(service_account_id);
    }
    if let Some(workspace_id) = config
        .workspace_id
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        body["workspace_id"] = json!(workspace_id);
    }
    let url = format!("{}{TOKEN_ENDPOINT}", config.base_url.trim_end_matches('/'));
    let response = reqwest::Client::new()
        .post(&url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header(
            "anthropic-beta",
            format!("{OAUTH_API_BETA_HEADER},{FEDERATION_BETA_HEADER}"),
        )
        .header("user-agent", rpi_user_agent())
        .json(&body)
        .timeout(EXCHANGE_TIMEOUT)
        .send()
        .await
        .map_err(|send_error| {
            error(format!(
                "Failed to reach token endpoint {url}: {send_error}"
            ))
        })?;
    let status = response.status();
    let request_id = response
        .headers()
        .get("Request-Id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let text = response.text().await.unwrap_or_default();
    let text = if text.len() > MAX_TOKEN_RESPONSE_BYTES {
        String::from_utf8_lossy(&text.as_bytes()[..MAX_TOKEN_RESPONSE_BYTES]).into_owned()
    } else {
        text
    };
    if !status.is_success() {
        let request_id_suffix = request_id
            .as_deref()
            .map(|request_id| format!(" (request-id {request_id})"))
            .unwrap_or_default();
        let mut message = format!(
            "Token exchange failed with status {}{request_id_suffix}: {}",
            status.as_u16(),
            redact_token_body(&text)
        );
        if status.as_u16() == 401 {
            message.push_str(" Ensure your federation rule matches your identity token. ");
            if config
                .workspace_id
                .as_deref()
                .is_none_or(|workspace_id| workspace_id.is_empty())
            {
                message.push_str(
                    "If your federation rule is scoped to multiple workspaces, set the ANTHROPIC_WORKSPACE_ID environment variable, the 'workspace_id' config key, or the `workspaceId` option. ",
                );
            }
            message.push_str(
                "View your authentication events in the Workload identity page of Claude Console for more details.",
            );
        }
        return Err(error(message));
    }
    let data: Value = serde_json::from_str(&text).map_err(|_| {
        error(format!(
            "Token endpoint returned non-JSON response (status {})",
            status.as_u16()
        ))
    })?;
    let access_token = data
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| {
            error(format!(
                "Token endpoint response missing access_token: {}",
                redact_token_body(&text)
            ))
        })?;
    if let Some(token_type) = data.get("token_type").and_then(Value::as_str)
        && !token_type.eq_ignore_ascii_case("bearer")
    {
        return Err(error(format!(
            "Token endpoint response: unsupported token_type \"{token_type}\" (want Bearer)"
        )));
    }
    let expires_in = data
        .get("expires_in")
        .and_then(Value::as_f64)
        .filter(|expires_in| expires_in.is_finite())
        .ok_or_else(|| {
            error(format!(
                "Token endpoint response missing required fields: {}",
                redact_token_body(&text)
            ))
        })?;
    Ok(AccessToken {
        token: access_token.to_owned(),
        expires_at: now_secs() + expires_in as i64,
    })
}

/// `anthropic-sdk-typescript/<version> oidcFederationProvider` stand-in.
fn rpi_user_agent() -> String {
    format!(
        "rpi-ai/{} oidcFederationProvider",
        env!("CARGO_PKG_VERSION")
    )
}

type ProviderFuture = BoxFutureSend<'static, Result<AccessToken, ModelsError>>;
type ProviderFn = Arc<dyn Fn(bool) -> ProviderFuture + Send + Sync>;

struct PendingRefresh {
    id: u64,
    shared: Shared<ProviderFuture>,
}

struct CacheState {
    cached: Option<AccessToken>,
    pending: Option<PendingRefresh>,
    next_force: bool,
    last_advisory_error_secs: i64,
    next_id: u64,
}

/// `TokenCache` — two-tier proactive refresh with concurrent deduplication.
struct FederationTokenCache {
    provider: ProviderFn,
    state: AsyncMutex<CacheState>,
}

impl FederationTokenCache {
    fn new(provider: ProviderFn) -> Self {
        Self {
            provider,
            state: AsyncMutex::new(CacheState {
                cached: None,
                pending: None,
                next_force: false,
                last_advisory_error_secs: 0,
                next_id: 0,
            }),
        }
    }

    /// `getToken()`.
    async fn get_token(self: &Arc<Self>) -> Result<String, ModelsError> {
        let force = {
            let mut state = self.state.lock().await;
            let force = state.next_force;
            state.next_force = false;
            force
        };
        if force {
            return Ok(self.refresh(true).await?.token);
        }
        let cached = { self.state.lock().await.cached.clone() };
        let Some(cached) = cached else {
            return Ok(self.refresh(false).await?.token);
        };
        let remaining = cached.expires_at - now_secs();
        if remaining > ADVISORY_REFRESH_THRESHOLD_SECS {
            return Ok(cached.token);
        }
        if remaining > MANDATORY_REFRESH_THRESHOLD_SECS {
            // Advisory window: serve the stale token immediately and refresh
            // in the background; a failure keeps the stale token.
            self.background_refresh().await;
            return Ok(cached.token);
        }
        Ok(self.refresh(false).await?.token)
    }

    /// `invalidate()` — clear the cache and force the next refresh (called
    /// after a 401 from the API).
    async fn invalidate(&self) {
        let mut state = self.state.lock().await;
        state.cached = None;
        state.next_force = true;
    }

    /// `backgroundRefresh()` — shares the in-flight refresh, swallows errors
    /// and backs off for [`ADVISORY_REFRESH_BACKOFF_SECS`] after a failure.
    async fn background_refresh(self: &Arc<Self>) {
        let skip = {
            let state = self.state.lock().await;
            state.pending.is_some()
                || now_secs() - state.last_advisory_error_secs < ADVISORY_REFRESH_BACKOFF_SECS
        };
        if skip {
            return;
        }
        let this = self.clone();
        tokio::spawn(async move {
            if let Err(refresh_error) = this.refresh(false).await {
                let mut state = this.state.lock().await;
                state.last_advisory_error_secs = now_secs();
                drop(state);
                tracing::warn!(
                    error = %refresh_error.message,
                    "Anthropic federation advisory refresh failed; serving the cached token"
                );
            }
        });
    }

    /// `refresh(force)` — coalesces concurrent refreshes into one provider
    /// call unless forced.
    async fn refresh(&self, force: bool) -> Result<AccessToken, ModelsError> {
        enum Action {
            Await(Shared<ProviderFuture>),
            Start(u64, Shared<ProviderFuture>),
        }
        let action = {
            let mut state = self.state.lock().await;
            if !force && let Some(pending) = &state.pending {
                Action::Await(pending.shared.clone())
            } else {
                let id = {
                    state.next_id += 1;
                    state.next_id
                };
                let provider = self.provider.clone();
                let future = async move { provider(force).await }.boxed();
                let shared = future.shared();
                state.pending = Some(PendingRefresh {
                    id,
                    shared: shared.clone(),
                });
                Action::Start(id, shared)
            }
        };
        let (id, shared) = match action {
            Action::Await(pending) => return pending.await,
            Action::Start(id, shared) => (id, shared),
        };
        let result = shared.await;
        let mut state = self.state.lock().await;
        if state
            .pending
            .as_ref()
            .is_some_and(|pending| pending.id == id)
        {
            state.pending = None;
        }
        if let Ok(token) = &result {
            state.cached = Some(token.clone());
        }
        result
    }
}

/// Process-wide cache registry, one entry per resolved federation config
/// (`base_url` + ids), like the SDK's per-config client cache.
static CACHES: LazyLock<Mutex<HashMap<String, Arc<FederationTokenCache>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn cache_for(config: &AnthropicFederationConfig) -> Arc<FederationTokenCache> {
    let key = config.cache_key();
    let mut caches = CACHES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    caches
        .entry(key)
        .or_insert_with(|| {
            let config = config.clone();
            Arc::new(FederationTokenCache::new(Arc::new(move |_force| {
                let config = config.clone();
                Box::pin(async move { exchange_identity_token(&config).await })
            })))
        })
        .clone()
}

/// `getToken` for a resolved federation config — blocks on the first exchange
/// and on mandatory refreshes; serves the cached token in the advisory window.
pub async fn get_access_token(config: &AnthropicFederationConfig) -> Result<String, ModelsError> {
    cache_for(config).get_token().await
}

/// `TokenCache.invalidate()` — called after a 401 from the API so the next
/// attempt re-exchanges the assertion.
pub async fn invalidate_access_token(config: &AnthropicFederationConfig) {
    cache_for(config).invalidate().await;
}

#[cfg(test)]
mod tests {
    //! Test intents ported from the Anthropic SDK credentials tests and the
    //! pi federation suite (`packages/ai/test/anthropic-federation.test.ts` /
    //! `anthropic-federation-sdk.test.ts` @ pi abe508e1b), same names in
    //! snake_case where the surface overlaps. The SDK's mocked `fetch`
    //! becomes a scripted loopback token endpoint.

    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::sync::oneshot;

    use super::*;

    /// One recorded exchange: request body + lowercased request headers.
    type RecordedExchange = (Value, HashMap<String, String>);

    struct MockTokenEndpoint {
        url: String,
        requests: Arc<Mutex<Vec<RecordedExchange>>>,
        shutdown: Option<oneshot::Sender<()>>,
    }

    impl MockTokenEndpoint {
        async fn start(responses: Vec<(u16, Value)>) -> Self {
            let requests: Arc<Mutex<Vec<RecordedExchange>>> = Arc::new(Mutex::new(Vec::new()));
            let handler_requests = requests.clone();
            let responses = Arc::new(Mutex::new(responses.into_iter().collect::<Vec<_>>()));
            let handler_responses = responses.clone();
            let app = axum::Router::new().route(
                "/v1/oauth/token",
                axum::routing::post(move |headers: axum::http::HeaderMap, body: String| {
                    let requests = handler_requests.clone();
                    let responses = handler_responses.clone();
                    async move {
                        let parsed: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
                        let mut header_map = HashMap::new();
                        for (name, value) in headers.iter() {
                            header_map.insert(
                                name.as_str().to_ascii_lowercase(),
                                value.to_str().unwrap_or_default().to_owned(),
                            );
                        }
                        requests.lock().expect("lock").push((parsed, header_map));
                        let (status, body) = {
                            let mut responses = responses.lock().expect("lock");
                            if responses.len() > 1 {
                                responses.remove(0)
                            } else {
                                responses
                                    .first()
                                    .cloned()
                                    .unwrap_or((500, json!({"error": "no script"})))
                            }
                        };
                        (
                            axum::http::StatusCode::from_u16(status)
                                .unwrap_or(axum::http::StatusCode::INTERNAL_SERVER_ERROR),
                            axum::Json(body),
                        )
                    }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind");
            let address = listener.local_addr().expect("addr");
            let (shutdown, rx) = oneshot::channel();
            tokio::spawn(async move {
                let _ = axum::serve(listener, app)
                    .with_graceful_shutdown(async {
                        let _ = rx.await;
                    })
                    .await;
            });
            Self {
                url: format!("http://{address}"),
                requests,
                shutdown: Some(shutdown),
            }
        }

        fn config(&self, identity_token_file: &str) -> AnthropicFederationConfig {
            AnthropicFederationConfig {
                base_url: self.url.clone(),
                federation_rule_id: "fdrl_test".to_owned(),
                organization_id: "org-test".to_owned(),
                identity_token_file: identity_token_file.to_owned(),
                service_account_id: None,
                workspace_id: None,
            }
        }

        fn requests(&self) -> Vec<RecordedExchange> {
            self.requests.lock().expect("lock").clone()
        }

        fn exchange_count(&self) -> usize {
            self.requests.lock().expect("lock").len()
        }
    }

    impl Drop for MockTokenEndpoint {
        fn drop(&mut self) {
            if let Some(shutdown) = self.shutdown.take() {
                let _ = shutdown.send(());
            }
        }
    }

    /// Temp identity-token file removed on drop.
    struct TempIdentityFile {
        path: std::path::PathBuf,
    }

    impl TempIdentityFile {
        fn new(contents: &str) -> Self {
            static COUNTER: AtomicUsize = AtomicUsize::new(0);
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0);
            let path = std::env::temp_dir().join(format!(
                "rpi-federation-test-{}-{nanos}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::SeqCst)
            ));
            std::fs::write(&path, contents).expect("write identity token");
            Self { path }
        }
    }

    impl Drop for TempIdentityFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    fn identity_token_file(contents: &str) -> (TempIdentityFile, String) {
        let file = TempIdentityFile::new(contents);
        let path = file.path.display().to_string();
        (file, path)
    }

    fn token_response(access_token: &str, expires_in: i64) -> Value {
        json!({"access_token": access_token, "expires_in": expires_in})
    }

    /// SDK test "exchanges the identity token once across requests": repeated
    /// calls inside the cache window reuse the token.
    #[tokio::test]
    async fn exchanges_the_identity_token_once_across_requests() {
        let mock =
            MockTokenEndpoint::start(vec![(200, token_response("federated-token", 3600))]).await;
        let (_dir, identity_file) = identity_token_file("header.payload.signature");
        let config = mock.config(&identity_file);
        for _ in 0..3 {
            assert_eq!(
                get_access_token(&config).await.expect("token"),
                "federated-token"
            );
        }
        assert_eq!(mock.exchange_count(), 1);
    }

    /// The exchange sends the documented RFC 7523 request.
    #[tokio::test]
    async fn exchange_request_shape_matches_the_sdk() {
        let mock =
            MockTokenEndpoint::start(vec![(200, token_response("federated-token", 3600))]).await;
        let (_dir, identity_file) = identity_token_file("the-assertion");
        let mut config = mock.config(&identity_file);
        config.service_account_id = Some("svac_test".to_owned());
        config.workspace_id = Some("wrkspc_test".to_owned());
        get_access_token(&config).await.expect("token");

        let requests = mock.requests();
        assert_eq!(requests.len(), 1);
        let (body, headers) = &requests[0];
        assert_eq!(body["grant_type"], json!(GRANT_TYPE_JWT_BEARER));
        assert_eq!(body["assertion"], json!("the-assertion"));
        assert_eq!(body["federation_rule_id"], json!("fdrl_test"));
        assert_eq!(body["organization_id"], json!("org-test"));
        assert_eq!(body["service_account_id"], json!("svac_test"));
        assert_eq!(body["workspace_id"], json!("wrkspc_test"));
        assert_eq!(
            headers.get("anthropic-beta").map(String::as_str),
            Some("oauth-2025-04-20,oidc-federation-2026-04-01")
        );
    }

    /// The advisory window serves the stale token immediately and refreshes
    /// in the background.
    #[tokio::test]
    async fn advisory_window_serves_stale_and_refreshes_in_background() {
        let mock = MockTokenEndpoint::start(vec![
            (200, token_response("token-1", 60)),
            (200, token_response("token-2", 60)),
        ])
        .await;
        let (_dir, identity_file) = identity_token_file("jwt");
        let config = mock.config(&identity_file);
        assert_eq!(get_access_token(&config).await.expect("first"), "token-1");
        // 60s remaining → advisory window: stale token, background refresh.
        assert_eq!(
            get_access_token(&config).await.expect("advisory"),
            "token-1"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(mock.exchange_count(), 2);
        // The background refresh replaced the cached token.
        assert_eq!(
            get_access_token(&config).await.expect("refreshed"),
            "token-2"
        );
    }

    /// Below the mandatory threshold the refresh blocks.
    #[tokio::test]
    async fn mandatory_window_blocks_for_a_fresh_token() {
        let mock = MockTokenEndpoint::start(vec![
            (200, token_response("token-1", 10)),
            (200, token_response("token-2", 3600)),
        ])
        .await;
        let (_dir, identity_file) = identity_token_file("jwt");
        let config = mock.config(&identity_file);
        assert_eq!(get_access_token(&config).await.expect("first"), "token-1");
        // 10s remaining → mandatory window: blocks and refreshes.
        assert_eq!(get_access_token(&config).await.expect("second"), "token-2");
        assert_eq!(mock.exchange_count(), 2);
    }

    /// `invalidate()` forces the next call to re-exchange.
    #[tokio::test]
    async fn invalidate_forces_the_next_refresh() {
        let mock = MockTokenEndpoint::start(vec![
            (200, token_response("token-1", 3600)),
            (200, token_response("token-2", 3600)),
        ])
        .await;
        let (_dir, identity_file) = identity_token_file("jwt");
        let config = mock.config(&identity_file);
        assert_eq!(get_access_token(&config).await.expect("first"), "token-1");
        invalidate_access_token(&config).await;
        assert_eq!(get_access_token(&config).await.expect("second"), "token-2");
        assert_eq!(mock.exchange_count(), 2);
    }

    /// An advisory refresh failure keeps the stale token and backs off.
    #[tokio::test]
    async fn advisory_failure_keeps_the_stale_token_and_backs_off() {
        let mock = MockTokenEndpoint::start(vec![
            (200, token_response("token-1", 60)),
            (500, json!({"error": "boom"})),
        ])
        .await;
        let (_dir, identity_file) = identity_token_file("jwt");
        let config = mock.config(&identity_file);
        assert_eq!(get_access_token(&config).await.expect("first"), "token-1");
        assert_eq!(
            get_access_token(&config).await.expect("advisory"),
            "token-1"
        );
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(mock.exchange_count(), 2);
        // Backoff window: no new exchange, stale token still served.
        assert_eq!(get_access_token(&config).await.expect("stale"), "token-1");
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(mock.exchange_count(), 2);
    }

    /// Concurrent mandatory callers coalesce into a single exchange.
    #[tokio::test]
    async fn concurrent_mandatory_refreshes_coalesce() {
        let mock = MockTokenEndpoint::start(vec![(200, token_response("token-1", 3600))]).await;
        let (_dir, identity_file) = identity_token_file("jwt");
        let config = Arc::new(mock.config(&identity_file));
        let mut tasks = Vec::new();
        for _ in 0..8 {
            let config = config.clone();
            tasks.push(tokio::spawn(async move { get_access_token(&config).await }));
        }
        for task in tasks {
            assert_eq!(task.await.expect("join").expect("token"), "token-1");
        }
        assert_eq!(mock.exchange_count(), 1);
    }

    /// Assertions over 16 KiB are rejected client-side.
    #[tokio::test]
    async fn rejects_oversized_assertions() {
        let mock = MockTokenEndpoint::start(vec![]).await;
        let oversized = "a".repeat(MAX_IDENTITY_TOKEN_BYTES + 1);
        let (_dir, identity_file) = identity_token_file(&oversized);
        let config = mock.config(&identity_file);
        let failure = get_access_token(&config).await.expect_err("oversized");
        assert!(
            failure
                .message
                .contains("exceeds the 16 KiB assertion limit")
        );
        assert_eq!(mock.exchange_count(), 0);
    }

    /// Missing/empty identity token files are diagnostic, not silent.
    #[tokio::test]
    async fn rejects_unreadable_or_empty_identity_token_files() {
        let mock = MockTokenEndpoint::start(vec![]).await;
        let missing = mock.config("/nonexistent/identity.jwt");
        let failure = get_access_token(&missing).await.expect_err("missing file");
        assert!(
            failure
                .message
                .contains("Failed to read identity token file")
        );

        let (_dir, empty_file) = identity_token_file("   \n");
        let empty = mock.config(&empty_file);
        let failure = get_access_token(&empty).await.expect_err("empty file");
        assert!(failure.message.contains("is empty"));
        assert_eq!(mock.exchange_count(), 0);
    }

    /// Non-loopback cleartext endpoints are refused before the assertion is
    /// read.
    #[tokio::test]
    async fn refuses_cleartext_non_loopback_endpoints() {
        let (_dir, identity_file) = identity_token_file("jwt");
        let config = AnthropicFederationConfig {
            base_url: "http://federation.example.com".to_owned(),
            federation_rule_id: "fdrl".to_owned(),
            organization_id: "org".to_owned(),
            identity_token_file: identity_file,
            service_account_id: None,
            workspace_id: None,
        };
        let failure = get_access_token(&config).await.expect_err("cleartext");
        assert!(failure.message.contains("Refusing to send credential"));
    }

    /// A 401 exchange failure carries the workspace/federation-rule hint.
    #[tokio::test]
    async fn exchange_401_carries_the_diagnostic_hint() {
        let mock = MockTokenEndpoint::start(vec![(
            401,
            json!({"error": "unauthorized", "secret": "leaked"}),
        )])
        .await;
        let (_dir, identity_file) = identity_token_file("jwt");
        let config = mock.config(&identity_file);
        let failure = get_access_token(&config).await.expect_err("401");
        assert!(
            failure
                .message
                .contains("Token exchange failed with status 401")
        );
        assert!(
            failure
                .message
                .contains("Ensure your federation rule matches")
        );
        assert!(failure.message.contains("ANTHROPIC_WORKSPACE_ID"));
        assert!(!failure.message.contains("leaked"), "secret redacted");
        assert_eq!(mock.exchange_count(), 1);
    }

    /// Missing `access_token` / `expires_in` are response-contract failures.
    #[tokio::test]
    async fn rejects_invalid_token_responses() {
        let mock = MockTokenEndpoint::start(vec![
            (200, json!({"expires_in": 3600})),
            (200, json!({"access_token": "token"})),
        ])
        .await;
        let (_dir, identity_file) = identity_token_file("jwt");
        let config = mock.config(&identity_file);
        let failure = get_access_token(&config)
            .await
            .expect_err("no access token");
        assert!(failure.message.contains("missing access_token"));
        invalidate_access_token(&config).await;
        let failure = get_access_token(&config).await.expect_err("no expires_in");
        assert!(failure.message.contains("missing required fields"));
    }

    /// The resolution helper follows the upstream precedence and guards.
    #[test]
    fn federation_resolution_guards() {
        let env: ProviderEnv = [
            (
                ANTHROPIC_FEDERATION_RULE_ID_ENV.to_owned(),
                "fdrl".to_owned(),
            ),
            (ANTHROPIC_ORGANIZATION_ID_ENV.to_owned(), "org".to_owned()),
            (
                ANTHROPIC_IDENTITY_TOKEN_FILE_ENV.to_owned(),
                "/tmp/identity.jwt".to_owned(),
            ),
        ]
        .into_iter()
        .collect();
        let mut headers = ProviderHeaders::new();
        headers.insert(
            "Authorization".to_owned(),
            Some("Bearer auth-token".to_owned()),
        );
        // Provider guard and request-auth guard.
        assert!(
            get_anthropic_federation(
                "kimi-coding",
                "https://api.kimi.com",
                None,
                None,
                Some(&env)
            )
            .is_none()
        );
        assert!(
            get_anthropic_federation(
                "anthropic",
                "https://api.anthropic.com",
                None,
                Some(&headers),
                Some(&env)
            )
            .is_none()
        );
        assert!(
            get_anthropic_federation(
                "anthropic",
                "https://api.anthropic.com",
                Some("sk-key"),
                None,
                Some(&env)
            )
            .is_none()
        );
        // All three required variables must be present.
        let mut partial = env.clone();
        partial.remove(ANTHROPIC_IDENTITY_TOKEN_FILE_ENV);
        assert!(
            get_anthropic_federation(
                "anthropic",
                "https://api.anthropic.com",
                None,
                None,
                Some(&partial)
            )
            .is_none()
        );
        // Optional service account / workspace ride along when set.
        let mut full = env.clone();
        full.insert(
            ANTHROPIC_SERVICE_ACCOUNT_ID_ENV.to_owned(),
            "svac".to_owned(),
        );
        full.insert(ANTHROPIC_WORKSPACE_ID_ENV.to_owned(), "wrk".to_owned());
        let config = get_anthropic_federation(
            "anthropic",
            "https://api.anthropic.com",
            None,
            None,
            Some(&full),
        )
        .expect("federation config");
        assert_eq!(config.service_account_id.as_deref(), Some("svac"));
        assert_eq!(config.workspace_id.as_deref(), Some("wrk"));
    }

    /// The `invalidate` + forced-refresh path clears the cache once.
    #[tokio::test]
    async fn forced_refresh_bypasses_an_in_flight_non_forced_refresh() {
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        let provider: ProviderFn = Arc::new(|force| {
            Box::pin(async move {
                CALLS.fetch_add(1, Ordering::SeqCst);
                Ok(AccessToken {
                    token: if force { "forced" } else { "soft" }.to_owned(),
                    expires_at: now_secs() + 3600,
                })
            })
        });
        let cache = Arc::new(FederationTokenCache::new(provider));
        let first = cache.refresh(false).await.expect("first");
        assert_eq!(first.token, "soft");
        cache.invalidate().await;
        let second = cache.get_token().await.expect("second");
        assert_eq!(second, "forced");
        assert_eq!(CALLS.load(Ordering::SeqCst), 2);
    }
}
