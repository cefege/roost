//! Port of `packages/ai/src/auth/oauth/xai.ts` @ pi 0.82.1 (2efa728) — xAI
//! OAuth device-code flow (RFC 8628) against `https://auth.x.ai` for
//! SuperGrok / X Premium subscriptions; the access token is used as an
//! api key. Refresh keeps the previous refresh token when the server does
//! not rotate it, and applies a 5-minute expiry skew so the token never
//! dies mid-request.
//!
//! Test seams (upstream stubs the global `fetch`; here a constructor field —
//! same precedent as `github_copilot.rs`'s `authority`): `authority`
//! rewrites `https://auth.x.ai/{path}` → `http://{authority}/auth.x.ai/{path}`,
//! so one loopback mock stands in for both endpoints while the recorded
//! path still shows the real host.
//!
//! Intentional differences:
//! - `AbortSignal` becomes `CancellationToken`; a cancelled signal surfaces
//!   [`super::device_code::CANCEL_MESSAGE`] exactly where upstream rethrows
//!   `Error("Login cancelled")` from `postForm`;
//! - `Date.now()` becomes `SystemTime` milliseconds; `expires_in`/`interval`
//!   parse as `f64` (JS `number`) and narrow into the `u64` event fields and
//!   the `i64` credential field;
//! - poll-closure request errors surface via `DeviceCodePollResult::Failed`
//!   (upstream rethrows them through the polling framework).

use std::sync::Arc;

use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

use super::super::interaction::{AuthEvent, AuthInteraction};
use super::super::resolve::{ModelsError, ModelsErrorCode};
use super::super::types::{LoginOptions, ModelAuth, OAuthAuth, OAuthCredential};
use super::device_code::{
    CANCEL_MESSAGE, DeviceCodePollOptions, DeviceCodePollResult, poll_oauth_device_code_flow,
};

/// `XAI_CLIENT_ID`.
const XAI_CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
/// `XAI_SCOPE`.
const XAI_SCOPE: &str = "openid profile email offline_access grok-cli:access api:access";
/// `XAI_DEVICE_CODE_URL`.
const XAI_DEVICE_CODE_URL: &str = "https://auth.x.ai/oauth2/device/code";
/// `XAI_TOKEN_URL`.
const XAI_TOKEN_URL: &str = "https://auth.x.ai/oauth2/token";
/// `REFRESH_SKEW_MS` — refresh slightly before the reported expiry to avoid
/// using a token that dies mid-request.
const REFRESH_SKEW_MS: i64 = 5 * 60 * 1000;
/// `DEFAULT_TOKEN_LIFETIME_SECONDS`.
const DEFAULT_TOKEN_LIFETIME_SECONDS: f64 = 3600.0;
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

/// `OAuthHttpResponse`.
struct OAuthHttpResponse {
    ok: bool,
    status: u16,
    body: Map<String, Value>,
}

/// `XaiDeviceCode`.
#[derive(Debug)]
struct XaiDeviceCode {
    device_code: String,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: Option<String>,
    interval_seconds: Option<f64>,
    expires_in_seconds: f64,
}

/// `requiredString` — non-empty string field.
fn required_string(body: &Map<String, Value>, field: &str) -> Result<String, ModelsError> {
    match body.get(field).and_then(Value::as_str) {
        Some(value) if !value.is_empty() => Ok(value.to_owned()),
        _ => Err(error(format!("Invalid xAI OAuth response field: {field}"))),
    }
}

/// `positiveNumber` — finite positive number field.
fn positive_number(body: &Map<String, Value>, field: &str) -> Result<f64, ModelsError> {
    match body.get(field).and_then(Value::as_f64) {
        Some(value) if value > 0.0 => Ok(value),
        _ => Err(error(format!("Invalid xAI OAuth response field: {field}"))),
    }
}

/// `validateVerificationUri` — the verification URI is opened in the user's
/// browser; force it to be an https URL so a malicious response cannot make
/// `open` launch something else. Returns the parsed-and-serialized form
/// (JS `url.href`).
fn validate_verification_uri(raw: &str) -> Result<String, ModelsError> {
    let url = url::Url::parse(raw)
        .map_err(|_| error("Untrusted verification URI in xAI OAuth response"))?;
    if url.scheme() != "https" {
        return Err(error("Untrusted verification URI in xAI OAuth response"));
    }
    Ok(url.to_string())
}

