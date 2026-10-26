//! Port of `packages/ai/src/auth/oauth/openai-chatgpt.ts` @ pi abe508e1b
//! (v1.0.0, `02eed88fd`) — OpenAI Responses API token sharing through Sign in
//! with ChatGPT.
//!
//! Public-client flow: every login registers a new client with
//! `dynamic_agent_client`, OpenAI returns the issued client ID on the
//! callback, and the resulting user access token is sent directly to
//! `api.openai.com`.
//!
//! Intentional differences: the upstream `node:http` callback server becomes
//! an axum router (coding-standards appendix A) and the promise race becomes
//! `tokio::select!`; `AbortSignal` becomes a [`CancellationToken`]. The
//! callback server stays bespoke (as upstream): the issued `client_id` is a
//! callback query parameter the shared [`super::callback_server`] hook does
//! not expose.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{Method, Response, StatusCode, Uri};
use axum::response::{Html, IntoResponse};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use super::super::interaction::{AuthEvent, AuthInteraction, AuthPrompt};
use super::super::resolve::{ModelsError, ModelsErrorCode};
use super::super::types::{LoginOptions, ModelAuth, OAuthAuth, OAuthCredential};
use super::callback_page::{oauth_error_html, oauth_success_html};
use super::callback_server::default_callback_host;
use super::pkce::generate_pkce;

/// `DYNAMIC_CLIENT_ID` — every login registers a new client with this ID;
/// OpenAI returns the issued client ID in the callback.
const DYNAMIC_CLIENT_ID: &str = "dynamic_agent_client";
/// `AGENT_NAME_HINT` — the app's name in OpenAI's consent screen (upstream
/// "Pi"; ADR-0001 brand rename). Overridable through `LoginOptions.agentName`.
const AGENT_NAME_HINT: &str = "rpi";
/// `AUTHORIZE_URL`.
const AUTHORIZE_URL: &str = "https://auth.openai.com/api/accounts/authorize";
/// `TOKEN_URL`.
const TOKEN_URL: &str = "https://auth.openai.com/api/accounts/oauth/token";
/// `RESOURCE`.
const RESOURCE: &str = "https://api.openai.com/v1";
/// `CALLBACK_PORT`.
const CALLBACK_PORT: u16 = 1455;
/// `CALLBACK_PATH`.
const CALLBACK_PATH: &str = "/auth/callback";
/// `REDIRECT_URI` — fixed loopback URL, independent of the bind host override.
const REDIRECT_URI: &str = "http://127.0.0.1:1455/auth/callback";
/// `DIRECT_TOKEN_SCOPE`.
const DIRECT_TOKEN_SCOPE: &str = "chatgpt.tokens.use.direct";
/// `SCOPE`.
const SCOPE: &str = "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
/// `EXPIRY_MARGIN_MS` — refresh this long before the real expiry so a request
/// never starts with a token about to expire.
const EXPIRY_MARGIN_MS: i64 = 3 * 60 * 1000;

fn error(message: impl Into<String>) -> ModelsError {
    ModelsError::new(ModelsErrorCode::Oauth, message.into())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

/// `randomValue()` — 32 random bytes, base64url without padding.
fn random_value() -> String {
    use base64::Engine;
    use ring::rand::SecureRandom;
    let rng = ring::rand::SystemRandom::new();
    let mut bytes = [0u8; 32];
    // Invariant: the system RNG is available; a failure would only degrade
    // the state/nonce to a zero value, which a fresh call replaces.
    if rng.fill(&mut bytes).is_err() {
        bytes = [0u8; 32];
    }
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// `UUID_PATTERN` shape check (case-insensitive).
fn is_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (index, byte) in bytes.iter().enumerate() {
        match index {
            8 | 13 | 18 | 23 => {
                if *byte != b'-' {
                    return false;
                }
            }
            _ => {
                if !byte.is_ascii_hexdigit() {
                    return false;
                }
            }
        }
    }
    true
}

/// `AuthorizationResult` — the callback carries the issued `client_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizationResult {
    pub code: String,
    pub client_id: String,
}

/// `authorizationResultFromCallback`.
fn authorization_result_from_callback(
    url: &url::Url,
    expected_state: &str,
) -> Result<AuthorizationResult, ModelsError> {
    let code = url
        .query_pairs()
        .find(|(key, _)| key == "code")
        .map(|(_, value)| value.into_owned())
        .filter(|code| !code.is_empty())
        .ok_or_else(|| error("Missing authorization code"))?;
    let state = url
        .query_pairs()
        .find(|(key, _)| key == "state")
        .map(|(_, value)| value.into_owned())
        .filter(|state| !state.is_empty())
        .ok_or_else(|| error("Missing OAuth state"))?;
    if state != expected_state {
        return Err(error("OAuth state mismatch"));
    }
    let client_id = url
        .query_pairs()
        .find(|(key, _)| key == "client_id")
        .map(|(_, value)| value.trim().to_owned())
        .filter(|client_id| !client_id.is_empty())
        .ok_or_else(|| {
            error("OpenAI OAuth registration callback did not contain an issued client ID")
        })?;
    Ok(AuthorizationResult { code, client_id })
}

/// `authorizationResultFromManualInput`.
fn authorization_result_from_manual_input(
    input: &str,
    expected_state: &str,
) -> Result<AuthorizationResult, ModelsError> {
    let url = url::Url::parse(input.trim())
        .map_err(|_| error("Paste the full callback URL from the browser"))?;
    let expected =
        url::Url::parse(REDIRECT_URI).map_err(|parse_error| error(parse_error.to_string()))?;
    if url.origin() != expected.origin() || url.path() != expected.path() {
        return Err(error(format!(
            "The pasted callback URL must start with {REDIRECT_URI}"
        )));
    }
    if let Some((_, value)) = url
        .query_pairs()
        .find(|(key, _)| key == "error")
        .filter(|(_, value)| !value.is_empty())
    {
        return Err(error(format!("ChatGPT authorization failed: {value}")));
    }
    authorization_result_from_callback(&url, expected_state)
}

