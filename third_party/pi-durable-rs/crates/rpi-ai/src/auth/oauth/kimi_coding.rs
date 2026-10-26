//! Port of `packages/ai/src/auth/oauth/kimi-coding.ts` @ pi 0.82.1
//! (2efa728) — Kimi Code (subscription) OAuth: RFC 8628 device code flow
//! against `https://auth.kimi.com` (overridable via `KIMI_CODE_OAUTH_HOST` /
//! `KIMI_OAUTH_HOST`), JSON responses, 30s request timeout, refresh with
//! bounded retries. The access token authenticates requests to
//! `https://api.kimi.com/coding` as an `Authorization: Bearer` header.
//!
//! Test seams (upstream stubs the global `fetch` / `vi.stubEnv`; here they
//! are constructor fields — same precedent as `anthropic.rs`'s
//! `token_url`/`callback_port`):
//! - `oauth_host`: when set, wins over the env-var override and the default
//!   host. The upstream "honors the KIMI_CODE_OAUTH_HOST override" test uses
//!   `vi.stubEnv`; a constructor field keeps the process-global env out of
//!   parallel tests. The env-var chain itself is pinned by a dedicated
//!   `EnvGuard` test (distinct names, the only test touching them).
//!
//! Intentional differences:
//! - `AbortSignal.timeout(30_000)` becomes a client-level reqwest timeout;
//!   a cancelled `CancellationToken` races the request and surfaces
//!   [`super::device_code::CANCEL_MESSAGE`] (upstream rejects with the raw
//!   `AbortError`; same precedent as `radius.rs`'s token requests);
//! - poll-closure fetch errors surface via `DeviceCodePollResult::Failed`
//!   with the reqwest error text (upstream rethrows them through the
//!   polling framework);
//! - `Date.now()` becomes `SystemTime` milliseconds; `interval`/`expires_in`
//!   parse as `f64` (JS `number`) and narrow into the `u64` event fields and
//!   the `i64` credential field (no expiry skew — upstream kimi-coding.ts
//!   has none, unlike anthropic/xai).

use std::sync::Arc;

use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

use super::super::interaction::{AuthEvent, AuthInteraction};
use super::super::resolve::{ModelsError, ModelsErrorCode};
use super::super::types::{LoginOptions, ModelAuth, OAuthAuth, OAuthCredential};
use super::device_code::{
    CANCEL_MESSAGE, DeviceCodePollOptions, DeviceCodePollResult, poll_oauth_device_code_flow,
};
use crate::utils::provider_env::get_provider_env_value;

/// `CLIENT_ID`.
const CLIENT_ID: &str = "17e5f671-d194-4dfb-9706-5516cb48c098";
/// `DEFAULT_OAUTH_HOST`.
const DEFAULT_OAUTH_HOST: &str = "https://auth.kimi.com";
/// `DEVICE_CODE_TIMEOUT_SECONDS` = 15 * 60.
const DEVICE_CODE_TIMEOUT_SECONDS: f64 = 15.0 * 60.0;
/// `DEFAULT_POLL_INTERVAL_SECONDS`.
const DEFAULT_POLL_INTERVAL_SECONDS: f64 = 5.0;
/// `AbortSignal.timeout(30_000)`.
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// `REFRESH_MAX_RETRIES`.
const REFRESH_MAX_RETRIES: u32 = 3;
/// `grant_type` of the device-code poll (`urn:ietf:params:oauth:grant-type:device_code`).
const DEVICE_CODE_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:device_code";

