//! Port of `packages/ai/src/auth/oauth/radius.ts` @ pi 0.82.1 (2efa728) —
//! Radius gateway OAuth: OAuth client APIs live on the configured gateway
//! (`{gateway}/v1/oauth*`), only the interactive browser authorization
//! endpoint is discovered. Two login methods: browser (PKCE + localhost
//! callback on 127.0.0.1:1456) and device code (RFC 8628). Model catalog
//! loading stays with the Radius provider (`providers/radius_config.rs`).
//!
//! Test seams (upstream stubs the global `fetch` and binds the fixed
//! callback port; here the seams are constructor fields — same precedent as
//! `anthropic.rs`):
//! - the gateway is already a constructor option (`createRadiusOAuth`), so
//!   tests simply point it at a loopback mock;
//! - `callback_port`: free ports let browser-flow tests avoid colliding on
//!   the fixed `CALLBACK_PORT` (upstream tests bind the real port
//!   sequentially). `REDIRECT_URI` stays the upstream constant — it is
//!   advertised to the authorization server, while tests drive the callback
//!   at the actual bound port directly.
//!
//! Intentional differences:
//! - the upstream `node:http` callback server becomes `axum` (coding-standards
//!   appendix A), reusing `oauth_page` HTML from `super::callback_page`;
//!   handler branch order and page copy are verbatim;
//! - `crypto.randomUUID()` becomes a `ring`-generated UUIDv4 (same 36-char
//!   shape; only uniqueness matters — it is the OAuth `state`);
//! - the poll closure's non-OAuth errors surface via
//!   `DeviceCodePollResult::Failed` with the same message text (upstream
//!   rethrows them through the polling framework);
//! - `expires_in`/`interval` parse as `f64` (JS `number`) and narrow into
//!   the `u64` event fields; the device-authorization "missing fields" check
//!   keeps JS falsy semantics (empty strings and `expires_in: 0` count as
//!   missing).

use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

use super::super::interaction::{AuthEvent, AuthInteraction, AuthPrompt, SelectOption};
use super::super::resolve::{ModelsError, ModelsErrorCode};
use super::super::types::{LoginOptions, ModelAuth, OAuthAuth, OAuthCredential};
use super::callback_server::{CompleteFn, OAuthCallbackServer, OAuthCallbackServerOptions};
use super::device_code::{
    DeviceCodePollOptions, DeviceCodePollResult, poll_oauth_device_code_flow,
};
use super::pkce::generate_pkce;
use crate::providers::radius_config::normalize_radius_gateway_url;

/// `CALLBACK_HOST`.
const CALLBACK_HOST: &str = "127.0.0.1";
/// `CALLBACK_PORT`.
const CALLBACK_PORT: u16 = 1456;
/// `CALLBACK_PATH`.
const CALLBACK_PATH: &str = "/oauth/callback";
/// `REDIRECT_URI` = `http://{CALLBACK_HOST}:{CALLBACK_PORT}{CALLBACK_PATH}`.
const REDIRECT_URI: &str = "http://127.0.0.1:1456/oauth/callback";
/// `TOKEN_EXPIRY_SKEW_MS`.
const TOKEN_EXPIRY_SKEW_MS: i64 = 60_000;
/// `LOGIN_METHOD_BROWSER`.
const LOGIN_METHOD_BROWSER: &str = "browser";
/// `LOGIN_METHOD_DEVICE_CODE`.
const LOGIN_METHOD_DEVICE_CODE: &str = "device-code";
/// `OAUTH_CLIENT_ID`.
const OAUTH_CLIENT_ID: &str = "pi-gateway";
/// `OAUTH_SCOPE`.
const OAUTH_SCOPE: &str = "gateway offline_access";
/// `OAUTH_DEVICE_CODE_GRANT_TYPE`.
const DEVICE_CODE_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:device_code";

/// `requestOAuthToken` body shared by the `OAuthAuth` methods and the
/// callback server's `complete` hook.
async fn request_oauth_token_with(
    client: &reqwest::Client,
    gateway: &str,
    form: &[(&str, &str)],
    signal: Option<&CancellationToken>,
) -> Result<OAuthCredential, TokenRequestError> {
    let url = gateway_url(gateway, "/v1/oauth/token").map_err(TokenRequestError::Other)?;
    let send = client
        .post(url)
        .header(reqwest::header::ACCEPT, "application/json")
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .form(form)
        .send();
    let response = match signal {
        Some(token) => {
            tokio::select! {
                () = token.cancelled() => {
                    return Err(TokenRequestError::Other(error(
                        super::device_code::CANCEL_MESSAGE,
                    )));
                }
                response = send => response,
            }
        }
        None => send.await,
    };
    let response = response
        .map_err(|request_error| TokenRequestError::Other(error(request_error.to_string())))?;

    if !response.status().is_success() {
        return Err(TokenRequestError::OAuth(
            read_oauth_response_error(response, "Radius OAuth token request failed").await,
        ));
    }

    let data: TokenResponse = response
        .json()
        .await
        .map_err(|json_error| TokenRequestError::Other(error(json_error.to_string())))?;

    let mut extra = Map::new();
    if let Some(scope) = data.scope {
        extra.insert("scope".to_owned(), Value::String(scope));
    }
    Ok(OAuthCredential {
        refresh: data.refresh_token,
        access: data.access_token,
        expires: now_ms() + (data.expires_in * 1000.0) as i64 - TOKEN_EXPIRY_SKEW_MS,
        extra,
    })
}