/// `requestFailure`.
fn request_failure(action: &str, response: &OAuthHttpResponse) -> ModelsError {
    let error_code = response.body.get("error").and_then(Value::as_str);
    let description = response
        .body
        .get("error_description")
        .and_then(Value::as_str);
    let detail = [error_code, description]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(": ");
    let suffix = if detail.is_empty() {
        String::new()
    } else {
        format!(": {detail}")
    };
    error(format!(
        "xAI OAuth {action} failed (HTTP {}){suffix}",
        response.status
    ))
}

/// `postForm` — form-encoded POST with JSON response handling; aborts
/// surface as `Login cancelled`.
async fn post_form(
    client: &reqwest::Client,
    authority: Option<&str>,
    url: &str,
    fields: &[(&str, &str)],
    signal: Option<&CancellationToken>,
) -> Result<OAuthHttpResponse, ModelsError> {
    let rewritten = match authority {
        Some(authority) => match url.strip_prefix("https://") {
            Some(rest) => format!("http://{authority}/{rest}"),
            None => url.to_owned(),
        },
        None => url.to_owned(),
    };
    let send = client
        .post(rewritten)
        .header(reqwest::header::ACCEPT, "application/json")
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .form(fields)
        .send();
    let response = match signal {
        Some(token) => tokio::select! {
            () = token.cancelled() => return Err(error(CANCEL_MESSAGE)),
            response = send => response,
        },
        None => send.await,
    };
    let response = response.map_err(|request_error| {
        if signal.is_some_and(CancellationToken::is_cancelled) {
            error(CANCEL_MESSAGE)
        } else {
            error(request_error.to_string())
        }
    })?;
    let ok = response.status().is_success();
    let status = response.status().as_u16();
    let body = match response.json::<Value>().await {
        Ok(Value::Object(map)) => map,
        // JS: parsed-and-not-object bodies collapse to `{}`.
        Ok(_) => Map::new(),
        Err(_) => {
            if signal.is_some_and(CancellationToken::is_cancelled) {
                return Err(error(CANCEL_MESSAGE));
            }
            return Err(error(format!(
                "xAI OAuth returned invalid JSON (HTTP {status})"
            )));
        }
    };
    Ok(OAuthHttpResponse { ok, status, body })
}

/// `parseDeviceCode` — RFC 8628 allows `interval` 0 (no minimum wait);
/// fall back to the poller's default instead of failing on non-positive or
/// malformed values. `verification_uri_complete` is optional but must be
/// https when present.
fn parse_device_code(body: &Map<String, Value>) -> Result<XaiDeviceCode, ModelsError> {
    let interval = body.get("interval").and_then(Value::as_f64);
    let interval_seconds = interval.filter(|interval| *interval > 0.0);
    let verification_uri_complete = match body
        .get("verification_uri_complete")
        .and_then(Value::as_str)
    {
        Some(raw) if !raw.is_empty() => Some(validate_verification_uri(raw)?),
        _ => None,
    };
    Ok(XaiDeviceCode {
        device_code: required_string(body, "device_code")?,
        user_code: required_string(body, "user_code")?,
        verification_uri: validate_verification_uri(&required_string(body, "verification_uri")?)?,
        verification_uri_complete,
        interval_seconds,
        expires_in_seconds: positive_number(body, "expires_in")?,
    })
}

/// `credentialsFromTokenResponse` — `expires = Date.now() + expires_in *
/// 1000 - REFRESH_SKEW_MS`; xAI may omit `refresh_token` on refresh when
/// the token is not rotated.
fn credentials_from_token_response(
    body: &Map<String, Value>,
    previous_refresh_token: Option<&str>,
) -> Result<OAuthCredential, ModelsError> {
    let access = required_string(body, "access_token")?;
    let refresh = match (body.get("refresh_token"), previous_refresh_token) {
        (None, Some(previous)) => previous.to_owned(),
        _ => required_string(body, "refresh_token")?,
    };
    let expires_in_seconds = match body.get("expires_in") {
        None => DEFAULT_TOKEN_LIFETIME_SECONDS,
        Some(_) => positive_number(body, "expires_in")?,
    };
    Ok(OAuthCredential {
        refresh,
        access,
        expires: now_ms() + (expires_in_seconds * 1000.0) as i64 - REFRESH_SKEW_MS,
        extra: Map::new(),
    })
}

