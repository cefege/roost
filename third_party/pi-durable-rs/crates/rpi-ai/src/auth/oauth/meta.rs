//! Port of `packages/ai/src/auth/oauth/meta.ts` @ pi 0.86.1 (`b73412a37`,
//! #9096) — Meta Model API OAuth: RFC 8628 device authorization grant
//! against `https://auth.meta.com` (JSON responses). Meta splits identity
//! from API access: the resulting identity token is not accepted for
//! inference, so it is exchanged for a Model API key via the Muse Code
//! key-mint endpoint (`https://api.meta.ai/muse-code/key`, minted keys live
//! about a day). The identity token is stored as `refresh` and the minted
//! key as `access`, so the standard refresh-on-expiry machinery
//! (`Models::resolve_refresh_credential`) re-mints the key daily with no
//! bespoke renewal logic. The identity token itself is not renewable
//! (auth.meta.com answers `grant_type=refresh_token` with 404 and issues no
//! `refresh_token`), so a 401/403 from mint means the session is dead and
//! the user must sign in again (error text directs to `/login meta`).
//!
//! Upstream `lazyOAuth({ loginLabel: "Sign in with Meta" })` has no
//! `OAuthAuth` slot in rpi (same as the other flows, see
//! `providers/meta.rs`).
//!
//! Test seams (upstream stubs the global `fetch` and matches URLs; here the
//! two hosts become constructor fields — same precedent as
//! `kimi_coding.rs`'s `oauth_host`):
//! - `auth_host`: overrides the auth host (device authorization/token
//!   URLs derive from it);
//! - `mint_url`: overrides the Muse Code key-mint endpoint.
//!
//! Intentional differences:
//! - `AbortSignal.any([AbortSignal.timeout(30_000), signal])` becomes a
//!   client-level reqwest timeout racing the `CancellationToken`; the login
//!   catch maps a cancelled flow to [`super::device_code::CANCEL_MESSAGE`]
//!   (upstream throws `new Error("Login cancelled")` when
//!   `interaction.signal.aborted`);
//! - `Date.now()` becomes `SystemTime` milliseconds; `interval` /
//!   `expires_in` parse as `f64` (JS `number`) and narrow into the `u64`
//!   event fields (no expiry skew — the credential expiry is computed once
//!   at mint time from `API_KEY_LIFETIME_MS`).

use std::sync::Arc;

use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

use super::super::interaction::{AuthEvent, AuthInteraction};
use super::super::resolve::{ModelsError, ModelsErrorCode};
use super::super::types::{LoginOptions, ModelAuth, OAuthAuth, OAuthCredential};
use super::device_code::{
    CANCEL_MESSAGE, DeviceCodePollOptions, DeviceCodePollResult, poll_oauth_device_code_flow,
};

/// Muse Code CLI client id.
const CLIENT_ID: &str = "1031625952748946";
/// `AUTH_HOST`.
const AUTH_HOST: &str = "https://auth.meta.com";
/// `DEVICE_AUTHORIZATION_URL` = `${AUTH_HOST}/oidc/device/authorization/`.
const DEVICE_AUTHORIZATION_PATH: &str = "/oidc/device/authorization/";
/// `DEVICE_TOKEN_URL` = `${AUTH_HOST}/oidc/device/token/`.
const DEVICE_TOKEN_PATH: &str = "/oidc/device/token/";
/// `API_KEY_MINT_URL`.
const API_KEY_MINT_URL: &str = "https://api.meta.ai/muse-code/key";
/// `API_KEY_LIFETIME_MS` = 24h ("minted keys live about a day" → the daily
/// re-mint of the standard scheduler).
const API_KEY_LIFETIME_MS: i64 = 24 * 60 * 60 * 1000;
/// `REQUEST_TIMEOUT_MS` = `AbortSignal.timeout(30_000)`.
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// `grant_type` of the device-code poll (`urn:ietf:params:oauth:grant-type:device_code`).
const DEVICE_CODE_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:device_code";
/// Body of the mint request (`body: "{}"`).
const MINT_BODY: &str = "{}";