/// `agentHostId(deviceId)` — OpenAI identifies each installation by a stable
/// `urn:uuid:<uuid>`.
fn agent_host_id(device_id: Option<&str>) -> Result<String, ModelsError> {
    let device_id = device_id
        .filter(|device_id| is_uuid(device_id))
        .ok_or_else(|| {
            error("Sign in with ChatGPT requires a device ID (UUID) for this installation")
        })?;
    Ok(format!("urn:uuid:{}", device_id.to_lowercase()))
}

// ---------------------------------------------------------------------------
// `startCallbackServer` (openai-chatgpt.ts's own `node:http` server, here axum)
// ---------------------------------------------------------------------------

/// First occurrence of each query parameter (mirrors `URLSearchParams.get`).
fn query_params(uri: &Uri) -> HashMap<String, String> {
    let mut params = HashMap::new();
    for (key, value) in url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes()) {
        params
            .entry(key.into_owned())
            .or_insert_with(|| value.into_owned());
    }
    params
}

#[derive(Clone)]
enum CallbackSettle {
    Result(AuthorizationResult),
    Error(ModelsError),
}

struct CallbackState {
    expected_state: String,
    /// Settle-once channel: outer `None` = waiting; `Some(settle)` = done.
    settle: watch::Sender<Option<CallbackSettle>>,
    settled: AtomicBool,
}

impl CallbackState {
    fn settle(&self, value: CallbackSettle) {
        if self
            .settled
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            self.settle.send_replace(Some(value));
        }
    }
}

fn send_page(status: StatusCode, html: String) -> Response<Body> {
    (
        status,
        [
            (axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (axum::http::header::CACHE_CONTROL, "no-store"),
        ],
        Html(html),
    )
        .into_response()
}

async fn handle_chatgpt_callback(
    State(state): State<Arc<CallbackState>>,
    request: Request,
) -> Response<Body> {
    let uri = request.uri().clone();
    if request.method() != Method::GET || uri.path() != CALLBACK_PATH {
        return send_page(
            StatusCode::NOT_FOUND,
            oauth_error_html("Callback route not found.", None),
        );
    }
    let params = query_params(&uri);
    if let Some(callback_error) = params.get("error").filter(|value| !value.is_empty()) {
        state.settle(CallbackSettle::Error(error(format!(
            "ChatGPT authorization failed: {callback_error}"
        ))));
        return send_page(
            StatusCode::BAD_REQUEST,
            oauth_error_html(
                "ChatGPT was not connected.",
                Some(&format!("Error: {callback_error}")),
            ),
        );
    }
    let url = match url::Url::parse(&format!("{REDIRECT_URI}?{}", uri.query().unwrap_or(""))) {
        Ok(url) => url,
        Err(parse_error) => {
            return send_page(
                StatusCode::BAD_REQUEST,
                oauth_error_html(&parse_error.to_string(), None),
            );
        }
    };
    match authorization_result_from_callback(&url, &state.expected_state) {
        Ok(authorization_result) => {
            state.settle(CallbackSettle::Result(authorization_result));
            send_page(
                StatusCode::OK,
                oauth_success_html("ChatGPT authentication completed. You can close this window."),
            )
        }
        Err(parse_error) => send_page(
            StatusCode::BAD_REQUEST,
            oauth_error_html(&parse_error.message, None),
        ),
    }
}

/// `startCallbackServer` — one-shot callback server on `CALLBACK_PORT` (bind
/// host from `RPI_OAUTH_CALLBACK_HOST`). Bind failures propagate; the caller
/// notifies and falls back to the pasted redirect URL.
struct ChatGptCallbackServer {
    state: Arc<CallbackState>,
    shutdown: CancellationToken,
    serve: Option<tokio::task::JoinHandle<()>>,
}

/// Why [`ChatGptCallbackServer::start`] failed.
enum ChatGptCallbackStartError {
    /// `EADDRINUSE`: another pending login (another rpi session or the Codex
    /// CLI) holds the port. The browser callback would reach that listener and
    /// be rejected with a state mismatch, so login fails before opening the
    /// browser (#10265 `eeac84ca9`).
    PortInUse(std::io::Error),
    /// Any other bind failure; login falls back to the pasted redirect URL.
    Bind(ModelsError),
}

impl std::fmt::Display for ChatGptCallbackStartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChatGptCallbackStartError::PortInUse(bind_error) => write!(f, "{bind_error}"),
            ChatGptCallbackStartError::Bind(error) => write!(f, "{error}"),
        }
    }
}

impl ChatGptCallbackServer {
    async fn start(expected_state: &str, port: u16) -> Result<Self, ChatGptCallbackStartError> {
        let host = default_callback_host();
        let listener = tokio::net::TcpListener::bind((host.as_str(), port))
            .await
            .map_err(|bind_error| {
                if bind_error.kind() == std::io::ErrorKind::AddrInUse {
                    ChatGptCallbackStartError::PortInUse(bind_error)
                } else {
                    ChatGptCallbackStartError::Bind(error(format!(
                        "OAuth callback server failed to bind {host}:{port}: {bind_error}"
                    )))
                }
            })?;
        let (settle, _) = watch::channel(None);
        let state = Arc::new(CallbackState {
            expected_state: expected_state.to_owned(),
            settle,
            settled: AtomicBool::new(false),
        });
        let app = axum::Router::new()
            .fallback(handle_chatgpt_callback)
            .with_state(state.clone());
        let shutdown = CancellationToken::new();
        let serve_shutdown = shutdown.clone();
        let serve = tokio::spawn(async move {
            let result = axum::serve(listener, app)
                .with_graceful_shutdown(serve_shutdown.cancelled_owned())
                .await;
            if let Err(serve_error) = result {
                tracing::warn!(%serve_error, "ChatGPT OAuth callback server terminated with an error");
            }
        });
        Ok(Self {
            state,
            shutdown,
            serve: Some(serve),
        })
    }