fn error(message: impl Into<String>) -> ModelsError {
    ModelsError::new(ModelsErrorCode::Oauth, message.into())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

/// `new URL(path, gateway)` — absolute-path join (drops any path component
/// of the gateway, like the WHATWG URL constructor).
fn gateway_url(gateway: &str, path: &str) -> Result<String, ModelsError> {
    let base = url::Url::parse(gateway).map_err(|parse_error| {
        error(format!(
            "Invalid Radius gateway URL {gateway}: {parse_error}"
        ))
    })?;
    base.join(path)
        .map(|url| url.to_string())
        .map_err(|join_error| {
            error(format!(
                "Invalid Radius gateway URL {gateway}: {join_error}"
            ))
        })
}

/// `crypto.randomUUID()` — UUIDv4 from the system RNG.
fn random_uuid() -> Result<String, ModelsError> {
    use ring::rand::SecureRandom;
    let rng = ring::rand::SystemRandom::new();
    let mut bytes = [0u8; 16];
    rng.fill(&mut bytes)
        .map_err(|_| error("Failed to generate random OAuth state"))?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    ))
}

/// `DeviceAuthorizationResponse`.
#[derive(Debug, Clone)]
struct DeviceAuthorizationResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_in: f64,
    interval: Option<f64>,
}

/// `OAuthResponseError` — non-2xx OAuth endpoint response carrying the
/// parsed RFC 6749 error fields.
#[derive(Debug)]
struct OAuthResponseError {
    #[allow(dead_code)] // `status` rides along for parity with upstream.
    status: u16,
    oauth_error: Option<String>,
    message: String,
}

/// `readOAuthResponseError` — detail = `error[: description]` when an OAuth
/// error code is present, else the description/body text, else the status.
async fn read_oauth_response_error(
    response: reqwest::Response,
    message: &str,
) -> OAuthResponseError {
    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    let mut oauth_error = None;
    let mut description = None;
    if !text.is_empty() {
        match serde_json::from_str::<Value>(&text) {
            Ok(data) => {
                oauth_error = data.get("error").and_then(Value::as_str).map(str::to_owned);
                description = data
                    .get("error_description")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
            }
            Err(_) => description = Some(text),
        }
    }
    let detail = match &oauth_error {
        Some(oauth_error) => match &description {
            Some(description) => format!("{oauth_error}: {description}"),
            None => oauth_error.clone(),
        },
        None => description.unwrap_or_else(|| status.to_string()),
    };
    OAuthResponseError {
        status,
        oauth_error,
        message: format!("{message}: {detail}"),
    }
}

/// Token endpoint failure: OAuth-shaped errors keep their parsed code for
/// the device-poll mapping (`error instanceof OAuthResponseError` upstream);
/// everything else propagates plainly.
#[derive(Debug)]
enum TokenRequestError {
    OAuth(OAuthResponseError),
    Other(ModelsError),
}

impl TokenRequestError {
    fn into_models_error(self) -> ModelsError {
        match self {
            TokenRequestError::OAuth(oauth_error) => error(oauth_error.message),
            TokenRequestError::Other(models_error) => models_error,
        }
    }
}

/// Token endpoint JSON shape (`{ access_token, refresh_token, expires_in,
/// scope? }`; upstream casts unchecked — strict serde is the
/// faithful-in-spirit choice, same precedent as `anthropic.rs`).
#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    expires_in: f64,
    scope: Option<String>,
}

/// `RadiusOAuthOptions`.
pub struct RadiusOAuthOptions {
    pub name: String,
    pub gateway: String,
}

/// `createRadiusOAuth(options)` — the gateway is normalized at construction.
pub fn create_radius_oauth(options: RadiusOAuthOptions) -> Arc<dyn OAuthAuth> {
    Arc::new(RadiusOAuth::new(options.name, options.gateway))
}

/// Radius gateway OAuth (`OAuthAuth`) implementation.
pub struct RadiusOAuth {
    name: String,
    gateway: String,
    client: reqwest::Client,
    /// `CALLBACK_PORT` (test seam — see module docs).
    callback_port: u16,
}