fn error(message: impl Into<String>) -> ModelsError {
    ModelsError::new(ModelsErrorCode::Oauth, message.into())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

/// `trustedHttpUrl` — the verification URI is opened in the user's browser;
/// only http(s) URLs are trusted. Returns the parsed-and-serialized form
/// (JS `url.href`).
fn trusted_http_url(value: Option<&Value>) -> Option<String> {
    let raw = value?.as_str().filter(|value| !value.is_empty())?;
    let url = url::Url::parse(raw).ok()?;
    match url.scheme() {
        "https" | "http" => Some(url.to_string()),
        _ => None,
    }
}

/// `readJson` — parse JSON, or `null` for unparseable bodies. JS
/// `typeof json === "object"` admits arrays too; both keep their value here
/// (the callers' field lookups then report them as invalid responses).
async fn read_json(response: reqwest::Response) -> Value {
    match response.json::<Value>().await {
        Ok(parsed) if parsed.is_object() || parsed.is_array() => parsed,
        _ => Value::Null,
    }
}

/// `errorDetail` — first string field among `error_description` / `detail`
/// / `message` / `error` with a non-blank value, formatted as `": value"`.
fn error_detail(json: &Value) -> String {
    for key in ["error_description", "detail", "message", "error"] {
        if let Some(value) = json.get(key).and_then(Value::as_str) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return format!(": {trimmed}");
            }
        }
    }
    String::new()
}

/// `positiveNumber` — a positive finite number wins, else `undefined`.
fn positive_number(value: Option<&Value>) -> Option<f64> {
    let number = value.and_then(Value::as_f64)?;
    if number.is_finite() && number > 0.0 {
        Some(number)
    } else {
        None
    }
}

/// `DeviceAuthorization` — `verificationUri` is the trusted
/// `verification_uri_complete` falling back to the plain `verification_uri`.
struct DeviceAuthorization {
    device_code: String,
    user_code: String,
    verification_uri: String,
    interval_seconds: Option<f64>,
    expires_in_seconds: Option<f64>,
}

/// `metaOAuth` — the Meta (Muse subscription) provider auth.
pub fn meta_oauth() -> Arc<dyn OAuthAuth> {
    Arc::new(MetaOAuth::new())
}

/// Meta Model API OAuth (`OAuthAuth`) implementation.
pub struct MetaOAuth {
    client: reqwest::Client,
    /// Auth-host test seam — see module docs.
    auth_host: Option<String>,
    /// Mint-endpoint test seam — see module docs.
    mint_url: Option<String>,
}

impl Default for MetaOAuth {
    fn default() -> Self {
        Self::new()
    }
}

impl MetaOAuth {
    pub fn new() -> Self {
        let client = match reqwest::Client::builder().timeout(REQUEST_TIMEOUT).build() {
            Ok(client) => client,
            // Unreachable with this configuration; fall back rather than
            // panic (no unwrap in non-test code).
            Err(_) => reqwest::Client::new(),
        };
        Self {
            client,
            auth_host: None,
            mint_url: None,
        }
    }

    /// Auth-host test seam — see module docs.
    pub fn with_auth_host(mut self, auth_host: impl Into<String>) -> Self {
        self.auth_host = Some(auth_host.into());
        self
    }

    /// Mint-endpoint test seam — see module docs.
    pub fn with_mint_url(mut self, mint_url: impl Into<String>) -> Self {
        self.mint_url = Some(mint_url.into());
        self
    }

    fn auth_host(&self) -> &str {
        self.auth_host.as_deref().unwrap_or(AUTH_HOST)
    }

    fn device_authorization_url(&self) -> String {
        format!("{}{DEVICE_AUTHORIZATION_PATH}", self.auth_host())
    }

    fn device_token_url(&self) -> String {
        format!("{}{DEVICE_TOKEN_PATH}", self.auth_host())
    }

    fn mint_url(&self) -> String {
        self.mint_url
            .clone()
            .unwrap_or_else(|| API_KEY_MINT_URL.to_owned())
    }