    /// `result` — resolves with the callback result or rejects on an error
    /// redirect.
    async fn wait(&self) -> Result<AuthorizationResult, ModelsError> {
        let mut rx = self.state.settle.subscribe();
        if let Some(value) = rx.borrow().clone() {
            return value.into_result();
        }
        loop {
            if rx.changed().await.is_err() {
                return Err(error("OAuth callback server closed"));
            }
            if let Some(value) = rx.borrow_and_update().clone() {
                return value.into_result();
            }
        }
    }

    /// `close()` — graceful shutdown also closes idle keep-alive connections
    /// (upstream `closeAllConnections()`).
    async fn close(mut self) {
        self.shutdown.cancel();
        if let Some(serve) = self.serve.take() {
            let _ = serve.await;
        }
    }
}

impl CallbackSettle {
    fn into_result(self) -> Result<AuthorizationResult, ModelsError> {
        match self {
            CallbackSettle::Result(result) => Ok(result),
            CallbackSettle::Error(error) => Err(error),
        }
    }
}

impl Drop for ChatGptCallbackServer {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

// ---------------------------------------------------------------------------
// Token endpoint helpers
// ---------------------------------------------------------------------------

/// `openaiChatGPTOAuth` — the OpenAI ChatGPT subscription OAuth provider auth.
pub fn openai_chatgpt_oauth() -> Arc<dyn OAuthAuth> {
    Arc::new(OpenAiChatGptOAuth::new())
}

/// OpenAI ChatGPT OAuth (`OAuthAuth`) implementation.
pub struct OpenAiChatGptOAuth {
    client: reqwest::Client,
    /// `TOKEN_URL` (test seam — see module docs).
    token_url: String,
    /// `CALLBACK_PORT` (test seam — see module docs).
    callback_port: u16,
}

impl Default for OpenAiChatGptOAuth {
    fn default() -> Self {
        Self::new()
    }
}

impl OpenAiChatGptOAuth {
    pub fn new() -> Self {
        Self::with_endpoints(TOKEN_URL, CALLBACK_PORT)
    }

    fn with_endpoints(token_url: impl Into<String>, callback_port: u16) -> Self {
        Self {
            client: reqwest::Client::new(),
            token_url: token_url.into(),
            callback_port,
        }
    }

    /// `requestToken` — POST the form body, parse the JSON object. Failures
    /// carry the upstream message text (`OpenAI OAuth token request failed`).
    async fn request_token(
        &self,
        body: &[(&str, String)],
        signal: Option<&CancellationToken>,
    ) -> Result<serde_json::Value, ModelsError> {
        let send = self
            .client
            .post(&self.token_url)
            .header(reqwest::header::ACCEPT, "application/json")
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .form(body)
            .send();
        let response = match signal {
            Some(token) => {
                tokio::select! {
                    () = token.cancelled() => return Err(error("Login cancelled")),
                    response = send => response,
                }
            }
            None => send.await,
        };
        let response =
            response.map_err(|request_error| error(format_error_details(&request_error)))?;
        let status = response.status();
        let status_text = status.canonical_reason().unwrap_or_default().to_owned();
        let response_body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            let detail = if response_body.is_empty() {
                status_text
            } else {
                response_body
            };
            return Err(error(format!(
                "OpenAI OAuth token request failed ({}): {detail}",
                status.as_u16()
            )));
        }
        let data: serde_json::Value = serde_json::from_str(&response_body)
            .map_err(|json_error| error(format!("Error: {json_error}")))?;
        if !data.is_object() {
            return Err(error("OpenAI OAuth token response must be an object"));
        }
        Ok(data)
    }