impl RadiusOAuth {
    /// `createRadiusOAuth({ name, gateway })`.
    pub fn new(name: impl Into<String>, gateway: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            gateway: normalize_radius_gateway_url(&gateway.into()),
            client: reqwest::Client::new(),
            callback_port: CALLBACK_PORT,
        }
    }

    /// Test seam — see module docs.
    #[cfg(test)]
    fn with_callback_port(mut self, callback_port: u16) -> Self {
        self.callback_port = callback_port;
        self
    }

    /// Normalized gateway this flow targets (`normalizeRadiusGatewayUrl`).
    pub fn gateway(&self) -> &str {
        &self.gateway
    }

    /// `requestOAuthToken` — POST `{gateway}/v1/oauth/token` (form-encoded);
    /// `expires = Date.now() + expires_in * 1000 - TOKEN_EXPIRY_SKEW_MS`;
    /// the optional `scope` rides the credential extras.
    async fn request_oauth_token(
        &self,
        form: &[(&str, &str)],
        signal: Option<&CancellationToken>,
    ) -> Result<OAuthCredential, TokenRequestError> {
        request_oauth_token_with(&self.client, &self.gateway, form, signal).await
    }

    /// `loadRadiusOAuthDiscovery` — GET `{gateway}/v1/oauth`; only the
    /// interactive browser authorization endpoint is discovered.
    async fn load_radius_oauth_discovery(&self) -> Result<String, ModelsError> {
        let url = gateway_url(&self.gateway, "/v1/oauth")?;
        let response = self
            .client
            .get(url)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|request_error| error(request_error.to_string()))?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(error(format!(
                "Could not load Radius OAuth config from {}: {status} {text}",
                self.gateway
            )));
        }
        let discovery: Value = response
            .json()
            .await
            .map_err(|json_error| error(json_error.to_string()))?;
        discovery
            .get("authorizationEndpoint")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| error(format!("Invalid Radius OAuth config from {}", self.gateway)))
    }

    /// `requestDeviceAuthorization` — POST `{gateway}/v1/oauth/device`
    /// (`client_id` + scope).
    async fn request_device_authorization(
        &self,
        signal: Option<&CancellationToken>,
    ) -> Result<DeviceAuthorizationResponse, ModelsError> {
        let url = gateway_url(&self.gateway, "/v1/oauth/device")?;
        let send = self
            .client
            .post(url)
            .header(reqwest::header::ACCEPT, "application/json")
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .form(&[("client_id", OAUTH_CLIENT_ID), ("scope", OAUTH_SCOPE)])
            .send();
        let response = match signal {
            Some(token) => {
                tokio::select! {
                    () = token.cancelled() => {
                        return Err(error(super::device_code::CANCEL_MESSAGE));
                    }
                    response = send => response,
                }
            }
            None => send.await,
        };
        let response = response.map_err(|request_error| error(request_error.to_string()))?;

        if !response.status().is_success() {
            return Err(error(
                read_oauth_response_error(response, "Radius OAuth device authorization failed")
                    .await
                    .message,
            ));
        }

        let data: Value = response
            .json()
            .await
            .map_err(|json_error| error(json_error.to_string()))?;
        let device_code = data.get("device_code").and_then(Value::as_str);
        let user_code = data.get("user_code").and_then(Value::as_str);
        let verification_uri = data.get("verification_uri").and_then(Value::as_str);
        let expires_in = data.get("expires_in").and_then(Value::as_f64);
        let interval = data.get("interval").and_then(Value::as_f64);
        // JS falsy semantics: empty strings and `expires_in: 0` are missing.
        match (device_code, user_code, verification_uri, expires_in) {
            (Some(device_code), Some(user_code), Some(verification_uri), Some(expires_in))
                if !device_code.is_empty()
                    && !user_code.is_empty()
                    && !verification_uri.is_empty()
                    && expires_in != 0.0 =>
            {
                Ok(DeviceAuthorizationResponse {
                    device_code: device_code.to_owned(),
                    user_code: user_code.to_owned(),
                    verification_uri: verification_uri.to_owned(),
                    expires_in,
                    interval,
                })
            }
            _ => Err(error(
                "Radius OAuth device authorization response is missing required fields",
            )),
        }
    }

    /// `loginWithDeviceCode`.
    async fn login_with_device_code(
        &self,
        interaction: &dyn AuthInteraction,
    ) -> Result<OAuthCredential, ModelsError> {
        let device = self
            .request_device_authorization(interaction.signal().as_ref())
            .await?;
        interaction.notify(AuthEvent::DeviceCode {
            user_code: device.user_code.clone(),
            verification_uri: device.verification_uri.clone(),
            interval_seconds: device.interval.map(|interval| interval as u64),
            expires_in_seconds: Some(device.expires_in as u64),
        });

        poll_oauth_device_code_flow(DeviceCodePollOptions {
            interval_seconds: device.interval,
            expires_in_seconds: Some(device.expires_in),
            wait_before_first_poll: false,
            signal: interaction.signal(),
            poll: || async {
                let form = [
                    ("grant_type", DEVICE_CODE_GRANT_TYPE),
                    ("client_id", OAUTH_CLIENT_ID),
                    ("device_code", device.device_code.as_str()),
                ];
                match self
                    .request_oauth_token(&form, interaction.signal().as_ref())
                    .await
                {
                    Ok(credential) => DeviceCodePollResult::Complete { value: credential },
                    Err(TokenRequestError::Other(models_error)) => DeviceCodePollResult::Failed {
                        message: models_error.message,
                    },
                    Err(TokenRequestError::OAuth(oauth_error)) => {
                        match oauth_error.oauth_error.as_deref() {
                            Some("authorization_pending") => DeviceCodePollResult::Pending,
                            Some("slow_down") => DeviceCodePollResult::SlowDown {
                                interval_seconds: None,
                            },
                            Some("expired_token") => DeviceCodePollResult::Failed {
                                message: "Device authorization expired.".to_owned(),
                            },
                            Some("access_denied") => DeviceCodePollResult::Failed {
                                message: "Device authorization was denied.".to_owned(),
                            },
                            // Upstream rethrows unrecognized OAuth errors.
                            _ => DeviceCodePollResult::Failed {
                                message: oauth_error.message,
                            },
                        }
                    }
                }
            },
        })
        .await
    }

    /// `loginWithBrowser` — PKCE + state, shared localhost callback server
    /// (`complete` exchanges the code before the page is sent), then the
    /// credential comes back through `wait`.
    async fn login_with_browser(
        &self,
        authorization_endpoint: &str,
        interaction: &dyn AuthInteraction,
    ) -> Result<OAuthCredential, ModelsError> {
        let pkce = generate_pkce();
        let state = random_uuid()?;
        let mut authorize_url = url::Url::parse(authorization_endpoint).map_err(|parse_error| {
            error(format!(
                "Invalid authorization endpoint {authorization_endpoint}: {parse_error}"
            ))
        })?;
        // `authorizeUrl.search = new URLSearchParams({...}).toString()`.
        let query = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs([
                ("response_type", "code"),
                ("client_id", OAUTH_CLIENT_ID),
                ("redirect_uri", REDIRECT_URI),
                ("scope", OAUTH_SCOPE),
                ("code_challenge", pkce.challenge.as_str()),
                ("code_challenge_method", "S256"),
                ("handoff", "url"),
                ("state", state.as_str()),
            ])
            .finish();
        authorize_url.set_query(Some(&query));

        // `startOAuthCallbackServer({...})` (no `.catch`): a bind failure
        // fails the login; Radius offers device-code as the alternative.
        let complete: CompleteFn<OAuthCredential> = {
            let client = self.client.clone();
            let gateway = self.gateway.clone();
            let verifier = pkce.verifier.clone();
            let signal = interaction.signal();
            Arc::new(move |code: String| {
                let client = client.clone();
                let gateway = gateway.clone();
                let verifier = verifier.clone();
                let signal = signal.clone();
                Box::pin(async move {
                    let form = [
                        ("grant_type", "authorization_code"),
                        ("client_id", OAUTH_CLIENT_ID),
                        ("redirect_uri", REDIRECT_URI),
                        ("code", code.as_str()),
                        ("code_verifier", verifier.as_str()),
                    ];
                    request_oauth_token_with(&client, &gateway, &form, signal.as_ref())
                        .await
                        .map_err(TokenRequestError::into_models_error)
                })
            })
        };
        let server = OAuthCallbackServer::start(OAuthCallbackServerOptions {
            provider_name: "Radius".to_owned(),
            host: CALLBACK_HOST.to_owned(),
            port: self.callback_port,
            path: CALLBACK_PATH.to_owned(),
            redirect_host: None,
            state: Some(state),
            complete,
            signal: interaction.signal(),
            timeout: None,
        })
        .await?;

        interaction.notify(AuthEvent::Progress {
            message: format!("Listening for OAuth callback on {REDIRECT_URI}"),
        });
        interaction.notify(AuthEvent::AuthUrl {
            url: authorize_url.to_string(),
            instructions: Some("Continue in your browser.".to_owned()),
        });

        let result = match server.wait().await {
            Ok(Some(credential)) => Ok(credential),
            // `if (!credential) throw new Error("OAuth callback did not complete.")`.
            Ok(None) => Err(error("OAuth callback did not complete.")),
            Err(callback_error) => Err(callback_error),
        };

        // `finally { callbackServer.close(); }`
        server.close().await;
        result
    }
}