    /// `requestSignal(signal)` — send a form-encoded POST, racing the
    /// request against the flow cancellation signal.
    async fn send_form(
        &self,
        url: &str,
        form: &[(&str, &str)],
        signal: Option<&CancellationToken>,
    ) -> Result<reqwest::Response, ModelsError> {
        let send = self
            .client
            .post(url)
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .header(reqwest::header::ACCEPT, "application/json")
            .form(form)
            .send();
        let response = match signal {
            Some(token) => tokio::select! {
                () = token.cancelled() => return Err(error(CANCEL_MESSAGE)),
                response = send => response,
            },
            None => send.await,
        };
        response.map_err(|request_error| error(request_error.to_string()))
    }

    /// `startDeviceAuthorization` — POST the device authorization endpoint
    /// (`client_id`), validate the response shape; the verification URI is
    /// the trusted `verification_uri_complete` with the plain
    /// `verification_uri` as fallback (upstream
    /// `trustedHttpUrl(...complete) ?? trustedHttpUrl(...)`, so an untrusted
    /// or missing complete form falls back rather than failing).
    async fn start_device_authorization(
        &self,
        signal: Option<&CancellationToken>,
    ) -> Result<DeviceAuthorization, ModelsError> {
        let response = self
            .send_form(
                &self.device_authorization_url(),
                &[("client_id", CLIENT_ID)],
                signal,
            )
            .await?;
        let status = response.status().as_u16();
        let ok = response.status().is_success();
        let json = read_json(response).await;
        if !ok {
            return Err(error(format!(
                "Meta device authorization failed with status {status}{}",
                error_detail(&json)
            )));
        }

        let verification_uri = trusted_http_url(json.get("verification_uri_complete"))
            .or_else(|| trusted_http_url(json.get("verification_uri")));
        match (
            json.get("device_code").and_then(Value::as_str),
            json.get("user_code").and_then(Value::as_str),
            verification_uri,
        ) {
            (Some(device_code), Some(user_code), Some(verification_uri))
                if !device_code.is_empty() && !user_code.is_empty() =>
            {
                Ok(DeviceAuthorization {
                    device_code: device_code.to_owned(),
                    user_code: user_code.to_owned(),
                    verification_uri,
                    interval_seconds: positive_number(json.get("interval")),
                    expires_in_seconds: positive_number(json.get("expires_in")),
                })
            }
            _ => Err(error(format!(
                "Invalid Meta device authorization response: {json}"
            ))),
        }
    }

    /// One token poll (`poll` closure of `pollForIdentityToken`): POST the
    /// device token endpoint with the device_code grant; a 2xx response
    /// with a non-empty `access_token` string completes (identity token),
    /// otherwise the RFC 8628 `error` field drives the poll result.
    async fn poll_identity_token(
        &self,
        device: &DeviceAuthorization,
        signal: Option<CancellationToken>,
    ) -> DeviceCodePollResult<String> {
        let response = match self
            .send_form(
                &self.device_token_url(),
                &[
                    ("grant_type", DEVICE_CODE_GRANT_TYPE),
                    ("device_code", device.device_code.as_str()),
                    ("client_id", CLIENT_ID),
                ],
                signal.as_ref(),
            )
            .await
        {
            // Upstream lets the fetch error propagate out of the poll
            // closure; the framework surface here is `Failed`.
            Err(fetch_error) => {
                return DeviceCodePollResult::Failed {
                    message: fetch_error.message,
                };
            }
            Ok(response) => response,
        };
        let status = response.status().as_u16();
        let ok = response.status().is_success();
        let json = read_json(response).await;

        let access_token = json
            .get("access_token")
            .and_then(Value::as_str)
            .filter(|token| !token.is_empty());
        if ok && let Some(token) = access_token {
            return DeviceCodePollResult::Complete {
                value: token.to_owned(),
            };
        }

        match json.get("error").and_then(Value::as_str) {
            Some("authorization_pending") => DeviceCodePollResult::Pending,
            Some("slow_down") => DeviceCodePollResult::SlowDown {
                interval_seconds: positive_number(json.get("interval")),
            },
            Some("access_denied") => DeviceCodePollResult::Failed {
                message: "Meta login was denied.".to_owned(),
            },
            Some("expired_token") => DeviceCodePollResult::Failed {
                message: "Meta device authorization expired. Please restart login.".to_owned(),
            },
            _ => DeviceCodePollResult::Failed {
                message: format!(
                    "Meta device token request failed with status {status}{}",
                    error_detail(&json)
                ),
            },
        }
    }