    /// `requireTokenString`.
    fn require_token_string<'a>(
        token: &'a serde_json::Value,
        field: &str,
    ) -> Result<&'a str, ModelsError> {
        token
            .get(field)
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| error(format!("OpenAI OAuth token response has invalid {field}")))
    }

    /// `credentialFromTokenResponse`.
    fn credential_from_token_response(
        token: &serde_json::Value,
        client_id: &str,
    ) -> Result<OAuthCredential, ModelsError> {
        let access = Self::require_token_string(token, "access_token")?.to_owned();
        let refresh = Self::require_token_string(token, "refresh_token")?.to_owned();
        let scope = Self::require_token_string(token, "scope")?.to_owned();
        let expires_in = token
            .get("expires_in")
            .and_then(serde_json::Value::as_f64)
            .filter(|expires_in| expires_in.is_finite() && *expires_in > 0.0)
            .ok_or_else(|| error("OpenAI OAuth token response has invalid expires_in"))?;
        let scopes: Vec<String> = scope
            .split_whitespace()
            .filter(|scope| !scope.is_empty())
            .map(str::to_owned)
            .collect();
        if !scopes.iter().any(|scope| scope == DIRECT_TOKEN_SCOPE) {
            return Err(error(format!(
                "OpenAI OAuth grant did not include {DIRECT_TOKEN_SCOPE}"
            )));
        }
        let mut extra = serde_json::Map::new();
        extra.insert(
            "clientId".to_owned(),
            serde_json::Value::String(client_id.to_owned()),
        );
        extra.insert(
            "scopes".to_owned(),
            serde_json::Value::Array(scopes.into_iter().map(serde_json::Value::String).collect()),
        );
        Ok(OAuthCredential {
            refresh,
            access,
            expires: now_ms() + (expires_in * 1000.0) as i64 - EXPIRY_MARGIN_MS,
            extra,
        })
    }

    /// `exchangeAuthorizationCode`.
    async fn exchange_authorization_code(
        &self,
        code: &str,
        verifier: &str,
        client_id: &str,
        signal: Option<&CancellationToken>,
    ) -> Result<OAuthCredential, ModelsError> {
        let token = self
            .request_token(
                &[
                    ("grant_type", "authorization_code".to_owned()),
                    ("client_id", client_id.to_owned()),
                    ("code", code.to_owned()),
                    ("code_verifier", verifier.to_owned()),
                    ("redirect_uri", REDIRECT_URI.to_owned()),
                    ("resource", RESOURCE.to_owned()),
                ],
                signal,
            )
            .await?;
        // Pi does not use the ID token to identify the user or read profile
        // data. Keep the presence check as part of the token-response contract.
        if token
            .get("id_token")
            .and_then(serde_json::Value::as_str)
            .is_none_or(|id_token| id_token.trim().is_empty())
        {
            return Err(error(
                "OpenAI OAuth token response did not contain an ID token",
            ));
        }
        Self::credential_from_token_response(&token, client_id)
    }

    /// `refreshAccessToken` — uses the stored issued client ID and requires
    /// the rotated refresh token.
    async fn refresh_access_token(
        &self,
        credential: &OAuthCredential,
        signal: Option<&CancellationToken>,
    ) -> Result<OAuthCredential, ModelsError> {
        let client_id = credential
            .extra
            .get("clientId")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|client_id| !client_id.is_empty())
            .ok_or_else(|| {
                error(
                    "Stored OpenAI OAuth credential does not contain an issued client ID; reconnect ChatGPT",
                )
            })?
            .to_owned();
        let token = self
            .request_token(
                &[
                    ("grant_type", "refresh_token".to_owned()),
                    ("client_id", client_id.clone()),
                    ("refresh_token", credential.refresh.clone()),
                    ("resource", RESOURCE.to_owned()),
                ],
                signal,
            )
            .await?;
        Self::credential_from_token_response(&token, &client_id)
    }

    /// `loginOpenAIChatGPT`.
    async fn login_openai_chatgpt(
        &self,
        interaction: &dyn AuthInteraction,
        options: Option<&LoginOptions>,
    ) -> Result<OAuthCredential, ModelsError> {
        let device_id = options.and_then(|options| options.get_device_id.as_ref().map(|get| get()));
        let host_id = agent_host_id(device_id.as_deref())?;
        let pkce = generate_pkce();
        let verifier = pkce.verifier;
        let challenge = pkce.challenge;
        let state = random_value();
        let nonce = random_value();

        let callback = match ChatGptCallbackServer::start(&state, self.callback_port).await {
            Ok(callback) => Some(callback),
            // #10265 (`eeac84ca9`): without the server, the browser callback
            // would reach whatever else holds the port, which rejects it as a
            // state mismatch. Fail with a clear error instead.
            Err(ChatGptCallbackStartError::PortInUse(bind_error)) => {
                return Err(error(format!(
                    "Port {} is in use, probably by an unfinished login in another rpi session or by the Codex CLI. Cancel that login and try again. ({bind_error})",
                    self.callback_port
                )));
            }
            Err(ChatGptCallbackStartError::Bind(start_error)) => {
                interaction.notify(AuthEvent::Info {
                    message: format!(
                        "Could not listen on {REDIRECT_URI}; paste the final redirect URL to continue. {start_error}"
                    ),
                    links: None,
                });
                None
            }
        };

        let authorization_url = {
            let query = url::form_urlencoded::Serializer::new(String::new())
                .extend_pairs([
                    ("client_id", DYNAMIC_CLIENT_ID),
                    (
                        "agent_name_hint",
                        options
                            .and_then(|options| options.agent_name.as_deref())
                            .unwrap_or(AGENT_NAME_HINT),
                    ),
                    ("ext_agent_host_id", host_id.as_str()),
                    ("response_type", "code"),
                    ("redirect_uri", REDIRECT_URI),
                    ("resource", RESOURCE),
                    ("scope", SCOPE),
                    ("state", state.as_str()),
                    ("code_challenge", challenge.as_str()),
                    ("code_challenge_method", "S256"),
                    ("nonce", nonce.as_str()),
                ])
                .finish();
            format!("{AUTHORIZE_URL}?{query}")
        };
        interaction.notify(AuthEvent::AuthUrl {
            url: authorization_url,
            instructions: Some(
                "Complete sign-in in your browser. If the callback does not complete, paste the final redirect URL here."
                    .to_owned(),
            ),
        });

        let manual_cancel = CancellationToken::new();
        // Upstream signals the manual prompt with
        // `AbortSignal.any([manualAbort.signal, interaction.signal])`
        // (openai-chatgpt.ts:278): an interaction abort must cancel the
        // prompt too, not only the later token exchange (v0.1.6 review
        // P2-2).
        let prompt_signal = CancellationToken::new();
        let prompt_watcher = {
            let prompt_signal = prompt_signal.clone();
            let manual_cancel = manual_cancel.clone();
            let interaction_signal = interaction.signal();
            tokio::spawn(async move {
                match interaction_signal {
                    Some(signal) => {
                        tokio::select! {
                            _ = signal.cancelled() => {}
                            _ = manual_cancel.cancelled() => {}
                        }
                    }
                    None => manual_cancel.cancelled().await,
                }
                prompt_signal.cancel();
            })
        };
        let prompt = interaction.prompt(AuthPrompt::ManualCode {
            message: "Complete login in your browser, or paste the final redirect URL here:"
                .to_owned(),
            placeholder: Some(REDIRECT_URI.to_owned()),
            signal: Some(prompt_signal),
        });
        tokio::pin!(prompt);

        let outcome = match &callback {
            Some(server) => {
                let wait = server.wait();
                tokio::pin!(wait);
                tokio::select! {
                    result = &mut wait => {
                        manual_cancel.cancel();
                        result
                    }
                    manual = &mut prompt => {
                        manual.and_then(|input| {
                            authorization_result_from_manual_input(&input, &state)
                        })
                    }
                }
            }
            None => prompt
                .await
                .and_then(|input| authorization_result_from_manual_input(&input, &state)),
        };
        if let Some(callback) = callback {
            callback.close().await;
        }
        prompt_watcher.abort();

        // `catch (error) { if (interaction.signal.aborted) throw ... }` —
        // an aborted login reports cancellation, not the prompt error.
        let authorization = match outcome {
            Err(_)
                if interaction
                    .signal()
                    .is_some_and(|signal| signal.is_cancelled()) =>
            {
                return Err(error("Login cancelled"));
            }
            outcome => outcome?,
        };

        interaction.notify(AuthEvent::Progress {
            message: "Exchanging authorization code for tokens...".to_owned(),
        });
        self.exchange_authorization_code(
            &authorization.code,
            &verifier,
            &authorization.client_id,
            interaction.signal().as_ref(),
        )
        .await
    }
}