fn error(message: impl Into<String>) -> ModelsError {
    ModelsError::new(ModelsErrorCode::Oauth, message.into())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

/// `getOauthHost` — the `KIMI_CODE_OAUTH_HOST` override wins, then
/// `KIMI_OAUTH_HOST`, then the default; trailing slashes are stripped
/// (`replace(/\/+$/, "")`). The optional `seam` (test-only constructor
/// field) wins over everything.
fn resolve_oauth_host(seam: Option<&str>) -> String {
    let host = seam
        .map(str::to_owned)
        .or_else(|| get_provider_env_value("KIMI_CODE_OAUTH_HOST", None))
        .or_else(|| get_provider_env_value("KIMI_OAUTH_HOST", None))
        .unwrap_or_else(|| DEFAULT_OAUTH_HOST.to_owned());
    host.trim_end_matches('/').to_owned()
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

/// `interval`/`expires_in` fallback: a positive finite number wins, else the
/// caller's default (JS `Number.isFinite(...) && ... > 0 ? ... : default`).
fn positive_or_default(value: Option<f64>, default: f64) -> f64 {
    match value {
        Some(value) if value.is_finite() && value > 0.0 => value,
        _ => default,
    }
}

/// `DeviceAuthorization` — upstream also carries `verification_uri`; here it
/// is only trust-checked in `start_device_authorization` (the login notifies
/// `verification_uri_complete`), so it is not stored.
struct DeviceAuthorization {
    device_code: String,
    user_code: String,
    verification_uri_complete: String,
    interval_seconds: f64,
    expires_in_seconds: f64,
}

/// `TokenResponse`.
#[derive(Debug)]
struct TokenResponse {
    access: String,
    refresh: String,
    expires: i64,
}

/// `kimiCodingOAuth` — the Kimi Code (subscription) OAuth provider auth.
pub fn kimi_coding_oauth() -> Arc<dyn OAuthAuth> {
    Arc::new(KimiCodingOAuth::new())
}

/// Kimi Code OAuth (`OAuthAuth`) implementation.
pub struct KimiCodingOAuth {
    client: reqwest::Client,
    /// `getOauthHost` test seam — see module docs.
    oauth_host: Option<String>,
}

impl Default for KimiCodingOAuth {
    fn default() -> Self {
        Self::new()
    }
}

impl KimiCodingOAuth {
    pub fn new() -> Self {
        let client = match reqwest::Client::builder().timeout(REQUEST_TIMEOUT).build() {
            Ok(client) => client,
            // Unreachable with this configuration; fall back rather than
            // panic (no unwrap in non-test code).
            Err(_) => reqwest::Client::new(),
        };
        Self {
            client,
            oauth_host: None,
        }
    }

    /// Test seam — see module docs.
    pub fn with_oauth_host(mut self, oauth_host: impl Into<String>) -> Self {
        self.oauth_host = Some(oauth_host.into());
        self
    }

    fn get_oauth_host(&self) -> String {
        resolve_oauth_host(self.oauth_host.as_deref())
    }

    /// `requestSignal(signal)` — send a form-encoded POST, racing the
    /// request against the flow cancellation signal.
    async fn send(
        &self,
        url: String,
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

    /// `startDeviceAuthorization` — POST `{host}/api/oauth/device_authorization`
    /// (`client_id`), validate the response shape and trust-check both
    /// verification URIs.
    async fn start_device_authorization(
        &self,
        oauth_host: &str,
        signal: Option<&CancellationToken>,
    ) -> Result<DeviceAuthorization, ModelsError> {
        let url = format!("{oauth_host}/api/oauth/device_authorization");
        let response = self.send(url, &[("client_id", CLIENT_ID)], signal).await?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let text = response.text().await.unwrap_or_default();
            return Err(error(format!(
                "Kimi Code device authorization failed with status {status}{text}"
            )));
        }

        let json = read_json(response).await;
        let Some(object) = json.as_object() else {
            return Err(error(format!(
                "Invalid Kimi Code device authorization response: {json}"
            )));
        };
        let (Some(device_code), Some(user_code)) = (
            object.get("device_code").and_then(Value::as_str),
            object.get("user_code").and_then(Value::as_str),
        ) else {
            return Err(error(format!(
                "Invalid Kimi Code device authorization response: {json}"
            )));
        };
        // Both URIs are trust-checked; only `verification_uri_complete` is
        // carried (upstream keeps `verification_uri` in the type but the
        // login only notifies the complete URI).
        let (Some(_), Some(verification_uri_complete)) = (
            trusted_http_url(object.get("verification_uri")),
            trusted_http_url(object.get("verification_uri_complete")),
        ) else {
            return Err(error(format!(
                "Invalid Kimi Code device authorization response: {json}"
            )));
        };

        let interval = object.get("interval").and_then(Value::as_f64);
        let expires_in = object.get("expires_in").and_then(Value::as_f64);
        Ok(DeviceAuthorization {
            device_code: device_code.to_owned(),
            user_code: user_code.to_owned(),
            verification_uri_complete,
            interval_seconds: positive_or_default(interval, DEFAULT_POLL_INTERVAL_SECONDS),
            expires_in_seconds: positive_or_default(expires_in, DEVICE_CODE_TIMEOUT_SECONDS),
        })
    }

    /// `parseTokenResponse` — requires non-empty `access_token` /
    /// `refresh_token` and a positive finite `expires_in`;
    /// `expires = Date.now() + expires_in * 1000` (no skew).
    fn parse_token_response(json: &Value, operation: &str) -> Result<TokenResponse, ModelsError> {
        let access_token = json.get("access_token").and_then(Value::as_str);
        let refresh_token = json.get("refresh_token").and_then(Value::as_str);
        let expires_in = json.get("expires_in").and_then(Value::as_f64);
        if let (Some(access_token), Some(refresh_token), Some(expires_in)) =
            (access_token, refresh_token, expires_in)
            && !access_token.is_empty()
            && !refresh_token.is_empty()
            && expires_in > 0.0
        {
            return Ok(TokenResponse {
                access: access_token.to_owned(),
                refresh: refresh_token.to_owned(),
                expires: now_ms() + (expires_in * 1000.0) as i64,
            });
        }
        Err(error(format!(
            "Kimi Code token {operation} response missing fields: {json}"
        )))
    }

    /// One token poll (`poll` closure of `pollForToken`): POST
    /// `{host}/api/oauth/token`; map the RFC 8628 error fields onto the
    /// poll result.
    async fn poll_token(
        &self,
        oauth_host: &str,
        device: &DeviceAuthorization,
        signal: Option<CancellationToken>,
    ) -> DeviceCodePollResult<TokenResponse> {
        let url = format!("{oauth_host}/api/oauth/token");
        let response = match self
            .send(
                url,
                &[
                    ("client_id", CLIENT_ID),
                    ("device_code", device.device_code.as_str()),
                    ("grant_type", DEVICE_CODE_GRANT_TYPE),
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

        if status >= 500 {
            let text = response.text().await.unwrap_or_default();
            return DeviceCodePollResult::Failed {
                message: format!(
                    "Kimi Code device token request failed with status {status}{text}"
                ),
            };
        }

        let json = read_json(response).await;
        if ok && json.get("access_token").is_some_and(Value::is_string) {
            return match Self::parse_token_response(&json, "poll") {
                Ok(token) => DeviceCodePollResult::Complete { value: token },
                Err(parse_error) => DeviceCodePollResult::Failed {
                    message: parse_error.message,
                },
            };
        }

        let error_code = json.get("error").and_then(Value::as_str);
        let description = json.get("error_description").and_then(Value::as_str);
        match error_code {
            Some("authorization_pending") => DeviceCodePollResult::Pending,
            Some("slow_down") => {
                let interval = json.get("interval").and_then(Value::as_f64);
                DeviceCodePollResult::SlowDown {
                    interval_seconds: interval.filter(|seconds| *seconds > 0.0),
                }
            }
            Some("expired_token") => DeviceCodePollResult::Failed {
                message: "Kimi Code device authorization expired. Please restart login.".to_owned(),
            },
            Some("access_denied") => DeviceCodePollResult::Failed {
                message: "Kimi Code login was denied.".to_owned(),
            },
            _ => {
                let suffix = match (error_code, description) {
                    (Some(error_code), description) => {
                        let description = description
                            .map(|description| format!(": {description}"))
                            .unwrap_or_default();
                        format!(": {error_code}{description}")
                    }
                    (None, _) => String::new(),
                };
                DeviceCodePollResult::Failed {
                    message: format!(
                        "Kimi Code device token request failed (status {status}){suffix}"
                    ),
                }
            }
        }
    }

    /// `pollForToken` — `waitBeforeFirstPoll: true`.
    async fn poll_for_token(
        &self,
        oauth_host: &str,
        device: &DeviceAuthorization,
        signal: Option<CancellationToken>,
    ) -> Result<TokenResponse, ModelsError> {
        let poll_signal = signal.clone();
        poll_oauth_device_code_flow(DeviceCodePollOptions {
            interval_seconds: Some(device.interval_seconds),
            expires_in_seconds: Some(device.expires_in_seconds),
            wait_before_first_poll: true,
            signal,
            poll: move || self.poll_token(oauth_host, device, poll_signal.clone()),
        })
        .await
    }

    /// `isRetryableRefreshFailure`.
    fn is_retryable_refresh_failure(status: u16) -> bool {
        status == 429 || status >= 500
    }

    /// `refreshToken` — `refresh_token` grant against
    /// `{host}/api/oauth/token` with exponential backoff (1s/2s/4s, plain
    /// sleeps like the upstream `setTimeout`), up to `REFRESH_MAX_RETRIES`
    /// on retryable failures. 401/403/`invalid_grant` fail immediately: the
    /// stored credential is dead.
    async fn refresh_token(
        &self,
        oauth_host: &str,
        refresh_token_value: &str,
        signal: Option<&CancellationToken>,
    ) -> Result<TokenResponse, ModelsError> {
        let mut last_error: Option<ModelsError> = None;
        for attempt in 0..=REFRESH_MAX_RETRIES {
            if attempt > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(
                    1000 * 2u64.pow(attempt - 1),
                ))
                .await;
            }
            if signal.is_some_and(CancellationToken::is_cancelled) {
                return Err(error("Kimi Code token refresh aborted"));
            }

            let url = format!("{oauth_host}/api/oauth/token");
            let response = match self
                .send(
                    url,
                    &[
                        ("client_id", CLIENT_ID),
                        ("grant_type", "refresh_token"),
                        ("refresh_token", refresh_token_value),
                    ],
                    signal,
                )
                .await
            {
                // Network errors (and in-flight aborts) are retried; the
                // top-of-loop check above turns a live abort into the
                // "aborted" error on the next attempt.
                Err(request_error) => {
                    last_error = Some(request_error);
                    continue;
                }
                Ok(response) => response,
            };

            let status = response.status().as_u16();
            let ok = response.status().is_success();
            let json = read_json(response).await;
            if ok {
                return Self::parse_token_response(&json, "refresh");
            }

            // Unauthorized: the stored credential is dead; Models clears it
            // and prompts re-login.
            let unauthorized = status == 401
                || status == 403
                || json.get("error").and_then(Value::as_str) == Some("invalid_grant");
            if unauthorized {
                let description = json
                    .get("error_description")
                    .and_then(Value::as_str)
                    .map(|description| format!(": {description}"))
                    .unwrap_or_default();
                return Err(error(format!(
                    "Kimi Code token refresh unauthorized (status {status}){description}"
                )));
            }

            if Self::is_retryable_refresh_failure(status) && attempt < REFRESH_MAX_RETRIES {
                last_error = Some(error(format!(
                    "Kimi Code token refresh failed with status {status}"
                )));
                continue;
            }

            return Err(error(format!(
                "Kimi Code token refresh failed with status {status}: {json}"
            )));
        }

        Err(last_error.unwrap_or_else(|| error("Kimi Code token refresh failed")))
    }

    /// `loginKimiCoding`.
    async fn login_kimi_coding(
        &self,
        interaction: &dyn AuthInteraction,
    ) -> Result<OAuthCredential, ModelsError> {
        let oauth_host = self.get_oauth_host();
        let device = self
            .start_device_authorization(&oauth_host, interaction.signal().as_ref())
            .await?;
        interaction.notify(AuthEvent::DeviceCode {
            user_code: device.user_code.clone(),
            verification_uri: device.verification_uri_complete.clone(),
            interval_seconds: Some(device.interval_seconds as u64),
            expires_in_seconds: Some(device.expires_in_seconds as u64),
        });
        let token = self
            .poll_for_token(&oauth_host, &device, interaction.signal())
            .await?;
        Ok(OAuthCredential {
            refresh: token.refresh,
            access: token.access,
            expires: token.expires,
            extra: Map::new(),
        })
    }
}

#[async_trait::async_trait]
impl OAuthAuth for KimiCodingOAuth {
    fn name(&self) -> &str {
        "Kimi Code (subscription)"
    }

    /// `isSubscription: true` (providers/kimi-coding.ts:16 @ 4181f66).
    fn is_subscription(&self) -> bool {
        true
    }

    /// `login` — device code flow (RFC 8628), no prompt (upstream "Kimi Code
    /// login should not prompt").
    async fn login(
        &self,
        interaction: &dyn AuthInteraction,
        _options: Option<&LoginOptions>,
    ) -> Result<OAuthCredential, ModelsError> {
        self.login_kimi_coding(interaction).await
    }

    /// `refresh: (credential, signal) => refreshToken(getOauthHost(),
    /// credential.refresh, signal)`.
    async fn refresh(
        &self,
        credential: &OAuthCredential,
        signal: Option<&CancellationToken>,
    ) -> Result<OAuthCredential, ModelsError> {
        let token = self
            .refresh_token(&self.get_oauth_host(), &credential.refresh, signal)
            .await?;
        Ok(OAuthCredential {
            refresh: token.refresh,
            access: token.access,
            expires: token.expires,
            extra: Map::new(),
        })
    }

    /// `toAuth: { headers: { Authorization: "Bearer ..." } }`.
    async fn to_auth(&self, credential: &OAuthCredential) -> Result<ModelAuth, ModelsError> {
        let mut headers = crate::types::ProviderHeaders::new();
        headers.insert(
            "Authorization".to_owned(),
            Some(format!("Bearer {}", credential.access)),
        );
        Ok(ModelAuth {
            api_key: None,
            headers: Some(headers),
            base_url: None,
        })
    }
}

#[cfg(test)]
mod tests {
    //! Test intents ported from `packages/ai/test/kimi-coding-oauth.test.ts`
    //! @ pi 0.82.1 (2efa728); the mocked `fetch` becomes a loopback axum
    //! server behind the `oauth_host` seam (module docs), and the
    //! fake-timer interval assertions stay in `device_code.rs`'s fake-clock
    //! tests (the flow-level checks here assert the request/response
    //! mapping with a 1s interval instead).

    use std::sync::Mutex;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::response::Json;
    use serde_json::json;
    use tokio::sync::oneshot;

    use super::super::super::interaction::AuthPrompt;
    use super::super::super::types::BoxFutureSend;
    use super::*;

    // ----- mock auth endpoints (upstream: `vi.stubGlobal("fetch")`) -----

    #[derive(Debug, Clone)]
    struct RecordedRequest {
        method: String,
        path: String,
        body: String,
    }

    impl RecordedRequest {
        /// `new URLSearchParams(String(init?.body))` → `form.get(name)`.
        fn form_get(&self, name: &str) -> Option<String> {
            url::form_urlencoded::parse(self.body.as_bytes())
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.into_owned())
        }
    }

    type Responder = Arc<dyn Fn(&RecordedRequest) -> (StatusCode, Value) + Send + Sync + 'static>;

    struct MockAuth {
        url: String,
        requests: Arc<Mutex<Vec<RecordedRequest>>>,
        shutdown: Option<oneshot::Sender<()>>,
    }

    impl MockAuth {
        async fn start(responder: Responder) -> Self {
            let requests: Arc<Mutex<Vec<RecordedRequest>>> = Arc::new(Mutex::new(Vec::new()));
            let handler_requests = requests.clone();
            let app = axum::Router::new().fallback(move |request: Request<Body>| {
                let requests = handler_requests.clone();
                let responder = responder.clone();
                async move {
                    let recorded = RecordedRequest {
                        method: request.method().to_string(),
                        path: request.uri().path().to_owned(),
                        body: axum::body::to_bytes(request.into_body(), usize::MAX)
                            .await
                            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                            .unwrap_or_default(),
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

        fn oauth(&self) -> KimiCodingOAuth {
            KimiCodingOAuth::new().with_oauth_host(self.url.clone())
        }

        fn requests(&self) -> Vec<RecordedRequest> {
            self.requests.lock().expect("lock").clone()
        }
    }

    impl Drop for MockAuth {
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

    impl FakeInteraction {
        fn new(handle: InteractionHandle) -> Self {
            Self { handle }
        }
    }

    impl AuthInteraction for FakeInteraction {
        fn prompt<'a>(
            &'a self,
            prompt: AuthPrompt,
        ) -> BoxFutureSend<'a, Result<String, ModelsError>> {
            // Upstream: "Kimi Code login should not prompt".
            Box::pin(async move {
                Err(ModelsError::new(
                    ModelsErrorCode::Auth,
                    format!("Kimi Code login should not prompt: {prompt:?}"),
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
            "user_code": "ABCD-1234",
            "device_code": "device-code-123",
            "verification_uri": "https://www.kimi.com/code",
            "verification_uri_complete": "https://www.kimi.com/code?user_code=ABCD-1234",
            "interval": 1,
            "expires_in": 600,
        })
    }

    fn token_response(access: &str, refresh: &str) -> Value {
        json!({
            "access_token": access,
            "refresh_token": refresh,
            "expires_in": 3600,
        })
    }

    /// Happy-path responder: device authorization → pending → token.
    fn happy_path_responder() -> Responder {
        let polls = Arc::new(Mutex::new(0usize));
        Arc::new(move |request: &RecordedRequest| {
            let path = request.path.as_str();
            if path.ends_with("/api/oauth/device_authorization") {
                assert_eq!(request.method, "POST");
                assert_eq!(request.form_get("client_id").as_deref(), Some(CLIENT_ID));
                return (StatusCode::OK, device_authorization_response());
            }
            if path.ends_with("/api/oauth/token") {
                assert_eq!(
                    request.form_get("grant_type").as_deref(),
                    Some("urn:ietf:params:oauth:grant-type:device_code")
                );
                assert_eq!(request.form_get("client_id").as_deref(), Some(CLIENT_ID));
                assert_eq!(
                    request.form_get("device_code").as_deref(),
                    Some("device-code-123")
                );
                let mut count = polls.lock().expect("lock");
                *count += 1;
                return match *count {
                    1 => (
                        StatusCode::BAD_REQUEST,
                        json!({ "error": "authorization_pending" }),
                    ),
                    _ => (
                        StatusCode::OK,
                        token_response("access-token", "refresh-token"),
                    ),
                };
            }
            panic!("unexpected request: {} {}", request.method, request.path);
        })
    }

    // ----- pure-function tests -----

    /// `getOauthHost` env chain: `KIMI_CODE_OAUTH_HOST` wins over
    /// `KIMI_OAUTH_HOST`, trailing slashes are stripped, the seam wins over
    /// env, the default is last. (Distinct env names, only touched here.)
    #[test]
    fn oauth_host_override_chain() {
        struct EnvGuard(Vec<&'static str>);
        impl EnvGuard {
            fn set(entries: &[(&'static str, &str)]) -> Self {
                for (name, value) in entries {
                    rpi_test_env::set_var(name, value);
                }
                Self(entries.iter().map(|(name, _)| *name).collect())
            }
        }
        impl Drop for EnvGuard {
            fn drop(&mut self) {
                for name in &self.0 {
                    rpi_test_env::remove_var(name);
                }
            }
        }

        // Empty overrides are falsy (JS `||`): fall through to the default.
        let _guard = EnvGuard::set(&[("KIMI_CODE_OAUTH_HOST", ""), ("KIMI_OAUTH_HOST", "")]);
        assert_eq!(resolve_oauth_host(None), DEFAULT_OAUTH_HOST);

        let _guard = EnvGuard::set(&[("KIMI_CODE_OAUTH_HOST", "https://auth.example.com/")]);
        assert_eq!(resolve_oauth_host(None), "https://auth.example.com");

        let _guard = EnvGuard::set(&[
            ("KIMI_CODE_OAUTH_HOST", ""),
            ("KIMI_OAUTH_HOST", "https://fallback.example.com///"),
        ]);
        assert_eq!(resolve_oauth_host(None), "https://fallback.example.com");

        // The seam (constructor field) wins over the env override.
        let _guard = EnvGuard::set(&[("KIMI_CODE_OAUTH_HOST", "https://auth.example.com")]);
        assert_eq!(
            resolve_oauth_host(Some("http://127.0.0.1:9")),
            "http://127.0.0.1:9"
        );
    }

    /// `trustedHttpUrl` — only non-empty http(s) URLs survive, normalized.
    #[test]
    fn trusted_http_url_accepts_only_http_schemes() {
        assert_eq!(
            trusted_http_url(Some(&json!("https://www.kimi.com/code"))),
            Some("https://www.kimi.com/code".to_owned())
        );
        assert_eq!(
            trusted_http_url(Some(&json!("http://www.kimi.com/code"))),
            Some("http://www.kimi.com/code".to_owned())
        );
        assert_eq!(trusted_http_url(Some(&json!("file:///etc/passwd"))), None);
        assert_eq!(trusted_http_url(Some(&json!("not a url"))), None);
        assert_eq!(trusted_http_url(Some(&json!(""))), None);
        assert_eq!(trusted_http_url(Some(&json!(42))), None);
        assert_eq!(trusted_http_url(None), None);
    }

    /// `parseTokenResponse` — missing/empty/non-positive fields fail with
    /// the verbatim message; `expires` has no skew.
    #[test]
    fn parse_token_response_requires_all_fields() {
        let ok = KimiCodingOAuth::parse_token_response(
            &json!({ "access_token": "a", "refresh_token": "r", "expires_in": 3600 }),
            "poll",
        )
        .expect("ok");
        assert_eq!(ok.access, "a");
        assert_eq!(ok.refresh, "r");
        assert!(ok.expires >= now_ms() + 3600 * 1000);

        for missing in [
            json!({ "refresh_token": "r", "expires_in": 3600 }),
            json!({ "access_token": "", "refresh_token": "r", "expires_in": 3600 }),
            json!({ "access_token": "a", "expires_in": 3600 }),
            json!({ "access_token": "a", "refresh_token": "r", "expires_in": 0 }),
            json!({ "access_token": "a", "refresh_token": "r", "expires_in": "3600" }),
        ] {
            let error = KimiCodingOAuth::parse_token_response(&missing, "poll").expect_err("bad");
            assert_eq!(
                error.message,
                format!("Kimi Code token poll response missing fields: {missing}")
            );
        }
        let error =
            KimiCodingOAuth::parse_token_response(&Value::Null, "refresh").expect_err("null");
        assert_eq!(
            error.message,
            "Kimi Code token refresh response missing fields: null"
        );
    }

    // ----- flow tests against the mock -----

    /// `logs in with the device authorization flow` — verbatim upstream
    /// assertions: device-code event (verification_uri_complete preferred),
    /// `waitBeforeFirstPoll` honored, form fields, credential shape with the
    /// no-skew expiry.
    #[tokio::test]
    async fn login_happy_path_reports_device_code_and_completes() {
        let mock = MockAuth::start(happy_path_responder()).await;
        let oauth = mock.oauth();
        let handle = InteractionHandle::default();
        let interaction = FakeInteraction::new(handle.clone());

        let before = now_ms();
        let credential = oauth.login(&interaction, None).await.expect("login");
        assert_eq!(credential.access, "access-token");
        assert_eq!(credential.refresh, "refresh-token");
        // No expiry skew (upstream `Date.now() + expires_in * 1000`).
        assert!(credential.expires >= before + 3600 * 1000);
        assert!(credential.expires <= before + 3600 * 1000 + 5000);

        match handle.events().as_slice() {
            [
                AuthEvent::DeviceCode {
                    user_code,
                    verification_uri,
                    interval_seconds,
                    expires_in_seconds,
                },
            ] => {
                assert_eq!(user_code, "ABCD-1234");
                assert_eq!(
                    verification_uri,
                    "https://www.kimi.com/code?user_code=ABCD-1234"
                );
                assert_eq!(interval_seconds, &Some(1));
                assert_eq!(expires_in_seconds, &Some(600));
            }
            other => panic!("unexpected events: {other:?}"),
        }

        let requests = mock.requests();
        assert_eq!(requests.len(), 3, "device auth + pending + token");
        assert_eq!(requests[0].path, "/api/oauth/device_authorization");
        assert_eq!(requests[1].path, "/api/oauth/token");
        assert_eq!(requests[2].path, "/api/oauth/token");
    }

    /// `fails when the device code expires`.
    #[tokio::test]
    async fn expired_token_fails_with_verbatim_message() {
        let responder: Responder = Arc::new(|request: &RecordedRequest| {
            let path = request.path.as_str();
            if path.ends_with("/api/oauth/device_authorization") {
                return (StatusCode::OK, device_authorization_response());
            }
            if path.ends_with("/api/oauth/token") {
                return (StatusCode::BAD_REQUEST, json!({ "error": "expired_token" }));
            }
            panic!("unexpected request: {} {}", request.method, request.path);
        });
        let mock = MockAuth::start(responder).await;
        let handle = InteractionHandle::default();
        let interaction = FakeInteraction::new(handle);

        let error = mock
            .oauth()
            .login(&interaction, None)
            .await
            .expect_err("expired");
        assert_eq!(
            error.message,
            "Kimi Code device authorization expired. Please restart login."
        );
    }

    /// `fails when the user denies the login`.
    #[tokio::test]
    async fn access_denied_fails_with_verbatim_message() {
        let responder: Responder = Arc::new(|request: &RecordedRequest| {
            let path = request.path.as_str();
            if path.ends_with("/api/oauth/device_authorization") {
                return (StatusCode::OK, device_authorization_response());
            }
            if path.ends_with("/api/oauth/token") {
                return (StatusCode::BAD_REQUEST, json!({ "error": "access_denied" }));
            }
            panic!("unexpected request: {} {}", request.method, request.path);
        });
        let mock = MockAuth::start(responder).await;
        let handle = InteractionHandle::default();
        let interaction = FakeInteraction::new(handle);

        let error = mock
            .oauth()
            .login(&interaction, None)
            .await
            .expect_err("denied");
        assert_eq!(error.message, "Kimi Code login was denied.");
    }

    /// Server-side failures (>= 500) fail the poll with the status message.
    #[tokio::test]
    async fn poll_5xx_fails_with_status_message() {
        let responder: Responder = Arc::new(|request: &RecordedRequest| {
            let path = request.path.as_str();
            if path.ends_with("/api/oauth/device_authorization") {
                return (StatusCode::OK, device_authorization_response());
            }
            if path.ends_with("/api/oauth/token") {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    json!({ "error": "boom" }),
                );
            }
            panic!("unexpected request: {} {}", request.method, request.path);
        });
        let mock = MockAuth::start(responder).await;
        let handle = InteractionHandle::default();
        let interaction = FakeInteraction::new(handle);

        let error = mock
            .oauth()
            .login(&interaction, None)
            .await
            .expect_err("5xx");
        assert!(
            error
                .message
                .starts_with("Kimi Code device token request failed with status 500"),
            "unexpected message: {}",
            error.message
        );
    }

    /// Unknown device-flow errors surface with the code + description.
    #[tokio::test]
    async fn poll_unknown_error_maps_to_failed_message() {
        let responder: Responder = Arc::new(|request: &RecordedRequest| {
            let path = request.path.as_str();
            if path.ends_with("/api/oauth/device_authorization") {
                return (StatusCode::OK, device_authorization_response());
            }
            if path.ends_with("/api/oauth/token") {
                return (
                    StatusCode::BAD_REQUEST,
                    json!({ "error": "unsupported_grant_type", "error_description": "nope" }),
                );
            }
            panic!("unexpected request: {} {}", request.method, request.path);
        });
        let mock = MockAuth::start(responder).await;
        let handle = InteractionHandle::default();
        let interaction = FakeInteraction::new(handle);

        let error = mock
            .oauth()
            .login(&interaction, None)
            .await
            .expect_err("unknown");
        assert_eq!(
            error.message,
            "Kimi Code device token request failed (status 400): unsupported_grant_type: nope"
        );
    }

    /// A malformed device-authorization response fails before any poll, with
    /// the verbatim message (the parsed body echoed back; JS `typeof`
    /// "object" admits arrays, so a string body echoes as `null`).
    #[tokio::test]
    async fn login_rejects_malformed_device_response() {
        let responder: Responder =
            Arc::new(|_: &RecordedRequest| (StatusCode::OK, json!(["not", "an", "object"])));
        let mock = MockAuth::start(responder).await;
        let handle = InteractionHandle::default();
        let interaction = FakeInteraction::new(handle);
        let error = mock
            .oauth()
            .login(&interaction, None)
            .await
            .expect_err("array");
        assert_eq!(
            error.message,
            "Invalid Kimi Code device authorization response: [\"not\",\"an\",\"object\"]"
        );

        let responder: Responder = Arc::new(|_: &RecordedRequest| (StatusCode::OK, json!("")));
        let mock = MockAuth::start(responder).await;
        let handle = InteractionHandle::default();
        let interaction = FakeInteraction::new(handle);
        let error = mock
            .oauth()
            .login(&interaction, None)
            .await
            .expect_err("string");
        assert_eq!(
            error.message,
            "Invalid Kimi Code device authorization response: null"
        );
    }

    /// A non-http(s) verification_uri_complete is rejected before it reaches
    /// the device_code event.
    #[tokio::test]
    async fn login_rejects_untrusted_verification_uri_complete() {
        let responder: Responder = Arc::new(|_: &RecordedRequest| {
            let mut body = device_authorization_response();
            body["verification_uri_complete"] = json!("file:///etc/passwd");
            (StatusCode::OK, body)
        });
        let mock = MockAuth::start(responder).await;
        let handle = InteractionHandle::default();
        let interaction = FakeInteraction::new(handle.clone());

        let error = mock
            .oauth()
            .login(&interaction, None)
            .await
            .expect_err("untrusted");
        assert!(
            error
                .message
                .starts_with("Invalid Kimi Code device authorization response: "),
            "unexpected message: {}",
            error.message
        );
        assert!(handle.events().is_empty(), "no event may be notified");
    }

    /// A non-2xx device-authorization response fails with the status + body.
    #[tokio::test]
    async fn device_authorization_http_error_shape() {
        let responder: Responder = Arc::new(|_: &RecordedRequest| {
            (
                StatusCode::BAD_REQUEST,
                json!({ "error": "invalid_client" }),
            )
        });
        let mock = MockAuth::start(responder).await;
        let handle = InteractionHandle::default();
        let interaction = FakeInteraction::new(handle);

        let error = mock
            .oauth()
            .login(&interaction, None)
            .await
            .expect_err("http 400");
        assert!(
            error
                .message
                .starts_with("Kimi Code device authorization failed with status 400"),
            "unexpected message: {}",
            error.message
        );
    }

    /// `retries refresh on 429` — one 429, then success (backoff 1s).
    #[tokio::test]
    async fn refresh_retries_429_then_succeeds() {
        let calls = Arc::new(Mutex::new(0usize));
        let responder_calls = calls.clone();
        let responder: Responder = Arc::new(move |request: &RecordedRequest| {
            assert!(request.path.ends_with("/api/oauth/token"));
            assert_eq!(
                request.form_get("grant_type").as_deref(),
                Some("refresh_token")
            );
            assert_eq!(request.form_get("client_id").as_deref(), Some(CLIENT_ID));
            assert_eq!(
                request.form_get("refresh_token").as_deref(),
                Some("old-refresh")
            );
            let mut count = responder_calls.lock().expect("lock");
            *count += 1;
            if *count == 1 {
                return (
                    StatusCode::TOO_MANY_REQUESTS,
                    json!({ "error": "temporarily_unavailable" }),
                );
            }
            (StatusCode::OK, token_response("new-access", "new-refresh"))
        });
        let mock = MockAuth::start(responder).await;
        let oauth = mock.oauth();
        let credential = OAuthCredential {
            refresh: "old-refresh".to_owned(),
            access: "old-access".to_owned(),
            expires: 0,
            extra: Map::new(),
        };

        let refreshed = oauth.refresh(&credential, None).await.expect("refresh");
        assert_eq!(refreshed.access, "new-access");
        assert_eq!(refreshed.refresh, "new-refresh");
        assert_eq!(*calls.lock().expect("lock"), 2);
    }

    /// `retries refresh on 5xx` — one 500, then success.
    #[tokio::test]
    async fn refresh_retries_5xx_then_succeeds() {
        let calls = Arc::new(Mutex::new(0usize));
        let responder_calls = calls.clone();
        let responder: Responder = Arc::new(move |request: &RecordedRequest| {
            assert!(request.path.ends_with("/api/oauth/token"));
            let mut count = responder_calls.lock().expect("lock");
            *count += 1;
            if *count == 1 {
                return (StatusCode::INTERNAL_SERVER_ERROR, json!({}));
            }
            (StatusCode::OK, token_response("a", "r"))
        });
        let mock = MockAuth::start(responder).await;
        let oauth = mock.oauth();
        let credential = OAuthCredential {
            refresh: "old-refresh".to_owned(),
            access: "old".to_owned(),
            expires: 0,
            extra: Map::new(),
        };

        let refreshed = oauth.refresh(&credential, None).await.expect("refresh");
        assert_eq!(refreshed.access, "a");
        assert_eq!(*calls.lock().expect("lock"), 2);
    }

    /// `fails unauthorized on invalid_grant` — not retried, verbatim message
    /// with the error_description.
    #[tokio::test]
    async fn refresh_invalid_grant_fails_unauthorized_without_retry() {
        let calls = Arc::new(Mutex::new(0usize));
        let responder_calls = calls.clone();
        let responder: Responder = Arc::new(move |request: &RecordedRequest| {
            assert!(request.path.ends_with("/api/oauth/token"));
            let mut count = responder_calls.lock().expect("lock");
            *count += 1;
            (
                StatusCode::BAD_REQUEST,
                json!({ "error": "invalid_grant", "error_description": "revoked" }),
            )
        });
        let mock = MockAuth::start(responder).await;
        let oauth = mock.oauth();
        let credential = OAuthCredential {
            refresh: "old-refresh".to_owned(),
            access: "old".to_owned(),
            expires: 0,
            extra: Map::new(),
        };

        let error = oauth
            .refresh(&credential, None)
            .await
            .expect_err("invalid_grant");
        assert_eq!(
            error.message,
            "Kimi Code token refresh unauthorized (status 400): revoked"
        );
        assert_eq!(*calls.lock().expect("lock"), 1);
    }

    /// A 401 refresh fails immediately (dead stored credential).
    #[tokio::test]
    async fn refresh_401_fails_unauthorized() {
        let responder: Responder = Arc::new(|request: &RecordedRequest| {
            assert!(request.path.ends_with("/api/oauth/token"));
            (
                StatusCode::UNAUTHORIZED,
                json!({ "error": "invalid_token" }),
            )
        });
        let mock = MockAuth::start(responder).await;
        let oauth = mock.oauth();
        let credential = OAuthCredential {
            refresh: "old-refresh".to_owned(),
            access: "old".to_owned(),
            expires: 0,
            extra: Map::new(),
        };

        let error = oauth.refresh(&credential, None).await.expect_err("401");
        assert_eq!(
            error.message,
            "Kimi Code token refresh unauthorized (status 401)"
        );
    }

    /// `toAuth` maps the access token to a Bearer header.
    #[tokio::test]
    async fn to_auth_maps_access_to_bearer_header() {
        let oauth = KimiCodingOAuth::new();
        let credential = OAuthCredential {
            refresh: "r".to_owned(),
            access: "access-token".to_owned(),
            expires: i64::MAX,
            extra: Map::new(),
        };
        let auth = oauth.to_auth(&credential).await.expect("to_auth");
        assert_eq!(auth.api_key, None);
        let headers = auth.headers.expect("headers");
        assert_eq!(
            headers.get("Authorization"),
            Some(&Some("Bearer access-token".to_owned()))
        );
    }
}