    /// `pollForIdentityToken` — `waitBeforeFirstPoll: true`.
    async fn poll_for_identity_token(
        &self,
        device: &DeviceAuthorization,
        signal: Option<CancellationToken>,
    ) -> Result<String, ModelsError> {
        let poll_signal = signal.clone();
        poll_oauth_device_code_flow(DeviceCodePollOptions {
            interval_seconds: device.interval_seconds,
            expires_in_seconds: device.expires_in_seconds,
            wait_before_first_poll: true,
            signal,
            poll: move || self.poll_identity_token(device, poll_signal.clone()),
        })
        .await
    }

    /// `mintApiKey` — exchange an identity token for a Model API key
    /// (`POST {mint_url}`, `Authorization: Bearer`, `x-api-version: 1.0.0`,
    /// body `"{}"`). Keys are valid for about a day. 401/403 means the
    /// identity session is dead (see module docs) — no silent retry.
    async fn mint_api_key(
        &self,
        identity_token: &str,
        signal: Option<&CancellationToken>,
    ) -> Result<OAuthCredential, ModelsError> {
        let send = self
            .client
            .post(self.mint_url())
            .header(reqwest::header::ACCEPT, "application/json")
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {identity_token}"),
            )
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header("x-api-version", "1.0.0")
            .body(MINT_BODY)
            .send();
        let response = match signal {
            Some(token) => tokio::select! {
                () = token.cancelled() => return Err(error(CANCEL_MESSAGE)),
                response = send => response,
            },
            None => send.await,
        };
        let response = response.map_err(|request_error| error(request_error.to_string()))?;
        let status = response.status().as_u16();
        let ok = response.status().is_success();
        let json = read_json(response).await;

        if status == 401 || status == 403 {
            // Identity token is not renewable (see module docs); only a
            // fresh device flow helps.
            return Err(error(format!(
                "Meta session expired (status {status}). Run `/login meta` to sign in again.{}",
                error_detail(&json)
            )));
        }
        if !ok {
            return Err(error(format!(
                "Meta API key mint failed with status {status}{}",
                error_detail(&json)
            )));
        }

        let api_key = json
            .get("api_key")
            .and_then(Value::as_str)
            .filter(|key| !key.is_empty());
        let Some(api_key) = api_key else {
            let action_url = trusted_http_url(json.get("action_url"));
            return Err(error(format!(
                "Meta did not issue an API key.{}",
                action_url
                    .map(|url| format!(" Complete setup at {url}"))
                    .unwrap_or_default()
            )));
        };
        Ok(OAuthCredential {
            refresh: identity_token.to_owned(),
            access: api_key.to_owned(),
            expires: now_ms() + API_KEY_LIFETIME_MS,
            extra: Map::new(),
        })
    }

    /// `loginMeta`.
    async fn login_meta(
        &self,
        interaction: &dyn AuthInteraction,
    ) -> Result<OAuthCredential, ModelsError> {
        match self.login_meta_inner(interaction).await {
            // Upstream catch: an aborted flow surfaces as "Login cancelled"
            // regardless of the in-flight error (the fetch rejects with a
            // DOMException on abort; the login UI matches this message).
            Err(_) if interaction.signal().is_some_and(|t| t.is_cancelled()) => {
                Err(error(CANCEL_MESSAGE))
            }
            result => result,
        }
    }

    async fn login_meta_inner(
        &self,
        interaction: &dyn AuthInteraction,
    ) -> Result<OAuthCredential, ModelsError> {
        let device = self
            .start_device_authorization(interaction.signal().as_ref())
            .await?;
        interaction.notify(AuthEvent::DeviceCode {
            user_code: device.user_code.clone(),
            verification_uri: device.verification_uri.clone(),
            interval_seconds: device.interval_seconds.map(|seconds| seconds as u64),
            expires_in_seconds: device.expires_in_seconds.map(|seconds| seconds as u64),
        });
        let identity_token = self
            .poll_for_identity_token(&device, interaction.signal())
            .await?;
        interaction.notify(AuthEvent::Progress {
            message: "Enabling Meta Model API access...".to_owned(),
        });
        self.mint_api_key(&identity_token, interaction.signal().as_ref())
            .await
    }
}