/// `formatErrorDetails` approximation for the request layer: `name: message`
/// with the `std::error::Error::source` chain.
fn format_error_details(error: &reqwest::Error) -> String {
    let name = if error.is_timeout() {
        "TimeoutError"
    } else if error.is_connect() {
        "ConnectionError"
    } else {
        "Error"
    };
    let mut details = vec![format!("{name}: {error}")];
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        details.push(format!("cause={cause}"));
        source = cause.source();
    }
    details.join("; ")
}

#[async_trait::async_trait]
impl OAuthAuth for OpenAiChatGptOAuth {
    fn name(&self) -> &str {
        "OpenAI (ChatGPT subscription)"
    }

    /// `isSubscription: true` (providers/openai.ts:15 @ abe508e1b).
    fn is_subscription(&self) -> bool {
        true
    }

    async fn login(
        &self,
        interaction: &dyn AuthInteraction,
        options: Option<&LoginOptions>,
    ) -> Result<OAuthCredential, ModelsError> {
        self.login_openai_chatgpt(interaction, options).await
    }

    /// `refresh: (credential, signal) => refreshAccessToken(...)`.
    async fn refresh(
        &self,
        credential: &OAuthCredential,
        signal: Option<&CancellationToken>,
    ) -> Result<OAuthCredential, ModelsError> {
        self.refresh_access_token(credential, signal).await
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
    //! Test intents ported from `packages/ai/test/openai-chatgpt-oauth.test.ts`
    //! @ pi abe508e1b (v1.1.0), same names in snake_case. The mocked `fetch`
    //! becomes a loopback axum token endpoint behind the `token_url` seam;
    //! the fixed callback port gets a `callback_port` seam so tests never
    //! collide with a real Codex CLI or another test.

    use std::sync::Mutex;

    use axum::Json;
    use axum::routing::post;
    use serde_json::{Value, json};
    use tokio::sync::oneshot;

    use super::super::super::interaction::AuthEvent;
    use super::super::super::types::BoxFutureSend;
    use super::*;

    const REQUIRED_SCOPE: &str =
        "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
    const DEVICE_ID: &str = "e61bbe28-07ef-466d-8e5d-a344f94ab305";

    fn token_response(scope: &str) -> Value {
        json!({
            "access_token": "access-token",
            "refresh_token": "refresh-token",
            "expires_in": 3600,
            "id_token": "id-token",
            "scope": scope,
        })
    }

    struct MockTokenEndpoint {
        url: String,
        requests: Arc<Mutex<Vec<Value>>>,
        shutdown: Option<oneshot::Sender<()>>,
    }

    impl MockTokenEndpoint {
        async fn start(response: Value) -> Self {
            let requests: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
            let handler_requests = requests.clone();
            let app = axum::Router::new().route(
                "/api/accounts/oauth/token",
                post(move |body: String| {
                    let requests = handler_requests.clone();
                    let response = response.clone();
                    async move {
                        let mut parsed = serde_json::Map::new();
                        for (key, value) in url::form_urlencoded::parse(body.as_bytes()) {
                            parsed.insert(key.into_owned(), Value::String(value.into_owned()));
                        }
                        requests.lock().expect("lock").push(Value::Object(parsed));
                        Json(response)
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

        fn oauth(&self) -> OpenAiChatGptOAuth {
            OpenAiChatGptOAuth::with_endpoints(format!("{}/api/accounts/oauth/token", self.url), 0)
        }

        fn bodies(&self) -> Vec<Value> {
            self.requests.lock().expect("lock").clone()
        }
    }

    impl Drop for MockTokenEndpoint {
        fn drop(&mut self) {
            if let Some(shutdown) = self.shutdown.take() {
                let _ = shutdown.send(());
            }
        }
    }

    type PromptHandler =
        Box<dyn Fn(&AuthPrompt, &[AuthEvent]) -> Result<String, ModelsError> + Send + Sync>;

    struct TestInteraction {
        signal: CancellationToken,
        events: Arc<Mutex<Vec<AuthEvent>>>,
        prompt_handler: PromptHandler,
    }

    impl TestInteraction {
        fn new(prompt_handler: PromptHandler) -> Self {
            Self {
                signal: CancellationToken::new(),
                events: Arc::new(Mutex::new(Vec::new())),
                prompt_handler,
            }
        }

        fn events(&self) -> Vec<AuthEvent> {
            self.events.lock().expect("lock").clone()
        }
    }

    impl AuthInteraction for TestInteraction {
        fn signal(&self) -> Option<CancellationToken> {
            Some(self.signal.clone())
        }

        fn prompt<'a>(
            &'a self,
            prompt: AuthPrompt,
        ) -> BoxFutureSend<'a, Result<String, ModelsError>> {
            let events = self.events();
            Box::pin(async move { (self.prompt_handler)(&prompt, &events) })
        }

        fn notify(&self, event: AuthEvent) {
            self.events.lock().expect("lock").push(event);
        }
    }

    /// Interaction whose prompt never settles until its per-prompt signal is
    /// cancelled (the mounted login dialog waiting for input).
    struct PendingInteraction {
        signal: CancellationToken,
        events: Mutex<Vec<AuthEvent>>,
    }

    impl PendingInteraction {
        fn new() -> Self {
            Self {
                signal: CancellationToken::new(),
                events: Mutex::new(Vec::new()),
            }
        }

        fn events(&self) -> Vec<AuthEvent> {
            self.events.lock().expect("lock").clone()
        }
    }

    impl AuthInteraction for PendingInteraction {
        fn signal(&self) -> Option<CancellationToken> {
            Some(self.signal.clone())
        }

        fn prompt<'a>(
            &'a self,
            prompt: AuthPrompt,
        ) -> BoxFutureSend<'a, Result<String, ModelsError>> {
            Box::pin(async move {
                let signal = prompt.signal().expect("per-prompt signal");
                signal.cancelled().await;
                Err(error("Login cancelled"))
            })
        }

        fn notify(&self, event: AuthEvent) {
            self.events.lock().expect("lock").push(event);
        }
    }

    fn interactive_callback_url(events: &[AuthEvent], callback_client_id: Option<&str>) -> String {
        let authorize_url = events
            .iter()
            .find_map(|event| match event {
                AuthEvent::AuthUrl { url, .. } => Some(url.clone()),
                _ => None,
            })
            .expect("authorization URL was not emitted before the callback prompt");
        let authorize = url::Url::parse(&authorize_url).expect("authorize url");
        let mut callback = url::Url::parse(
            &authorize
                .query_pairs()
                .find(|(key, _)| key == "redirect_uri")
                .expect("redirect_uri")
                .1,
        )
        .expect("callback url");
        callback
            .query_pairs_mut()
            .append_pair("code", "authorization-code")
            .append_pair(
                "state",
                &authorize
                    .query_pairs()
                    .find(|(key, _)| key == "state")
                    .expect("state")
                    .1,
            );
        if let Some(client_id) = callback_client_id {
            callback
                .query_pairs_mut()
                .append_pair("client_id", client_id);
        }
        callback.to_string()
    }

    fn device_options() -> LoginOptions {
        LoginOptions {
            get_device_id: Some(Arc::new(|| DEVICE_ID.to_owned())),
            agent_name: None,
        }
    }

    fn connected_credential() -> OAuthCredential {
        let mut extra = serde_json::Map::new();
        extra.insert("clientId".to_owned(), json!("oaiapp_existing"));
        extra.insert(
            "scopes".to_owned(),
            json!(REQUIRED_SCOPE.split(' ').collect::<Vec<_>>()),
        );
        OAuthCredential {
            refresh: "old-refresh".to_owned(),
            access: "old-access".to_owned(),
            expires: 0,
            extra,
        }
    }

    /// `registers a user-owned client and stores its issued ID and granted scopes`.
    #[tokio::test]
    async fn registers_a_user_owned_client_and_stores_its_issued_id_and_granted_scopes() {
        let mock = MockTokenEndpoint::start(token_response(REQUIRED_SCOPE)).await;
        let oauth = mock.oauth();
        let interaction = TestInteraction::new(Box::new(|prompt, events| {
            assert!(matches!(prompt, AuthPrompt::ManualCode { .. }));
            Ok(interactive_callback_url(events, Some("oaiapp_issued")))
        }));

        let credential = oauth
            .login(&interaction, Some(&device_options()))
            .await
            .expect("login");

        let authorize_url = interaction
            .events()
            .iter()
            .find_map(|event| match event {
                AuthEvent::AuthUrl { url, .. } => Some(url.clone()),
                _ => None,
            })
            .expect("auth url");
        let authorize = url::Url::parse(&authorize_url).expect("authorize");
        let param = |name: &str| {
            authorize
                .query_pairs()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.into_owned())
        };
        assert_eq!(param("client_id").as_deref(), Some("dynamic_agent_client"));
        // ADR-0001 brand rename + #10433 default (upstream "Pi").
        assert_eq!(param("agent_name_hint").as_deref(), Some("rpi"));
        assert_eq!(
            param("ext_agent_host_id").as_deref(),
            Some(&format!("urn:uuid:{DEVICE_ID}")[..])
        );
        assert_eq!(param("scope").as_deref(), Some(REQUIRED_SCOPE));
        assert_eq!(
            param("redirect_uri").as_deref(),
            Some("http://127.0.0.1:1455/auth/callback")
        );
        assert_eq!(
            param("resource").as_deref(),
            Some("https://api.openai.com/v1")
        );
        assert_eq!(param("code_challenge_method").as_deref(), Some("S256"));

        let bodies = mock.bodies();
        assert_eq!(bodies.len(), 1);
        assert_eq!(bodies[0]["client_id"], json!("oaiapp_issued"));
        assert_eq!(bodies[0]["code"], json!("authorization-code"));
        assert_eq!(bodies[0]["resource"], json!("https://api.openai.com/v1"));
        assert!(bodies[0]["code_verifier"].is_string());

        assert_eq!(credential.access, "access-token");
        assert_eq!(credential.refresh, "refresh-token");
        assert_eq!(credential.extra["clientId"], json!("oaiapp_issued"));
        assert_eq!(
            credential.extra["scopes"],
            json!(REQUIRED_SCOPE.split(' ').collect::<Vec<_>>())
        );
    }

    /// `rejects registration without an issued client ID`.
    #[tokio::test]
    async fn rejects_registration_without_an_issued_client_id() {
        let mock = MockTokenEndpoint::start(token_response(REQUIRED_SCOPE)).await;
        let oauth = mock.oauth();
        let interaction = TestInteraction::new(Box::new(|_, events| {
            Ok(interactive_callback_url(events, None))
        }));

        let error = oauth
            .login(&interaction, Some(&device_options()))
            .await
            .expect_err("missing client id");
        assert!(
            error
                .message
                .contains("registration callback did not contain an issued client ID")
        );
        assert!(mock.bodies().is_empty(), "no token exchange attempted");
    }

    /// `rejects a token response that did not grant direct token use`.
    #[tokio::test]
    async fn rejects_a_token_response_that_did_not_grant_direct_token_use() {
        let mock = MockTokenEndpoint::start(token_response(
            "openid profile email offline_access resource.invoke",
        ))
        .await;
        let oauth = mock.oauth();
        let interaction = TestInteraction::new(Box::new(|_, events| {
            Ok(interactive_callback_url(events, Some("oaiapp_issued")))
        }));

        let error = oauth
            .login(&interaction, Some(&device_options()))
            .await
            .expect_err("missing direct scope");
        assert!(
            error
                .message
                .contains("grant did not include chatgpt.tokens.use.direct")
        );
    }

    /// `requires a device ID before starting authorization`.
    #[tokio::test]
    async fn requires_a_device_id_before_starting_authorization() {
        let oauth = OpenAiChatGptOAuth::with_endpoints("http://127.0.0.1:1/token", 0);
        let interaction = TestInteraction::new(Box::new(|_, _| Ok(String::new())));

        let error = oauth
            .login(&interaction, None)
            .await
            .expect_err("no device id");
        assert!(error.message.contains("requires a device ID"));
        let non_uuid = LoginOptions {
            agent_name: None,
            get_device_id: Some(Arc::new(|| "not-a-uuid".to_owned())),
        };
        let error = oauth
            .login(&interaction, Some(&non_uuid))
            .await
            .expect_err("non uuid");
        assert!(error.message.contains("requires a device ID"));
        assert!(
            interaction.events().is_empty(),
            "authorization never started"
        );
    }

    /// `requires refresh responses to rotate the refresh token`.
    #[tokio::test]
    async fn requires_refresh_responses_to_rotate_the_refresh_token() {
        let mut response = token_response(REQUIRED_SCOPE);
        response
            .as_object_mut()
            .expect("object")
            .remove("refresh_token");
        let mock = MockTokenEndpoint::start(response).await;
        let oauth = mock.oauth();

        let error = oauth
            .refresh(&connected_credential(), None)
            .await
            .expect_err("rotation required");
        assert!(
            error
                .message
                .contains("token response has invalid refresh_token")
        );
    }

    /// `refreshes with the credential's issued client ID and stores replacement scopes`.
    #[tokio::test]
    async fn refreshes_with_the_credentials_issued_client_id_and_stores_replacement_scopes() {
        let mut response = token_response(REQUIRED_SCOPE);
        response["access_token"] = json!("new-access");
        response["refresh_token"] = json!("new-refresh");
        let mock = MockTokenEndpoint::start(response).await;
        let oauth = mock.oauth();

        let before = now_ms();
        let credential = oauth
            .refresh(&connected_credential(), None)
            .await
            .expect("refresh");
        assert!(credential.expires >= before + (3600 - 180) * 1000);
        assert!(credential.expires <= now_ms() + (3600 - 180) * 1000);

        let bodies = mock.bodies();
        assert_eq!(bodies.len(), 1);
        assert_eq!(bodies[0]["grant_type"], json!("refresh_token"));
        assert_eq!(bodies[0]["client_id"], json!("oaiapp_existing"));
        assert_eq!(bodies[0]["refresh_token"], json!("old-refresh"));
        assert_eq!(bodies[0]["resource"], json!("https://api.openai.com/v1"));
        assert!(bodies[0].get("scope").is_none());
        assert_eq!(credential.access, "new-access");
        assert_eq!(credential.refresh, "new-refresh");
        assert_eq!(credential.extra["clientId"], json!("oaiapp_existing"));
        assert_eq!(
            credential.extra["scopes"],
            json!(REQUIRED_SCOPE.split(' ').collect::<Vec<_>>())
        );
    }

    /// The loopback callback carries the issued client ID and completes the
    /// exchange before the page is sent.
    #[tokio::test]
    async fn loopback_callback_completes_the_login() {
        let mock = MockTokenEndpoint::start(token_response(REQUIRED_SCOPE)).await;
        let callback_port = free_port();
        let oauth = OpenAiChatGptOAuth::with_endpoints(
            format!("{}/api/accounts/oauth/token", mock.url),
            callback_port,
        );
        let interaction = Arc::new(PendingInteraction::new());
        let interaction_for_task = interaction.clone();
        let task = tokio::spawn(async move {
            let options = device_options();
            oauth
                .login(interaction_for_task.as_ref(), Some(&options))
                .await
        });

        let mut state = None;
        for _ in 0..1000 {
            state = interaction.events().iter().find_map(|event| match event {
                AuthEvent::AuthUrl { url, .. } => url::Url::parse(url).ok().and_then(|parsed| {
                    parsed
                        .query_pairs()
                        .find(|(key, _)| key == "state")
                        .map(|(_, value)| value.into_owned())
                }),
                _ => None,
            });
            if state.is_some() {
                break;
            }
            tokio::task::yield_now().await;
        }
        // The prompt handler never settles on its own, so start the manual
        // answer only after the callback would have won: run the callback
        // first, then let the prompt resolve/cancel.
        let response = reqwest::get(format!(
            "http://127.0.0.1:{callback_port}/auth/callback?code=authorization-code&state={}&client_id=oaiapp_issued",
            state.expect("state")
        ))
        .await
        .expect("callback response");
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response
                .text()
                .await
                .expect("page")
                .contains("ChatGPT authentication completed. You can close this window.")
        );

        let credential = task.await.expect("join").expect("login");
        assert_eq!(credential.access, "access-token");
        assert_eq!(credential.extra["clientId"], json!("oaiapp_issued"));
    }