/// `xaiOAuth` — the xAI (Grok/X subscription) OAuth provider auth.
pub fn xai_oauth() -> Arc<dyn OAuthAuth> {
    Arc::new(XaiOAuth::new())
}

/// xAI OAuth (`OAuthAuth`) implementation.
pub struct XaiOAuth {
    client: reqwest::Client,
    /// Loopback authority for the URL-rewriting test seam (see module docs).
    authority: Option<String>,
}

impl Default for XaiOAuth {
    fn default() -> Self {
        Self::new()
    }
}

impl XaiOAuth {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
            authority: None,
        }
    }

    /// Test seam — see module docs.
    pub fn with_authority(mut self, authority: impl Into<String>) -> Self {
        self.authority = Some(authority.into());
        self
    }

    /// `requestDeviceCode` — POST `XAI_DEVICE_CODE_URL` (`client_id` +
    /// scope + `referrer: "pi"`).
    async fn request_device_code(
        &self,
        signal: Option<&CancellationToken>,
    ) -> Result<XaiDeviceCode, ModelsError> {
        let response = post_form(
            &self.client,
            self.authority.as_deref(),
            XAI_DEVICE_CODE_URL,
            &[
                ("client_id", XAI_CLIENT_ID),
                ("scope", XAI_SCOPE),
                ("referrer", "pi"),
            ],
            signal,
        )
        .await?;
        if !response.ok {
            return Err(request_failure("device authorization", &response));
        }
        parse_device_code(&response.body)
    }

    /// One token poll (`poll` closure of `pollForTokens`): POST
    /// `XAI_TOKEN_URL`; map the RFC 8628 error fields onto the poll result.
    async fn poll_token(
        &self,
        device: &XaiDeviceCode,
        signal: Option<CancellationToken>,
    ) -> DeviceCodePollResult<OAuthCredential> {
        let response = match post_form(
            &self.client,
            self.authority.as_deref(),
            XAI_TOKEN_URL,
            &[
                ("grant_type", DEVICE_CODE_GRANT_TYPE),
                ("client_id", XAI_CLIENT_ID),
                ("device_code", device.device_code.as_str()),
            ],
            signal.as_ref(),
        )
        .await
        {
            // Upstream lets the fetch error propagate out of the poll
            // closure; the framework surface here is `Failed`.
            Err(request_error) => {
                return DeviceCodePollResult::Failed {
                    message: request_error.message,
                };
            }
            Ok(response) => response,
        };

        if response.ok {
            return match credentials_from_token_response(&response.body, None) {
                Ok(credential) => DeviceCodePollResult::Complete { value: credential },
                Err(parse_error) => DeviceCodePollResult::Failed {
                    message: parse_error.message,
                },
            };
        }

        let error_code = response.body.get("error").and_then(Value::as_str);
        match error_code {
            Some("authorization_pending") => DeviceCodePollResult::Pending,
            Some("slow_down") => DeviceCodePollResult::SlowDown {
                interval_seconds: response.body.get("interval").and_then(Value::as_f64),
            },
            Some("access_denied") | Some("authorization_denied") => DeviceCodePollResult::Failed {
                message: "xAI device authorization was denied".to_owned(),
            },
            Some("expired_token") => DeviceCodePollResult::Failed {
                message: "xAI device code expired".to_owned(),
            },
            _ => DeviceCodePollResult::Failed {
                message: request_failure("device token polling", &response).message,
            },
        }
    }

    /// `pollForTokens` — `waitBeforeFirstPoll: true`.
    async fn poll_for_tokens(
        &self,
        device: &XaiDeviceCode,
        signal: Option<CancellationToken>,
    ) -> Result<OAuthCredential, ModelsError> {
        let poll_signal = signal.clone();
        poll_oauth_device_code_flow(DeviceCodePollOptions {
            interval_seconds: device.interval_seconds,
            expires_in_seconds: Some(device.expires_in_seconds),
            wait_before_first_poll: true,
            signal,
            poll: move || self.poll_token(device, poll_signal.clone()),
        })
        .await
    }

    /// `loginXai`.
    async fn login_xai(
        &self,
        interaction: &dyn AuthInteraction,
    ) -> Result<OAuthCredential, ModelsError> {
        let device = self
            .request_device_code(interaction.signal().as_ref())
            .await?;
        interaction.notify(AuthEvent::DeviceCode {
            user_code: device.user_code.clone(),
            verification_uri: device
                .verification_uri_complete
                .clone()
                .unwrap_or_else(|| device.verification_uri.clone()),
            interval_seconds: device.interval_seconds.map(|interval| interval as u64),
            expires_in_seconds: Some(device.expires_in_seconds as u64),
        });
        self.poll_for_tokens(&device, interaction.signal()).await
    }

    /// `refreshXaiToken`.
    async fn refresh_xai_token(
        &self,
        refresh_token: &str,
        signal: Option<&CancellationToken>,
    ) -> Result<OAuthCredential, ModelsError> {
        let response = post_form(
            &self.client,
            self.authority.as_deref(),
            XAI_TOKEN_URL,
            &[
                ("grant_type", "refresh_token"),
                ("client_id", XAI_CLIENT_ID),
                ("refresh_token", refresh_token),
            ],
            signal,
        )
        .await?;
        if !response.ok {
            return Err(request_failure("token refresh", &response));
        }
        credentials_from_token_response(&response.body, Some(refresh_token))
    }
}