#[async_trait::async_trait]
impl OAuthAuth for MetaOAuth {
    fn name(&self) -> &str {
        "Meta (Muse subscription)"
    }

    /// `isSubscription: true` (providers/meta.ts @ b73412a37).
    fn is_subscription(&self) -> bool {
        true
    }

    /// `login` — device code flow (RFC 8628) + key mint, no prompt
    /// (upstream "Meta login should not prompt").
    async fn login(
        &self,
        interaction: &dyn AuthInteraction,
        _options: Option<&LoginOptions>,
    ) -> Result<OAuthCredential, ModelsError> {
        self.login_meta(interaction).await
    }

    /// `refresh: (credential, signal) => mintApiKey(credential.refresh,
    /// signal)` — the standard scheduler's daily re-mint.
    async fn refresh(
        &self,
        credential: &OAuthCredential,
        signal: Option<&CancellationToken>,
    ) -> Result<OAuthCredential, ModelsError> {
        self.mint_api_key(&credential.refresh, signal).await
    }

    /// `toAuth: { apiKey: credential.access }` — the minted key is the
    /// request api key.
    async fn to_auth(&self, credential: &OAuthCredential) -> Result<ModelAuth, ModelsError> {
        Ok(ModelAuth {
            api_key: Some(credential.access.clone()),
            headers: None,
            base_url: None,
        })
    }
}

#[cfg(test)]
mod tests {
    //! Test intents ported from `packages/ai/test/meta-oauth.test.ts`
    //! @ pi 0.86.1 (`b73412a37`, #9096); the mocked global `fetch` becomes a
    //! loopback axum server dispatching by path (device authorization /
    //! device token / key mint) behind the two constructor seams (module
    //! docs). Upstream drives vitest fake timers with a 5s poll interval;
    //! here the canned interval is 1s (same precedent as
    //! `kimi_coding.rs`'s tests — real sleeps, elapsed measured instead of
    //! pinned wall-clock).
    //!
    //! Plus one FR-B R3-derived case (`d875512cc`-window upstream file has
    //! no 401/403 mint test): the mint 401/403 → `/login meta` branch.

    use std::sync::Mutex;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::response::Json;
    use serde_json::json;
    use tokio::sync::oneshot;

    use super::super::super::interaction::AuthPrompt;
    use super::super::super::types::BoxFutureSend;
    use super::*;

    const DAY_MS: i64 = 24 * 60 * 60 * 1000;

    // ----- mock Meta endpoints (upstream: `vi.stubGlobal("fetch")`) -----

    #[derive(Debug, Clone)]
    struct RecordedRequest {
        method: String,
        path: String,
        body: String,
        authorization: Option<String>,
        x_api_version: Option<String>,
        content_type: Option<String>,
    }