    /// v0.1.6 review P2-2: aborting the interaction must cancel the manual
    /// prompt too (upstream signals it with
    /// `AbortSignal.any([manualAbort.signal, interaction.signal])`), not
    /// only the later token exchange.
    #[tokio::test]
    async fn interaction_abort_cancels_the_manual_prompt() {
        let mock = MockTokenEndpoint::start(token_response(REQUIRED_SCOPE)).await;
        let callback_port = free_port();
        let oauth = OpenAiChatGptOAuth::with_endpoints(
            format!("{}/api/accounts/oauth/token", mock.url),
            callback_port,
        );
        let interaction = Arc::new(PendingInteraction::new());
        let interaction_for_task = interaction.clone();
        let task = tokio::spawn(async move {
            let options = device_options();
            oauth
                .login(interaction_for_task.as_ref(), Some(&options))
                .await
        });
        // Wait until the login emitted the authorization URL; by then the
        // pending manual prompt owns the prompt signal.
        for _ in 0..1000 {
            if interaction
                .events()
                .iter()
                .any(|event| matches!(event, AuthEvent::AuthUrl { .. }))
            {
                break;
            }
            tokio::task::yield_now().await;
        }
        interaction.signal.cancel();
        let error = tokio::time::timeout(std::time::Duration::from_secs(5), task)
            .await
            .expect("an interaction abort must settle the login")
            .expect("join")
            .expect_err("login cancelled");
        assert!(error.message.contains("Login cancelled"), "{error:?}");
    }