#[async_trait::async_trait]
impl OAuthAuth for RadiusOAuth {
    fn name(&self) -> &str {
        &self.name
    }

    /// `login` — pick the sign-in method, then browser (discovery + PKCE) or
    /// device code.
    async fn login(
        &self,
        interaction: &dyn AuthInteraction,
        _options: Option<&LoginOptions>,
    ) -> Result<OAuthCredential, ModelsError> {
        let login_method = interaction
            .prompt(AuthPrompt::Select {
                message: format!("Sign in to {}:", self.name),
                options: vec![
                    SelectOption {
                        id: LOGIN_METHOD_BROWSER.to_owned(),
                        label: "Sign in with browser (recommended)".to_owned(),
                        description: None,
                    },
                    SelectOption {
                        id: LOGIN_METHOD_DEVICE_CODE.to_owned(),
                        label: "Sign in with device code (when signing in from another device)"
                            .to_owned(),
                        description: None,
                    },
                ],
                signal: None,
            })
            .await?;

        match login_method.as_str() {
            LOGIN_METHOD_DEVICE_CODE => self.login_with_device_code(interaction).await,
            LOGIN_METHOD_BROWSER => {
                let authorization_endpoint = self.load_radius_oauth_discovery().await?;
                self.login_with_browser(&authorization_endpoint, interaction)
                    .await
            }
            other => Err(error(format!(
                "Unknown {} sign-in method: {other}",
                self.name
            ))),
        }
    }