    /// `new URLSearchParams(String(init?.body))` → `form.get(name)`.
    fn form_get(body: &str, name: &str) -> Option<String> {
        url::form_urlencoded::parse(body.as_bytes())
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.into_owned())
    }

    type Responder = Arc<dyn Fn(&RecordedRequest) -> (StatusCode, Value) + Send + Sync + 'static>;

    struct MockMeta {
        url: String,
        requests: Arc<Mutex<Vec<RecordedRequest>>>,
        shutdown: Option<oneshot::Sender<()>>,
    }

    impl MockMeta {
        async fn start(responder: Responder) -> Self {
            let requests: Arc<Mutex<Vec<RecordedRequest>>> = Arc::new(Mutex::new(Vec::new()));
            let handler_requests = requests.clone();
            let app = axum::Router::new().fallback(move |request: Request<Body>| {
                let requests = handler_requests.clone();
                let responder = responder.clone();
                async move {
                    let headers = request.headers().clone();
                    let recorded = RecordedRequest {
                        method: request.method().to_string(),
                        path: request.uri().path().to_owned(),
                        body: axum::body::to_bytes(request.into_body(), usize::MAX)
                            .await
                            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                            .unwrap_or_default(),
                        authorization: headers
                            .get(axum::http::header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_owned),
                        x_api_version: headers
                            .get("x-api-version")
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_owned),
                        content_type: headers
                            .get(axum::http::header::CONTENT_TYPE)
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_owned),
                    };
                    requests.lock().expect("lock").push(recorded.clone());
                    let (status, body) = responder(&recorded);
                    (status, Json(body))
                }
            });
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind");
            let addr = listener.local_addr().expect("addr");
            let (tx, rx) = oneshot::channel::<()>();
            tokio::spawn(async move {
                let _ = axum::serve(listener, app)
                    .with_graceful_shutdown(async move {
                        let _ = rx.await;
                    })
                    .await;
            });
            Self {
                url: format!("http://{addr}"),
                requests,
                shutdown: Some(tx),
            }
        }

        fn oauth(&self) -> MetaOAuth {
            MetaOAuth::new()
                .with_auth_host(self.url.clone())
                .with_mint_url(format!("{}/muse-code/key", self.url))
        }

        fn requests(&self) -> Vec<RecordedRequest> {
            self.requests.lock().expect("lock").clone()
        }
    }

    impl Drop for MockMeta {
        fn drop(&mut self) {
            if let Some(shutdown) = self.shutdown.take() {
                let _ = shutdown.send(());
            }
        }
    }

    // ----- fake AuthInteraction (upstream `createInteraction`) -----

    #[derive(Clone, Default)]
    struct InteractionHandle {
        events: Arc<Mutex<Vec<AuthEvent>>>,
    }

    impl InteractionHandle {
        fn events(&self) -> Vec<AuthEvent> {
            self.events.lock().expect("lock").clone()
        }
    }

    struct FakeInteraction {
        handle: InteractionHandle,
    }

    impl AuthInteraction for FakeInteraction {
        fn prompt<'a>(
            &'a self,
            prompt: AuthPrompt,
        ) -> BoxFutureSend<'a, Result<String, ModelsError>> {
            // Upstream: "Meta login should not prompt".
            Box::pin(async move {
                Err(ModelsError::new(
                    ModelsErrorCode::Auth,
                    format!("Meta login should not prompt: {prompt:?}"),
                ))
            })
        }

        fn notify(&self, event: AuthEvent) {
            self.handle.events.lock().expect("lock").push(event);
        }
    }

    // ----- canned upstream response bodies -----

    fn device_authorization_response() -> Value {
        json!({
            "device_code": "device-code-123",
            "user_code": "ABCD-1234",
            "verification_uri": "https://auth.meta.com/oauth/device/",
            "verification_uri_complete":
                "https://auth.meta.com/oauth/device/?code=ABCD-1234",
            // Upstream uses interval 5 with fake timers; real sleeps use 1s
            // (kimi_coding.rs precedent).
            "interval": 1,
            "expires_in": 600,
        })
    }

    #[tokio::test]
    async fn logs_in_with_the_device_flow_and_mints_a_model_api_key() {
        // Upstream test 1: device authorization (client_id form) →
        // device_code notify (complete verification URI preferred) →
        // identity token poll (pending → complete) → key mint (Bearer
        // identity token) → credential {refresh: identity, access: minted,
        // expires: now + DAY}.
        let token_polls = Arc::new(Mutex::new(
            std::collections::VecDeque::<(StatusCode, Value)>::new(),
        ));
        token_polls.lock().expect("lock").extend([
            (
                StatusCode::BAD_REQUEST,
                json!({ "error": "authorization_pending" }),
            ),
            (
                StatusCode::OK,
                json!({ "access_token": "identity-token", "token_type": "Bearer" }),
            ),
        ]);
        let token_polls_for_handler = token_polls.clone();

        let mock = MockMeta::start(Arc::new(move |request| match request.path.as_str() {
            "/oidc/device/authorization/" => (StatusCode::OK, device_authorization_response()),
            "/oidc/device/token/" => token_polls_for_handler
                .lock()
                .expect("lock")
                .pop_front()
                .unwrap_or((
                    StatusCode::BAD_REQUEST,
                    json!({ "message": "Unexpected extra token poll" }),
                )),
            "/muse-code/key" => (StatusCode::OK, json!({ "api_key": "LLM|minted-key" })),
            _ => (
                StatusCode::NOT_FOUND,
                json!({ "message": format!("unexpected path {}", request.path) }),
            ),
        }))
        .await;

        let handle = InteractionHandle::default();
        let started = now_ms();
        let credential = mock
            .oauth()
            .login(
                &FakeInteraction {
                    handle: handle.clone(),
                },
                None,
            )
            .await
            .expect("login");
        let finished = now_ms();

        // Notify order: device_code (with the complete verification URI,
        // interval, expiry), then the "Enabling…" progress event.
        let events = handle.events();
        assert_eq!(events.len(), 2, "events: {events:?}");
        assert_eq!(
            events[0],
            AuthEvent::DeviceCode {
                user_code: "ABCD-1234".to_owned(),
                verification_uri: "https://auth.meta.com/oauth/device/?code=ABCD-1234".to_owned(),
                interval_seconds: Some(1),
                expires_in_seconds: Some(600),
            }
        );
        assert_eq!(
            events[1],
            AuthEvent::Progress {
                message: "Enabling Meta Model API access...".to_owned()
            }
        );

        // Device authorization request shape.
        let requests = mock.requests();
        let authorization_request = requests
            .iter()
            .find(|request| request.path == "/oidc/device/authorization/")
            .expect("authorization request");
        assert_eq!(authorization_request.method, "POST");
        assert_eq!(
            form_get(&authorization_request.body, "client_id").as_deref(),
            Some(CLIENT_ID)
        );

        // Token poll request shape.
        let token_request = requests
            .iter()
            .find(|request| request.path == "/oidc/device/token/")
            .expect("token request");
        assert_eq!(
            form_get(&token_request.body, "grant_type").as_deref(),
            Some(DEVICE_CODE_GRANT_TYPE)
        );
        assert_eq!(
            form_get(&token_request.body, "client_id").as_deref(),
            Some(CLIENT_ID)
        );
        assert_eq!(
            form_get(&token_request.body, "device_code").as_deref(),
            Some("device-code-123")
        );

        // Mint request shape.
        let mint_request = requests
            .iter()
            .find(|request| request.path == "/muse-code/key")
            .expect("mint request");
        assert_eq!(mint_request.method, "POST");
        assert_eq!(
            mint_request.authorization.as_deref(),
            Some("Bearer identity-token")
        );
        assert_eq!(mint_request.x_api_version.as_deref(), Some("1.0.0"));
        assert_eq!(
            mint_request.content_type.as_deref(),
            Some("application/json")
        );
        assert_eq!(mint_request.body, "{}");

        // Credential: identity token as refresh, minted key as access,
        // about a day of validity.
        assert_eq!(credential.refresh, "identity-token");
        assert_eq!(credential.access, "LLM|minted-key");
        assert!(credential.expires >= started + DAY_MS);
        assert!(credential.expires <= finished + DAY_MS);
    }

    #[tokio::test]
    async fn re_mints_the_api_key_from_the_stored_identity_token_on_refresh() {
        // Upstream test 2: refresh re-mints through the same endpoint with
        // the stored identity token (the standard scheduler's daily
        // re-mint), keeping refresh unchanged.
        let mock = MockMeta::start(Arc::new(|request| {
            assert_eq!(request.path, "/muse-code/key");
            assert_eq!(
                request.authorization.as_deref(),
                Some("Bearer identity-token")
            );
            (StatusCode::OK, json!({ "api_key": "LLM|fresh-key" }))
        }))
        .await;

        let started = now_ms();
        let credential = mock
            .oauth()
            .refresh(
                &OAuthCredential {
                    refresh: "identity-token".to_owned(),
                    access: "LLM|old-key".to_owned(),
                    expires: 1,
                    extra: Map::new(),
                },
                None,
            )
            .await
            .expect("refresh");
        let finished = now_ms();

        assert_eq!(credential.refresh, "identity-token");
        assert_eq!(credential.access, "LLM|fresh-key");
        assert!(credential.expires >= started + DAY_MS);
        assert!(credential.expires <= finished + DAY_MS);
    }

    #[tokio::test]
    async fn reports_the_setup_url_when_meta_issues_no_key() {
        // Upstream test 3: a mint response without an api_key surfaces the
        // action_url ("Complete setup at …").
        let mock = MockMeta::start(Arc::new(|_| {
            (
                StatusCode::OK,
                json!({ "require_payment": true, "action_url": "https://dev.meta.ai/billing" }),
            )
        }))
        .await;

        let error = mock
            .oauth()
            .refresh(
                &OAuthCredential {
                    refresh: "identity-token".to_owned(),
                    access: String::new(),
                    expires: 1,
                    extra: Map::new(),
                },
                None,
            )
            .await
            .expect_err("mint without key");
        assert!(
            error
                .message
                .contains("Complete setup at https://dev.meta.ai/billing"),
            "message: {}",
            error.message
        );
    }

    #[tokio::test]
    async fn uses_the_minted_key_as_the_request_api_key() {
        // Upstream test 4: toAuth maps the minted key to the request api
        // key (no bearer header — the OpenAI-responses api sends it as the
        // key).
        let auth = MetaOAuth::new()
            .to_auth(&OAuthCredential {
                refresh: "identity-token".to_owned(),
                access: "LLM|key".to_owned(),
                expires: 1,
                extra: Map::new(),
            })
            .await
            .expect("to_auth");
        assert_eq!(auth.api_key.as_deref(), Some("LLM|key"));
        assert!(auth.headers.is_none());
        assert!(auth.base_url.is_none());
    }

    #[tokio::test]
    async fn mint_unauthorized_directs_to_relogin() {
        // FR-B R3 (`b73412a37` module header): the identity token is not
        // renewable — mint 401/403 fails fast (no silent retry) and
        // directs to `/login meta`.
        for status in [StatusCode::UNAUTHORIZED, StatusCode::FORBIDDEN] {
            let mock = MockMeta::start(Arc::new(move |request| {
                assert_eq!(request.path, "/muse-code/key");
                (
                    status,
                    json!({ "error": "token_expired", "error_description": "identity expired" }),
                )
            }))
            .await;
            let error = mock
                .oauth()
                .refresh(
                    &OAuthCredential {
                        refresh: "identity-token".to_owned(),
                        access: "LLM|dead".to_owned(),
                        expires: 1,
                        extra: Map::new(),
                    },
                    None,
                )
                .await
                .expect_err("unauthorized mint");
            assert!(
                error.message.contains(&format!(
                    "Meta session expired (status {}). Run `/login meta` to sign in again.",
                    status.as_u16()
                )),
                "message: {}",
                error.message
            );
            // The error detail from the body is appended.
            assert!(
                error.message.ends_with(": identity expired"),
                "message: {}",
                error.message
            );
            // Exactly one mint attempt — no silent retry.
            assert_eq!(
                mock.requests()
                    .iter()
                    .filter(|request| request.path == "/muse-code/key")
                    .count(),
                1
            );
        }
    }
}