    /// #10265 (`eeac84ca9`): a taken callback port fails before opening the
    /// browser — the callback would otherwise reach the other listener and be
    /// rejected with a state mismatch.
    #[tokio::test]
    async fn occupied_callback_port_fails_before_opening_the_browser() {
        let mock = MockTokenEndpoint::start(token_response(REQUIRED_SCOPE)).await;
        let occupied = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("occupy");
        let callback_port = occupied.local_addr().expect("addr").port();
        let oauth = OpenAiChatGptOAuth::with_endpoints(
            format!("{}/api/accounts/oauth/token", mock.url),
            callback_port,
        );
        // Callback-first interaction: the flow must not reach the prompt.
        let interaction = TestInteraction::new(Box::new(|_, events| {
            Ok(interactive_callback_url(events, Some("oaiapp_issued")))
        }));

        let error = oauth
            .login(&interaction, Some(&device_options()))
            .await
            .expect_err("the taken port must fail the login");
        assert!(
            error.message.contains("is in use"),
            "port-in-use message: {}",
            error.message
        );
        assert!(
            error
                .message
                .contains("another rpi session or by the Codex CLI"),
            "upstream wording: {}",
            error.message
        );
        assert!(
            !interaction
                .events()
                .iter()
                .any(|event| matches!(event, AuthEvent::AuthUrl { .. })),
            "the browser must not be opened"
        );
    }