#[async_trait::async_trait]
impl OAuthAuth for XaiOAuth {
    fn name(&self) -> &str {
        "xAI (Grok/X subscription)"
    }

    /// `isSubscription: true` (providers/xai.ts:17 @ 4181f66).
    fn is_subscription(&self) -> bool {
        true
    }

    /// `login` — device code flow (RFC 8628), no prompt.
    async fn login(
        &self,
        interaction: &dyn AuthInteraction,
        _options: Option<&LoginOptions>,
    ) -> Result<OAuthCredential, ModelsError> {
        self.login_xai(interaction).await
    }

    /// `refresh: (credential, signal) => refreshXaiToken(credential.refresh,
    /// signal)`.
    async fn refresh(
        &self,
        credential: &OAuthCredential,
        signal: Option<&CancellationToken>,
    ) -> Result<OAuthCredential, ModelsError> {
        self.refresh_xai_token(&credential.refresh, signal).await
    }

    /// `toAuth: { apiKey: credential.access }`.
    async fn to_auth(&self, credential: &OAuthCredential) -> Result<ModelAuth, ModelsError> {
        Ok(ModelAuth {
            api_key: Some(credential.access.clone()),
            ..ModelAuth::default()
        })
    }
}

#[cfg(test)]
mod tests {
    //! Test intents ported from `packages/ai/test/xai-oauth.test.ts` @ pi
    //! 0.82.1 (2efa728); the mocked `fetch` becomes a loopback axum server
    //! behind the `authority` rewrite seam (module docs), and the
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
        authority: String,
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
                authority: addr.to_string(),
                requests,
                shutdown: Some(tx),
            }
        }

        fn oauth(&self) -> XaiOAuth {
            XaiOAuth::new().with_authority(self.authority.clone())
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

    // ----- fake AuthInteraction (upstream `loginXaiForTest`) -----

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
        signal: Option<CancellationToken>,
        on_device_code: Option<Arc<dyn Fn() + Send + Sync>>,
    }

    impl FakeInteraction {
        fn new(handle: InteractionHandle) -> Self {
            Self {
                handle,
                signal: None,
                on_device_code: None,
            }
        }

        fn with_signal(mut self, signal: CancellationToken) -> Self {
            self.signal = Some(signal);
            self
        }

        fn on_device_code(mut self, callback: impl Fn() + Send + Sync + 'static) -> Self {
            self.on_device_code = Some(Arc::new(callback));
            self
        }
    }

    impl AuthInteraction for FakeInteraction {
        fn signal(&self) -> Option<CancellationToken> {
            self.signal.clone()
        }

        fn prompt<'a>(
            &'a self,
            prompt: AuthPrompt,
        ) -> BoxFutureSend<'a, Result<String, ModelsError>> {
            // Upstream: `prompt: () => { throw new Error("Unexpected prompt") }`.
            Box::pin(async move {
                Err(ModelsError::new(
                    ModelsErrorCode::Auth,
                    format!("Unexpected prompt: {prompt:?}"),
                ))
            })
        }

        fn notify(&self, event: AuthEvent) {
            self.handle.events.lock().expect("lock").push(event.clone());
            if matches!(event, AuthEvent::DeviceCode { .. })
                && let Some(callback) = &self.on_device_code
            {
                callback();
            }
        }
    }

    // ----- canned upstream response bodies -----

    fn device_code_response(overrides: &[(&str, Value)]) -> Value {
        let mut body = json!({
            "device_code": "device-code",
            "user_code": "ABCD-1234",
            "verification_uri": "https://accounts.x.ai/oauth2/device",
            "expires_in": 900,
            "interval": 1,
        });
        for (key, value) in overrides {
            body[key] = value.clone();
        }
        body
    }

    fn token_response(overrides: &[(&str, Value)]) -> Value {
        let mut body = json!({
            "access_token": "access-token",
            "refresh_token": "refresh-token",
            "expires_in": 21_600,
            "token_type": "Bearer",
        });
        for (key, value) in overrides {
            body[key] = value.clone();
        }
        body
    }

    /// Happy-path responder: device code → pending → slow_down → token.
    fn happy_path_responder() -> Responder {
        let polls = Arc::new(Mutex::new(0usize));
        Arc::new(move |request: &RecordedRequest| {
            let path = request.path.as_str();
            if path.ends_with("/oauth2/device/code") {
                assert_eq!(request.method, "POST");
                assert_eq!(
                    request.form_get("client_id").as_deref(),
                    Some(XAI_CLIENT_ID)
                );
                assert_eq!(
                    request.form_get("scope").as_deref(),
                    Some("openid profile email offline_access grok-cli:access api:access")
                );
                assert_eq!(request.form_get("referrer").as_deref(), Some("pi"));
                return (StatusCode::OK, device_code_response(&[]));
            }
            if path.ends_with("/oauth2/token") {
                assert_eq!(
                    request.form_get("grant_type").as_deref(),
                    Some("urn:ietf:params:oauth:grant-type:device_code")
                );
                assert_eq!(
                    request.form_get("client_id").as_deref(),
                    Some(XAI_CLIENT_ID)
                );
                assert_eq!(
                    request.form_get("device_code").as_deref(),
                    Some("device-code")
                );
                let mut count = polls.lock().expect("lock");
                *count += 1;
                return match *count {
                    1 => (
                        StatusCode::BAD_REQUEST,
                        json!({ "error": "authorization_pending" }),
                    ),
                    2 => (
                        StatusCode::BAD_REQUEST,
                        json!({ "error": "slow_down", "interval": 1 }),
                    ),
                    _ => (StatusCode::OK, token_response(&[])),
                };
            }
            panic!("unexpected request: {} {}", request.method, request.path);
        })
    }

    // ----- pure-function tests -----

    /// `parseDeviceCode` — RFC 8628 `interval` 0 (no minimum wait) falls
    /// back to the poller's default instead of failing.
    #[test]
    fn parse_device_code_falls_back_for_interval_zero() {
        let body = json!({
            "device_code": "device-code",
            "user_code": "ABCD-1234",
            "verification_uri": "https://accounts.x.ai/oauth2/device",
            "expires_in": 900,
            "interval": 0,
        });
        let device = parse_device_code(body.as_object().expect("object")).expect("parse");
        assert_eq!(device.interval_seconds, None);
        assert_eq!(device.expires_in_seconds, 900.0);
        assert_eq!(device.verification_uri_complete, None);
    }

    /// `rejects a non-https verification URI` — parse-level table (upstream
    /// `it.each`): http, file and unparseable URLs all fail with the
    /// verbatim message; https survives.
    #[test]
    fn parse_device_code_rejects_non_https_verification_uri() {
        for raw in [
            "http://accounts.x.ai/oauth2/device",
            "file:///etc/passwd",
            "not a url",
        ] {
            let body = json!({
                "device_code": "device-code",
                "user_code": "ABCD-1234",
                "verification_uri": raw,
                "expires_in": 900,
            });
            let error =
                parse_device_code(body.as_object().expect("object")).expect_err("untrusted");
            assert_eq!(
                error.message,
                "Untrusted verification URI in xAI OAuth response"
            );
        }

        let body = json!({
            "device_code": "device-code",
            "user_code": "ABCD-1234",
            "verification_uri": "https://accounts.x.ai/oauth2/device",
            "expires_in": 900,
        });
        let device = parse_device_code(body.as_object().expect("object")).expect("parse");
        assert_eq!(
            device.verification_uri,
            "https://accounts.x.ai/oauth2/device"
        );
    }

    /// `rejects a non-https verification_uri_complete` — present but
    /// non-https fails; absent or empty is fine.
    #[test]
    fn parse_device_code_requires_https_complete_uri_when_present() {
        let body = json!({
            "device_code": "device-code",
            "user_code": "ABCD-1234",
            "verification_uri": "https://accounts.x.ai/oauth2/device",
            "verification_uri_complete": "http://accounts.x.ai/oauth2/device?user_code=ABCD-1234",
            "expires_in": 900,
        });
        let error = parse_device_code(body.as_object().expect("object")).expect_err("untrusted");
        assert_eq!(
            error.message,
            "Untrusted verification URI in xAI OAuth response"
        );

        // Empty string: treated as absent.
        let body = json!({
            "device_code": "device-code",
            "user_code": "ABCD-1234",
            "verification_uri": "https://accounts.x.ai/oauth2/device",
            "verification_uri_complete": "",
            "expires_in": 900,
        });
        let device = parse_device_code(body.as_object().expect("object")).expect("parse");
        assert_eq!(device.verification_uri_complete, None);
    }

    /// Missing / malformed device-code fields fail with the verbatim
    /// field-specific message.
    #[test]
    fn parse_device_code_rejects_missing_fields() {
        for field in ["device_code", "user_code", "verification_uri", "expires_in"] {
            let mut body = json!({
                "device_code": "device-code",
                "user_code": "ABCD-1234",
                "verification_uri": "https://accounts.x.ai/oauth2/device",
                "expires_in": 900,
            });
            body.as_object_mut().expect("object").remove(field);
            let error = parse_device_code(body.as_object().expect("object")).expect_err("missing");
            assert_eq!(
                error.message,
                format!("Invalid xAI OAuth response field: {field}")
            );
        }
    }

    // ----- flow tests against the mock -----

    /// `uses the device grant, delays polling, and handles pending and
    /// slow_down` — verbatim upstream assertions: device-code event,
    /// request form fields, credential shape with the 5-minute skew.
    #[tokio::test]
    async fn login_happy_path_handles_pending_and_slow_down() {
        let mock = MockAuth::start(happy_path_responder()).await;
        let oauth = mock.oauth();
        let handle = InteractionHandle::default();
        let interaction = FakeInteraction::new(handle.clone());

        let before = now_ms();
        let credential = oauth.login(&interaction, None).await.expect("login");
        assert_eq!(credential.access, "access-token");
        assert_eq!(credential.refresh, "refresh-token");
        // `expires = now + expires_in * 1000 - REFRESH_SKEW_MS`.
        assert!(credential.expires >= before + 21_600 * 1000 - REFRESH_SKEW_MS);
        assert!(credential.expires <= before + 21_600 * 1000 - REFRESH_SKEW_MS + 5000);

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
                // No verification_uri_complete in the response: the plain
                // verification URI is notified.
                assert_eq!(verification_uri, "https://accounts.x.ai/oauth2/device");
                assert_eq!(interval_seconds, &Some(1));
                assert_eq!(expires_in_seconds, &Some(900));
            }
            other => panic!("unexpected events: {other:?}"),
        }

        let requests = mock.requests();
        assert_eq!(
            requests.len(),
            4,
            "device code + pending + slow_down + token"
        );
        assert_eq!(requests[0].path, "/auth.x.ai/oauth2/device/code");
        for request in &requests[1..] {
            assert_eq!(request.path, "/auth.x.ai/oauth2/token");
        }
    }

    /// `prefers verification_uri_complete when the server provides it`.
    #[tokio::test]
    async fn login_prefers_verification_uri_complete() {
        let responder: Responder = Arc::new(|request: &RecordedRequest| {
            let path = request.path.as_str();
            if path.ends_with("/oauth2/device/code") {
                return (
                    StatusCode::OK,
                    device_code_response(&[(
                        "verification_uri_complete",
                        json!("https://accounts.x.ai/oauth2/device?user_code=ABCD-1234"),
                    )]),
                );
            }
            if path.ends_with("/oauth2/token") {
                return (StatusCode::OK, token_response(&[]));
            }
            panic!("unexpected request: {} {}", request.method, request.path);
        });
        let mock = MockAuth::start(responder).await;
        let oauth = mock.oauth();
        let handle = InteractionHandle::default();
        let interaction = FakeInteraction::new(handle.clone());

        oauth.login(&interaction, None).await.expect("login");
        match handle.events().as_slice() {
            [
                AuthEvent::DeviceCode {
                    verification_uri, ..
                },
            ] => assert_eq!(
                verification_uri,
                "https://accounts.x.ai/oauth2/device?user_code=ABCD-1234"
            ),
            other => panic!("unexpected events: {other:?}"),
        }
    }

    /// `fails when device authorization is denied` — both spellings.
    #[tokio::test]
    async fn denied_errors_map_to_verbatim_message() {
        let login_attempts = Arc::new(Mutex::new(0usize));
        let responder: Responder = Arc::new(move |request: &RecordedRequest| {
            let path = request.path.as_str();
            if path.ends_with("/oauth2/device/code") {
                return (StatusCode::OK, device_code_response(&[]));
            }
            if path.ends_with("/oauth2/token") {
                let mut attempts = login_attempts.lock().expect("lock");
                *attempts += 1;
                let error = if *attempts == 1 {
                    "access_denied"
                } else {
                    "authorization_denied"
                };
                return (StatusCode::BAD_REQUEST, json!({ "error": error }));
            }
            panic!("unexpected request: {} {}", request.method, request.path);
        });
        let mock = MockAuth::start(responder).await;
        let oauth = mock.oauth();

        for _ in 0..2 {
            let handle = InteractionHandle::default();
            let interaction = FakeInteraction::new(handle);
            let error = oauth.login(&interaction, None).await.expect_err("denied");
            assert_eq!(error.message, "xAI device authorization was denied");
        }
    }

    /// `expired_token` maps to the verbatim message.
    #[tokio::test]
    async fn expired_token_maps_to_verbatim_message() {
        let responder: Responder = Arc::new(|request: &RecordedRequest| {
            let path = request.path.as_str();
            if path.ends_with("/oauth2/device/code") {
                return (StatusCode::OK, device_code_response(&[]));
            }
            if path.ends_with("/oauth2/token") {
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
        assert_eq!(error.message, "xAI device code expired");
    }

    /// Unknown polling errors surface as the request-failure message with
    /// the error code + description.
    #[tokio::test]
    async fn unknown_poll_error_surfaces_request_failure() {
        let responder: Responder = Arc::new(|request: &RecordedRequest| {
            let path = request.path.as_str();
            if path.ends_with("/oauth2/device/code") {
                return (StatusCode::OK, device_code_response(&[]));
            }
            if path.ends_with("/oauth2/token") {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    json!({ "error": "server_error", "error_description": "boom" }),
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
            .expect_err("server_error");
        assert_eq!(
            error.message,
            "xAI OAuth device token polling failed (HTTP 500): server_error: boom"
        );
    }

    /// `cancels while waiting for the first token poll` — aborting after
    /// the device-code event fails with `Login cancelled`; only the device
    /// code request went out.
    #[tokio::test]
    async fn cancels_while_waiting_for_the_first_poll() {
        let responder: Responder = Arc::new(|request: &RecordedRequest| {
            assert!(request.path.ends_with("/oauth2/device/code"));
            (StatusCode::OK, device_code_response(&[]))
        });
        let mock = MockAuth::start(responder).await;
        let oauth = mock.oauth();
        let handle = InteractionHandle::default();
        let token = CancellationToken::new();
        let interaction = FakeInteraction::new(handle)
            .with_signal(token.clone())
            .on_device_code(move || token.cancel());

        let error = oauth
            .login(&interaction, None)
            .await
            .expect_err("cancelled");
        assert_eq!(error.message, "Login cancelled");
        assert_eq!(mock.requests().len(), 1, "only the device code request");
    }

    /// `refreshes tokens and preserves an unrotated refresh token` — the
    /// refresh grant sends the stored token and keeps it when the server
    /// omits `refresh_token`.
    #[tokio::test]
    async fn refresh_rotates_and_preserves_unrotated_refresh_token() {
        let request_count = Arc::new(Mutex::new(0usize));
        let responder_count = request_count.clone();
        let responder: Responder = Arc::new(move |request: &RecordedRequest| {
            assert!(request.path.ends_with("/oauth2/token"));
            assert_eq!(
                request.form_get("grant_type").as_deref(),
                Some("refresh_token")
            );
            assert_eq!(
                request.form_get("client_id").as_deref(),
                Some(XAI_CLIENT_ID)
            );
            let mut count = responder_count.lock().expect("lock");
            *count += 1;
            if *count == 1 {
                assert_eq!(
                    request.form_get("refresh_token").as_deref(),
                    Some("old-refresh")
                );
                return (
                    StatusCode::OK,
                    token_response(&[
                        ("access_token", json!("new-access")),
                        ("refresh_token", json!("new-refresh")),
                    ]),
                );
            }
            assert_eq!(
                request.form_get("refresh_token").as_deref(),
                Some("keep-refresh")
            );
            // Upstream `refresh_token: undefined` — JSON drops the key, so
            // the previous refresh token is preserved.
            let mut body = token_response(&[("access_token", json!("newer-access"))]);
            body.as_object_mut()
                .expect("object")
                .remove("refresh_token");
            (StatusCode::OK, body)
        });
        let mock = MockAuth::start(responder).await;
        let oauth = mock.oauth();
        let credential = |refresh: &str| OAuthCredential {
            refresh: refresh.to_owned(),
            access: "old-access".to_owned(),
            expires: 0,
            extra: Map::new(),
        };

        let rotated = oauth
            .refresh(&credential("old-refresh"), None)
            .await
            .expect("rotated");
        assert_eq!(rotated.access, "new-access");
        assert_eq!(rotated.refresh, "new-refresh");

        let preserved = oauth
            .refresh(&credential("keep-refresh"), None)
            .await
            .expect("preserved");
        assert_eq!(preserved.access, "newer-access");
        assert_eq!(preserved.refresh, "keep-refresh");
        assert_eq!(*request_count.lock().expect("lock"), 2);
    }

    /// `assumes a one-hour lifetime when expires_in is missing` (upstream
    /// `expires_in: undefined` — JSON drops the key).
    #[tokio::test]
    async fn refresh_assumes_one_hour_lifetime_when_expires_in_missing() {
        let responder: Responder = Arc::new(|request: &RecordedRequest| {
            assert!(request.path.ends_with("/oauth2/token"));
            let mut body = token_response(&[]);
            body.as_object_mut().expect("object").remove("expires_in");
            (StatusCode::OK, body)
        });
        let mock = MockAuth::start(responder).await;
        let oauth = mock.oauth();
        let credential = OAuthCredential {
            refresh: "old-refresh".to_owned(),
            access: "old".to_owned(),
            expires: 0,
            extra: Map::new(),
        };

        let before = now_ms();
        let refreshed = oauth.refresh(&credential, None).await.expect("refresh");
        assert!(refreshed.expires >= before + 3_600_000 - REFRESH_SKEW_MS);
        assert!(refreshed.expires <= before + 3_600_000 - REFRESH_SKEW_MS + 5000);
    }

    /// `rejects token responses with missing fields` — verbatim message.
    #[tokio::test]
    async fn refresh_rejects_missing_access_token() {
        let responder: Responder = Arc::new(|request: &RecordedRequest| {
            assert!(request.path.ends_with("/oauth2/token"));
            (
                StatusCode::OK,
                token_response(&[("access_token", Value::Null)]),
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

        let error = oauth.refresh(&credential, None).await.expect_err("missing");
        assert_eq!(
            error.message,
            "Invalid xAI OAuth response field: access_token"
        );
    }

    /// `surfaces the upstream error code and description on refresh failure`.
    #[tokio::test]
    async fn refresh_failure_surfaces_error_code_and_description() {
        let responder: Responder = Arc::new(|request: &RecordedRequest| {
            assert!(request.path.ends_with("/oauth2/token"));
            (
                StatusCode::BAD_REQUEST,
                json!({ "error": "invalid_grant", "error_description": "refresh token revoked" }),
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
            "xAI OAuth token refresh failed (HTTP 400): invalid_grant: refresh token revoked"
        );
    }

    /// `toAuth` maps the access token to an api key.
    #[tokio::test]
    async fn to_auth_maps_access_to_api_key() {
        let oauth = XaiOAuth::new();
        assert_eq!(oauth.name(), "xAI (Grok/X subscription)");
        let credential = OAuthCredential {
            refresh: "r".to_owned(),
            access: "access-token".to_owned(),
            expires: i64::MAX,
            extra: Map::new(),
        };
        let auth = oauth.to_auth(&credential).await.expect("to_auth");
        assert_eq!(auth.api_key.as_deref(), Some("access-token"));
        assert_eq!(auth.headers, None);
    }
}