    /// `refresh` — `refresh_token` grant straight against the gateway (no
    /// discovery).
    async fn refresh(
        &self,
        credential: &OAuthCredential,
        signal: Option<&CancellationToken>,
    ) -> Result<OAuthCredential, ModelsError> {
        self.request_oauth_token(
            &[
                ("grant_type", "refresh_token"),
                ("client_id", OAUTH_CLIENT_ID),
                ("refresh_token", credential.refresh.as_str()),
            ],
            signal,
        )
        .await
        .map_err(TokenRequestError::into_models_error)
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
    //! Test intents ported from `packages/ai/test/radius-oauth.test.ts` @ pi
    //! 0.82.1 (2efa728); the mocked `fetch` becomes a loopback axum gateway
    //! (the gateway is a constructor option, so no URL seam is needed) and
    //! the browser-flow callback server binds a free port via the
    //! `callback_port` seam.

    use std::sync::Mutex;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::response::Json;
    use serde_json::json;
    use tokio::sync::oneshot;

    use super::super::super::types::BoxFutureSend;
    use super::*;

    // ----- mock gateway (upstream: `vi.stubGlobal("fetch", ...)`) -----

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

    struct MockGateway {
        url: String,
        requests: Arc<Mutex<Vec<RecordedRequest>>>,
        shutdown: Option<oneshot::Sender<()>>,
    }

    impl MockGateway {
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

        fn oauth(&self) -> RadiusOAuth {
            RadiusOAuth::new("Radius", self.url.clone())
        }

        fn requests(&self) -> Vec<RecordedRequest> {
            self.requests.lock().expect("lock").clone()
        }
    }

    impl Drop for MockGateway {
        fn drop(&mut self) {
            if let Some(shutdown) = self.shutdown.take() {
                let _ = shutdown.send(());
            }
        }
    }

    // ----- fake AuthInteraction (upstream `interaction(loginMethod, events)`) -----

    #[derive(Clone, Default)]
    struct InteractionHandle {
        events: Arc<Mutex<Vec<AuthEvent>>>,
    }

    struct FakeInteraction {
        handle: InteractionHandle,
        login_method: String,
    }

    impl AuthInteraction for FakeInteraction {
        fn prompt<'a>(
            &'a self,
            prompt: AuthPrompt,
        ) -> BoxFutureSend<'a, Result<String, ModelsError>> {
            assert!(
                matches!(prompt, AuthPrompt::Select { .. }),
                "unexpected prompt: {prompt:?}"
            );
            let method = self.login_method.clone();
            Box::pin(async move { Ok(method) })
        }