    /// #10433 (`9ad083102`): `LoginOptions.agentName` replaces the agent name.
    #[tokio::test]
    async fn login_options_agent_name_overrides_the_hint() {
        let mock = MockTokenEndpoint::start(token_response(REQUIRED_SCOPE)).await;
        let oauth = mock.oauth();
        let interaction = TestInteraction::new(Box::new(|_, events| {
            Ok(interactive_callback_url(events, Some("oaiapp_issued")))
        }));
        let options = LoginOptions {
            get_device_id: Some(Arc::new(|| DEVICE_ID.to_owned())),
            agent_name: Some("my-app".to_owned()),
        };

        oauth
            .login(&interaction, Some(&options))
            .await
            .expect("login");
        let authorize_url = interaction
            .events()
            .iter()
            .find_map(|event| match event {
                AuthEvent::AuthUrl { url, .. } => Some(url.clone()),
                _ => None,
            })
            .expect("auth url");
        let authorize = url::Url::parse(&authorize_url).expect("authorize");
        let hint = authorize
            .query_pairs()
            .find(|(key, _)| key == "agent_name_hint")
            .map(|(_, value)| value.into_owned());
        assert_eq!(hint.as_deref(), Some("my-app"));
    }

    /// Manual state mismatches are rejected before any token request.
    #[tokio::test]
    async fn manual_state_mismatch_is_rejected() {
        let mock = MockTokenEndpoint::start(token_response(REQUIRED_SCOPE)).await;
        let oauth = mock.oauth();
        let interaction = TestInteraction::new(Box::new(|_, events| {
            let mut url = url::Url::parse(&interactive_callback_url(events, Some("oaiapp_issued")))
                .expect("url");
            url.query_pairs_mut()
                .clear()
                .append_pair("code", "authorization-code")
                .append_pair("state", "not-the-state")
                .append_pair("client_id", "oaiapp_issued");
            Ok(url.to_string())
        }));
        let error = oauth
            .login(&interaction, Some(&device_options()))
            .await
            .expect_err("state mismatch");
        assert_eq!(error.message, "OAuth state mismatch");
        assert!(mock.bodies().is_empty());
    }

    /// Allocate a currently-free port for the test callback server.
    fn free_port() -> u16 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.local_addr().expect("addr").port()
    }
}