        fn notify(&self, event: AuthEvent) {
            self.handle.events.lock().expect("lock").push(event);
        }
    }

    fn interaction(method: &str) -> (FakeInteraction, InteractionHandle) {
        let handle = InteractionHandle::default();
        (
            FakeInteraction {
                handle: handle.clone(),
                login_method: method.to_owned(),
            },
            handle,
        )
    }

    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .expect("bind")
            .local_addr()
            .expect("addr")
            .port()
    }

    fn device_authorization_response() -> Value {
        json!({
            "device_code": "device-code",
            "user_code": "ABCD-1234",
            "verification_uri": "https://radius-ui.example/pair",
            "expires_in": 600,
            "interval": 5,
        })
    }

    fn token_response(access: &str, refresh: &str) -> Value {
        json!({
            "access_token": access,
            "refresh_token": refresh,
            "expires_in": 3600,
            "scope": "gateway offline_access",
        })
    }

    // ----- pure-function tests -----

    /// `new URL(path, gateway)` absolute-path join.
    #[test]
    fn gateway_url_joins_absolute_paths() {
        assert_eq!(
            gateway_url("https://radius.example", "/v1/oauth").expect("url"),
            "https://radius.example/v1/oauth"
        );
        // A path component on the gateway is dropped (WHATWG absolute join).
        assert_eq!(
            gateway_url("https://radius.example/base", "/v1/oauth").expect("url"),
            "https://radius.example/v1/oauth"
        );
    }

    /// `crypto.randomUUID()` shape: 36 chars, v4 layout.
    #[test]
    fn random_uuid_shape() {
        let uuid = random_uuid().expect("uuid");
        assert_eq!(uuid.len(), 36);
        assert_eq!(uuid.chars().filter(|c| *c == '-').count(), 4);
        assert_eq!(uuid.as_bytes()[14], b'4');
        assert!(matches!(uuid.as_bytes()[19], b'8' | b'9' | b'a' | b'b'));
        assert_ne!(random_uuid().expect("uuid"), uuid);
    }

    // ----- flow tests against the mock gateway -----

    /// `uses gateway endpoints directly for device login` — verbatim upstream
    /// assertions: form fields, device-code event, request order, credential.
    #[tokio::test]
    async fn device_login_uses_gateway_endpoints_directly() {
        let responder: Responder = Arc::new(|request: &RecordedRequest| {
            if request.path == "/v1/oauth/device" {
                assert_eq!(request.form_get("client_id").as_deref(), Some("pi-gateway"));
                assert_eq!(
                    request.form_get("scope").as_deref(),
                    Some("gateway offline_access")
                );
                return (StatusCode::OK, device_authorization_response());
            }
            if request.path == "/v1/oauth/token" {
                assert_eq!(
                    request.form_get("grant_type").as_deref(),
                    Some("urn:ietf:params:oauth:grant-type:device_code")
                );
                assert_eq!(request.form_get("client_id").as_deref(), Some("pi-gateway"));
                assert_eq!(
                    request.form_get("device_code").as_deref(),
                    Some("device-code")
                );
                return (
                    StatusCode::OK,
                    token_response("access-token", "refresh-token"),
                );
            }
            panic!("unexpected request: {} {}", request.method, request.path);
        });
        let gateway = MockGateway::start(responder).await;
        let oauth = gateway.oauth();
        let (interaction, handle) = interaction("device-code");

        let before = now_ms();
        let credential = oauth.login(&interaction, None).await.expect("login");
        assert_eq!(credential.access, "access-token");
        assert_eq!(credential.refresh, "refresh-token");
        assert_eq!(
            credential.extra.get("scope"),
            Some(&json!("gateway offline_access"))
        );
        // `Date.now() + expires_in * 1000 - 60_000`.
        let expected = before + 3600 * 1000 - TOKEN_EXPIRY_SKEW_MS;
        assert!((credential.expires - expected).abs() < 10_000);

        assert_eq!(
            handle.events.lock().expect("lock").as_slice(),
            &[AuthEvent::DeviceCode {
                user_code: "ABCD-1234".to_owned(),
                verification_uri: "https://radius-ui.example/pair".to_owned(),
                interval_seconds: Some(5),
                expires_in_seconds: Some(600),
            }]
        );
        let paths: Vec<String> = gateway
            .requests()
            .iter()
            .map(|request| request.path.clone())
            .collect();
        assert_eq!(paths, ["/v1/oauth/device", "/v1/oauth/token"]);
    }

    /// `refreshes directly through the gateway without discovery`.
    #[tokio::test]
    async fn refresh_goes_directly_to_the_gateway_without_discovery() {
        let responder: Responder = Arc::new(|request: &RecordedRequest| {
            assert_eq!(request.path, "/v1/oauth/token");
            assert_eq!(
                request.form_get("grant_type").as_deref(),
                Some("refresh_token")
            );
            assert_eq!(request.form_get("client_id").as_deref(), Some("pi-gateway"));
            assert_eq!(
                request.form_get("refresh_token").as_deref(),
                Some("old-refresh")
            );
            (
                StatusCode::OK,
                json!({
                    "access_token": "new-access",
                    "refresh_token": "new-refresh",
                    "expires_in": 3600,
                }),
            )
        });
        let gateway = MockGateway::start(responder).await;
        let oauth = gateway.oauth();
        let credential = OAuthCredential {
            refresh: "old-refresh".to_owned(),
            access: "old-access".to_owned(),
            expires: 0,
            extra: Map::new(),
        };

        let refreshed = oauth.refresh(&credential, None).await.expect("refresh");
        assert_eq!(refreshed.access, "new-access");
        assert_eq!(refreshed.refresh, "new-refresh");
        assert!(refreshed.extra.get("scope").is_none());
        assert_eq!(gateway.requests().len(), 1);
    }

    /// `discovers only the interactive browser authorization endpoint` — a
    /// discovery document without `authorizationEndpoint` fails verbatim.
    #[tokio::test]
    async fn browser_discovery_requires_authorization_endpoint() {
        let responder: Responder = Arc::new(|request: &RecordedRequest| {
            assert_eq!(request.path, "/v1/oauth");
            (
                StatusCode::OK,
                json!({ "issuer": "https://radius-ui.example" }),
            )
        });
        let gateway = MockGateway::start(responder).await;
        let oauth = gateway.oauth();
        let (interaction, _handle) = interaction("browser");

        let error = oauth
            .login(&interaction, None)
            .await
            .expect_err("invalid config");
        assert_eq!(
            error.message,
            format!("Invalid Radius OAuth config from {}", gateway.url)
        );
        assert_eq!(gateway.requests().len(), 1);
    }

    /// Browser login: discovery → auth_url (PKCE S256 + state) → localhost
    /// callback → `authorization_code` exchange with the verifier.
    #[tokio::test]
    async fn browser_login_pkce_callback_happy_path() {
        // The discovery document must point back at the mock itself; the
        // gateway URL is only known after binding, so it is shared through a
        // OnceLock the responder reads per request.
        let self_url = Arc::new(std::sync::OnceLock::<String>::new());
        let responder: Responder = Arc::new({
            let self_url = self_url.clone();
            move |request: &RecordedRequest| {
                if request.path == "/v1/oauth" {
                    let gateway = self_url.get().expect("gateway url");
                    return (
                        StatusCode::OK,
                        json!({ "authorizationEndpoint": format!("{gateway}/authorize") }),
                    );
                }
                if request.path == "/v1/oauth/token" {
                    assert_eq!(
                        request.form_get("grant_type").as_deref(),
                        Some("authorization_code")
                    );
                    assert_eq!(request.form_get("client_id").as_deref(), Some("pi-gateway"));
                    assert_eq!(
                        request.form_get("redirect_uri").as_deref(),
                        Some("http://127.0.0.1:1456/oauth/callback")
                    );
                    assert_eq!(request.form_get("code").as_deref(), Some("the-code"));
                    let verifier = request.form_get("code_verifier").expect("verifier");
                    assert_eq!(verifier.len(), 43);
                    return (
                        StatusCode::OK,
                        token_response("browser-access", "browser-refresh"),
                    );
                }
                panic!("unexpected request: {} {}", request.method, request.path);
            }
        });
        let gateway = MockGateway::start(responder).await;
        self_url.set(gateway.url.clone()).expect("set gateway url");

        let callback_port = free_port();
        let oauth =
            RadiusOAuth::new("Radius", gateway.url.clone()).with_callback_port(callback_port);
        let (interaction, handle) = interaction("browser");
        let login = tokio::spawn(async move { oauth.login(&interaction, None).await });

        // Wait for the auth_url notification.
        let mut authorize = None;
        for _ in 0..1000 {
            let found = handle
                .events
                .lock()
                .expect("lock")
                .iter()
                .find_map(|event| match event {
                    AuthEvent::AuthUrl { url, .. } => Some(url.clone()),
                    _ => None,
                });
            if found.is_some() {
                authorize = found;
                break;
            }
            tokio::task::yield_now().await;
        }
        let authorize = authorize.expect("auth_url event");
        let parsed = url::Url::parse(&authorize).expect("authorize url");
        assert_eq!(parsed.path(), "/authorize");
        let param = |name: &str| {
            parsed
                .query_pairs()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.into_owned())
        };
        assert_eq!(param("response_type").as_deref(), Some("code"));
        assert_eq!(param("client_id").as_deref(), Some("pi-gateway"));
        assert_eq!(
            param("redirect_uri").as_deref(),
            Some("http://127.0.0.1:1456/oauth/callback")
        );
        assert_eq!(param("scope").as_deref(), Some("gateway offline_access"));
        assert_eq!(param("code_challenge_method").as_deref(), Some("S256"));
        assert_eq!(param("handoff").as_deref(), Some("url"));
        assert!(param("code_challenge").is_some_and(|value| !value.is_empty()));
        let state = param("state").expect("state");
        assert_eq!(state.len(), 36);

        // The progress notification advertises the fixed redirect URI.
        assert!(handle
            .events
            .lock()
            .expect("lock")
            .iter()
            .any(|event| matches!(
                event,
                AuthEvent::Progress { message } if message == &format!("Listening for OAuth callback on {REDIRECT_URI}")
            )));

        // Drive the localhost callback with the code + matching state.
        let response = reqwest::get(format!(
            "http://127.0.0.1:{callback_port}{CALLBACK_PATH}?code=the-code&state={state}"
        ))
        .await
        .expect("callback response");
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response
                .text()
                .await
                .expect("body")
                .contains("Signed in to Radius. You may now close this page.")
        );

        let credential = login.await.expect("join").expect("login");
        assert_eq!(credential.access, "browser-access");
        assert_eq!(credential.refresh, "browser-refresh");
    }

    /// A state mismatch renders the verbatim 400 page and does not settle
    /// the wait; a subsequent correct callback completes the login.
    #[tokio::test]
    async fn browser_callback_state_mismatch_does_not_settle() {
        let responder: Responder = Arc::new(|request: &RecordedRequest| {
            if request.path == "/v1/oauth" {
                return (
                    StatusCode::OK,
                    json!({ "authorizationEndpoint": "http://localhost/authorize" }),
                );
            }
            if request.path == "/v1/oauth/token" {
                return (StatusCode::OK, token_response("a", "r"));
            }
            panic!("unexpected request: {} {}", request.method, request.path);
        });
        let gateway = MockGateway::start(responder).await;
        let callback_port = free_port();
        let oauth =
            RadiusOAuth::new("Radius", gateway.url.clone()).with_callback_port(callback_port);
        let (interaction, handle) = interaction("browser");
        let login = tokio::spawn(async move { oauth.login(&interaction, None).await });

        let mut state = None;
        for _ in 0..1000 {
            let found = handle
                .events
                .lock()
                .expect("lock")
                .iter()
                .find_map(|event| match event {
                    AuthEvent::AuthUrl { url, .. } => {
                        url::Url::parse(url).ok().and_then(|parsed| {
                            parsed
                                .query_pairs()
                                .find(|(key, _)| key == "state")
                                .map(|(_, value)| value.into_owned())
                        })
                    }
                    _ => None,
                });
            if found.is_some() {
                state = found;
                break;
            }
            tokio::task::yield_now().await;
        }
        let state = state.expect("state");

        let response = reqwest::get(format!(
            "http://127.0.0.1:{callback_port}{CALLBACK_PATH}?code=wrong&state=not-the-state"
        ))
        .await
        .expect("callback response");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(
            response
                .text()
                .await
                .expect("body")
                .contains("State mismatch.")
        );

        let response = reqwest::get(format!(
            "http://127.0.0.1:{callback_port}{CALLBACK_PATH}?code=the-code&state={state}"
        ))
        .await
        .expect("callback response");
        assert_eq!(response.status(), StatusCode::OK);
        login.await.expect("join").expect("login");
    }

    /// An `error` callback fails the shared server wait with
    /// `${provider} authorization failed: ${description}` (upstream
    /// `callback-server.ts`); the page shows the same description.
    #[tokio::test]
    async fn browser_callback_error_param_fails_login() {
        let responder: Responder = Arc::new(|request: &RecordedRequest| {
            assert_eq!(request.path, "/v1/oauth");
            (
                StatusCode::OK,
                json!({ "authorizationEndpoint": "http://localhost/authorize" }),
            )
        });
        let gateway = MockGateway::start(responder).await;
        let callback_port = free_port();
        let oauth =
            RadiusOAuth::new("Radius", gateway.url.clone()).with_callback_port(callback_port);
        let (interaction, handle) = interaction("browser");
        let login = tokio::spawn(async move { oauth.login(&interaction, None).await });

        let mut state = None;
        for _ in 0..1000 {
            let found = handle
                .events
                .lock()
                .expect("lock")
                .iter()
                .find_map(|event| match event {
                    AuthEvent::AuthUrl { url, .. } => {
                        url::Url::parse(url).ok().and_then(|parsed| {
                            parsed
                                .query_pairs()
                                .find(|(key, _)| key == "state")
                                .map(|(_, value)| value.into_owned())
                        })
                    }
                    _ => None,
                });
            if found.is_some() {
                state = found;
                break;
            }
            tokio::task::yield_now().await;
        }
        let state = state.expect("state");

        let response = reqwest::get(format!(
            "http://127.0.0.1:{callback_port}{CALLBACK_PATH}?state={state}&error=access_denied&error_description=nope"
        ))
        .await
        .expect("callback response");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(response.text().await.expect("body").contains("nope"));

        let error = login.await.expect("join").expect_err("login must fail");
        assert_eq!(error.message, "Radius authorization failed: nope");
    }

    /// Device-poll error mapping: `authorization_pending` → pending,
    /// `slow_down` → backoff, then completion (radius.ts:340-351).
    #[tokio::test]
    async fn device_poll_maps_pending_and_slow_down_then_completes() {
        let polls = Arc::new(Mutex::new(0usize));
        let responder: Responder = Arc::new(move |request: &RecordedRequest| {
            if request.path == "/v1/oauth/device" {
                let mut response = device_authorization_response();
                response["interval"] = json!(1);
                return (StatusCode::OK, response);
            }
            if request.path == "/v1/oauth/token" {
                let mut count = polls.lock().expect("lock");
                *count += 1;
                return match *count {
                    1 => (
                        StatusCode::BAD_REQUEST,
                        json!({ "error": "authorization_pending" }),
                    ),
                    2 => (StatusCode::BAD_REQUEST, json!({ "error": "slow_down" })),
                    _ => (
                        StatusCode::OK,
                        token_response("access-token", "refresh-token"),
                    ),
                };
            }
            panic!("unexpected request: {} {}", request.method, request.path);
        });
        let gateway = MockGateway::start(responder).await;
        let oauth = gateway.oauth();
        let (interaction, _handle) = interaction("device-code");

        let credential = oauth.login(&interaction, None).await.expect("login");
        assert_eq!(credential.access, "access-token");
        let poll_count = gateway
            .requests()
            .iter()
            .filter(|request| request.path == "/v1/oauth/token")
            .count();
        assert_eq!(poll_count, 3);
    }

    /// `expired_token` / `access_denied` fail the flow with verbatim messages.
    #[tokio::test]
    async fn device_poll_terminal_errors_fail_verbatim() {
        for (oauth_error, expected) in [
            ("expired_token", "Device authorization expired."),
            ("access_denied", "Device authorization was denied."),
        ] {
            let responder: Responder = Arc::new(move |request: &RecordedRequest| {
                if request.path == "/v1/oauth/device" {
                    let mut response = device_authorization_response();
                    response["interval"] = json!(1);
                    return (StatusCode::OK, response);
                }
                if request.path == "/v1/oauth/token" {
                    return (
                        StatusCode::BAD_REQUEST,
                        json!({ "error": oauth_error, "error_description": "ignored" }),
                    );
                }
                panic!("unexpected request: {} {}", request.method, request.path);
            });
            let gateway = MockGateway::start(responder).await;
            let oauth = gateway.oauth();
            let (interaction, _handle) = interaction("device-code");

            let error = oauth
                .login(&interaction, None)
                .await
                .expect_err("terminal error");
            assert_eq!(error.message, expected);
        }
    }

    /// Missing device-authorization fields (JS falsy semantics) fail verbatim.
    #[tokio::test]
    async fn device_authorization_missing_fields_errors() {
        let responder: Responder = Arc::new(|request: &RecordedRequest| {
            assert_eq!(request.path, "/v1/oauth/device");
            (
                StatusCode::OK,
                json!({ "device_code": "device-code", "expires_in": 600 }),
            )
        });
        let gateway = MockGateway::start(responder).await;
        let oauth = gateway.oauth();
        let (interaction, _handle) = interaction("device-code");

        let error = oauth
            .login(&interaction, None)
            .await
            .expect_err("missing fields");
        assert_eq!(
            error.message,
            "Radius OAuth device authorization response is missing required fields"
        );
    }

    /// `readOAuthResponseError` detail formats, surfaced through `refresh`:
    /// `error: description`, bare `error`, non-JSON body, empty body.
    #[tokio::test]
    async fn token_error_detail_formats() {
        let cases: Vec<(Value, &str)> = vec![
            (
                json!({ "error": "invalid_grant", "error_description": "bad refresh" }),
                "Radius OAuth token request failed: invalid_grant: bad refresh",
            ),
            (
                json!({ "error": "invalid_grant" }),
                "Radius OAuth token request failed: invalid_grant",
            ),
        ];
        for (body, expected) in cases {
            let responder: Responder =
                Arc::new(move |_request: &RecordedRequest| (StatusCode::BAD_REQUEST, body.clone()));
            let gateway = MockGateway::start(responder).await;
            let oauth = gateway.oauth();
            let credential = OAuthCredential {
                refresh: "r".to_owned(),
                access: "a".to_owned(),
                expires: 0,
                extra: Map::new(),
            };
            let error = oauth.refresh(&credential, None).await.expect_err("400");
            assert_eq!(error.message, expected);
        }
    }

    /// Unknown sign-in method fails verbatim.
    #[tokio::test]
    async fn unknown_login_method_errors() {
        let gateway = MockGateway::start(Arc::new(|_request: &RecordedRequest| {
            panic!("no request may leave");
        }))
        .await;
        let oauth = gateway.oauth();
        let (interaction, _handle) = interaction("weird");

        let error = oauth
            .login(&interaction, None)
            .await
            .expect_err("unknown method");
        assert_eq!(error.message, "Unknown Radius sign-in method: weird");
    }
}
