//! Port of `packages/ai/src/auth/oauth/openai-codex.ts` @ pi 0.82.1
//! (2efa728) — OpenAI Codex (ChatGPT Plus/Pro) OAuth: PKCE + fixed-port
//! localhost callback raced against a `manual_code` prompt, the device-code
//! bypass against OpenAI's private deviceauth endpoints
//! (`/api/accounts/deviceauth/usercode|token`, verification URI
//! `/codex/device`), and `refresh_token` refresh.
//!
//! Test seams (upstream stubs the global `fetch` and binds the fixed 1455
//! port; here the seams are constructor fields, minimal-intrusion — same
//! precedent as `anthropic.rs`/`github_copilot.rs`):
//! - `authority`: every `https://auth.openai.com/{path}` URL the flow builds
//!   is rewritten to `http://{authority}/{path}` before dispatch, so a single
//!   loopback mock can stand in for the authorize/token/deviceauth endpoints
//!   (all on the one upstream host);
//! - `callback_port`: free ports let browser-flow tests avoid colliding on
//!   the fixed `CALLBACK_PORT`. `REDIRECT_URI` stays the upstream constant —
//!   it is advertised to the authorization server, while tests drive the
//!   callback at the actual bound port.
//!
//! Intentional differences:
//! - `atob` JWT decoding tries the base64url and standard alphabets with and
//!   without padding (Node `atob` is lenient; real JWTs are base64url);
//! - `crypto.randomBytes(16).toString("hex")` becomes a `ring`-filled hex
//!   string (same 32-char shape);
//! - the upstream `node:http` callback server becomes `axum` (coding-standards
//!   appendix A), reusing the `oauth_page` HTML from `super::callback_page`;
//!   handler branch order and page copy are verbatim; the upstream 500
//!   catch-all has no counterpart (the handler performs no fallible work);
//! - the poll closure's fetch errors surface via
//!   `DeviceCodePollResult::Failed` with the same message text (upstream
//!   rethrows them through the polling framework — same precedent as
//!   `github_copilot.rs`), and a 200 poll response with unparseable JSON
//!   fails with `Invalid OpenAI Codex device auth token response: null`
//!   instead of a raw `SyntaxError`;
//! - `Date.now()` math uses `SystemTime` milliseconds; `interval` parses as
//!   `f64` with JS `Number` semantics (`""` → 0, trim, finite, ≥ 0 required).

use serde_json::{Map, Value, json};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use super::super::interaction::{AuthEvent, AuthInteraction, AuthPrompt, SelectOption};
use super::super::resolve::{ModelsError, ModelsErrorCode};
use super::super::types::{LoginOptions, ModelAuth, OAuthAuth, OAuthCredential};
use super::callback_server::{
    CallbackOrManual, CompleteFn, ManualPrompt, OAuthCallbackServer, OAuthCallbackServerOptions,
    default_callback_host, wait_for_callback_or_manual_input,
};
use super::device_code::{
    CANCEL_MESSAGE, DeviceCodePollOptions, DeviceCodePollResult, poll_oauth_device_code_flow,
};
use super::pkce::generate_pkce;

/// `CLIENT_ID`.
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
/// `AUTH_BASE_URL` — every endpoint URL below hangs off this host.
const AUTH_BASE_URL: &str = "https://auth.openai.com";
/// `AUTHORIZE_URL`.
const AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
/// `TOKEN_URL`.
const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
/// `REDIRECT_URI` = `http://localhost:{CALLBACK_PORT}{CALLBACK_PATH}`.
const REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
/// `DEVICE_USER_CODE_URL` — OpenAI's private deviceauth usercode endpoint.
const DEVICE_USER_CODE_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/usercode";
/// `DEVICE_TOKEN_URL` — OpenAI's private deviceauth token endpoint.
const DEVICE_TOKEN_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/token";
/// `DEVICE_VERIFICATION_URI` — shown to the user for the device-code flow.
const DEVICE_VERIFICATION_URI: &str = "https://auth.openai.com/codex/device";
/// `DEVICE_REDIRECT_URI` — the redirect the deviceauth flow's authorization
/// code is exchanged with.
const DEVICE_REDIRECT_URI: &str = "https://auth.openai.com/deviceauth/callback";
/// `DEVICE_CODE_TIMEOUT_SECONDS`.
const DEVICE_CODE_TIMEOUT_SECONDS: f64 = 15.0 * 60.0;
/// `SCOPE`.
const SCOPE: &str = "openid profile email offline_access";
/// `JWT_CLAIM_PATH` — access-token claim carrying `chatgpt_account_id`.
const JWT_CLAIM_PATH: &str = "https://api.openai.com/auth";
/// `CALLBACK_PORT`.
const CALLBACK_PORT: u16 = 1455;
/// `CALLBACK_PATH`.
const CALLBACK_PATH: &str = "/auth/callback";
/// `OPENAI_CODEX_BROWSER_LOGIN_METHOD`.
const LOGIN_METHOD_BROWSER: &str = "browser";
/// `OPENAI_CODEX_DEVICE_CODE_LOGIN_METHOD`.
const LOGIN_METHOD_DEVICE_CODE: &str = "device_code";
/// Default `originator` (`createAuthorizationFlow(originator = "pi")`).
/// Default `originator` (upstream "pi"; ADR-0001 brand rename). Overridable
/// through `LoginOptions.agentName`.
const ORIGINATOR: &str = "rpi";

fn error(message: impl Into<String>) -> ModelsError {
    ModelsError::new(ModelsErrorCode::Oauth, message.into())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

/// `parseAuthorizationInput` result (`{ code?: string; state?: string }`).
#[derive(Debug, Default, PartialEq, Eq)]
struct ParsedAuthorizationInput {
    code: Option<String>,
    state: Option<String>,
}

/// `parseAuthorizationInput` — four branches: URL / `code#state` / query
/// containing `code=` / bare code. Empty-string values are preserved (JS
/// `searchParams.get` / `split` semantics); callers apply JS truthiness.
fn parse_authorization_input(input: &str) -> ParsedAuthorizationInput {
    let value = input.trim();
    if value.is_empty() {
        return ParsedAuthorizationInput::default();
    }

    if let Ok(url) = url::Url::parse(value) {
        let mut parsed = ParsedAuthorizationInput::default();
        for (key, entry) in url.query_pairs() {
            match key.as_ref() {
                "code" if parsed.code.is_none() => parsed.code = Some(entry.into_owned()),
                "state" if parsed.state.is_none() => parsed.state = Some(entry.into_owned()),
                _ => {}
            }
        }
        return parsed;
    }

    if value.contains('#') {
        let mut parts = value.splitn(2, '#');
        return ParsedAuthorizationInput {
            code: parts.next().map(str::to_owned),
            state: parts.next().map(str::to_owned),
        };
    }

    if value.contains("code=") {
        let mut parsed = ParsedAuthorizationInput::default();
        for (key, entry) in url::form_urlencoded::parse(value.as_bytes()) {
            match key.as_ref() {
                "code" if parsed.code.is_none() => parsed.code = Some(entry.into_owned()),
                "state" if parsed.state.is_none() => parsed.state = Some(entry.into_owned()),
                _ => {}
            }
        }
        return parsed;
    }

    ParsedAuthorizationInput {
        code: Some(value.to_owned()),
        state: None,
    }
}

/// Manual-input handling (`loginOpenAICodex`'s `parseAuthorizationInput`
/// call sites): JS truthiness on `state` (empty passes) and `code` (empty
/// counts as missing).
fn manual_code_from_input(
    input: &str,
    expected_state: &str,
) -> Result<Option<String>, ModelsError> {
    let parsed = parse_authorization_input(input);
    if let Some(state) = parsed.state.as_deref()
        && !state.is_empty()
        && state != expected_state
    {
        return Err(error("State mismatch"));
    }
    Ok(parsed.code.filter(|code| !code.is_empty()))
}

/// Node `atob`-lenient base64 decode: the base64url and standard alphabets,
/// with and without padding.
fn decode_jwt_base64(payload: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    for engine in [
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        &base64::engine::general_purpose::URL_SAFE,
        &base64::engine::general_purpose::STANDARD,
        &base64::engine::general_purpose::STANDARD_NO_PAD,
    ] {
        if let Ok(bytes) = engine.decode(payload) {
            return Some(bytes);
        }
    }
    None
}

/// `decodeJwt` — split on `.`, base64-decode the payload, parse JSON. Needs
/// exactly three dot-separated parts.
fn decode_jwt(token: &str) -> Option<Value> {
    let mut parts = token.split('.');
    let _header = parts.next()?;
    let payload = parts.next()?;
    parts.next()?;
    let bytes = decode_jwt_base64(payload)?;
    serde_json::from_slice(&bytes).ok()
}

/// `getAccountId` — `payload["https://api.openai.com/auth"].chatgpt_account_id`
/// (non-empty string).
fn get_account_id(access_token: &str) -> Option<String> {
    let payload = decode_jwt(access_token)?;
    let account_id = payload.get(JWT_CLAIM_PATH)?.get("chatgpt_account_id")?;
    match account_id.as_str() {
        Some(account_id) if !account_id.is_empty() => Some(account_id.to_owned()),
        _ => None,
    }
}

/// `OAuthToken`.
struct OAuthToken {
    access: String,
    refresh: String,
    expires: i64,
}

/// `credentialsFromToken` — `accountId` extracted from the access token's
/// JWT claim, stored on the credential extras.
fn credentials_from_token(token: OAuthToken) -> Result<OAuthCredential, ModelsError> {
    let account_id = get_account_id(&token.access)
        .ok_or_else(|| error("Failed to extract accountId from token"))?;
    let mut extra = Map::new();
    extra.insert("accountId".to_owned(), Value::String(account_id));
    Ok(OAuthCredential {
        refresh: token.refresh,
        access: token.access,
        expires: token.expires,
        extra,
    })
}

/// `readTokenResponse` — non-2xx:
/// `OpenAI Codex token {operation} failed ({status}): {text || statusText}`;
/// the three fields (`access_token`, `refresh_token`, numeric `expires_in`)
/// must be present — upstream's unchecked cast would produce `undefined`
/// entries, failing the parse is the faithful-in-spirit choice (same
/// precedent as `anthropic.rs`).
async fn read_token_response(
    response: reqwest::Response,
    operation: &str,
) -> Result<OAuthToken, ModelsError> {
    let status = response.status();
    if !status.is_success() {
        let text = response.text().await.unwrap_or_default();
        let status_text = status.canonical_reason().unwrap_or("");
        return Err(error(format!(
            "OpenAI Codex token {operation} failed ({}): {}",
            status.as_u16(),
            if text.is_empty() {
                status_text
            } else {
                text.as_str()
            },
        )));
    }
    let json: Value = response
        .json()
        .await
        .map_err(|json_error| error(json_error.to_string()))?;
    let access_token = json.get("access_token").and_then(Value::as_str);
    let refresh_token = json.get("refresh_token").and_then(Value::as_str);
    let expires_in = json.get("expires_in").and_then(Value::as_f64);
    let (Some(access_token), Some(refresh_token), Some(expires_in)) =
        (access_token, refresh_token, expires_in)
    else {
        return Err(error(format!(
            "OpenAI Codex token {operation} response missing fields: {json}"
        )));
    };
    Ok(OAuthToken {
        access: access_token.to_owned(),
        refresh: refresh_token.to_owned(),
        expires: now_ms() + (expires_in * 1000.0) as i64,
    })
}

/// `DeviceAuthInfo`.
#[derive(Debug, Clone)]
struct DeviceAuthInfo {
    device_auth_id: String,
    user_code: String,
    interval_seconds: f64,
}

/// `DeviceTokenSuccess`.
#[derive(Debug, Clone)]
struct DeviceTokenSuccess {
    authorization_code: String,
    code_verifier: String,
}

/// Map one deviceauth poll response onto the framework result (the body of
/// `pollOpenAICodexDeviceAuth`'s `poll` closure): 403/404 and the
/// `deviceauth_authorization_pending` error stay pending, `slow_down` slows
/// the poll, anything else fails with the status and body text.
fn poll_response_result(status: u16, body: &str) -> DeviceCodePollResult<DeviceTokenSuccess> {
    if (200..300).contains(&status) {
        let json: Value = serde_json::from_str(body).unwrap_or(Value::Null);
        let authorization_code = json.get("authorization_code").and_then(Value::as_str);
        let code_verifier = json.get("code_verifier").and_then(Value::as_str);
        let (Some(authorization_code), Some(code_verifier)) = (authorization_code, code_verifier)
        else {
            return DeviceCodePollResult::Failed {
                message: format!("Invalid OpenAI Codex device auth token response: {json}"),
            };
        };
        return DeviceCodePollResult::Complete {
            value: DeviceTokenSuccess {
                authorization_code: authorization_code.to_owned(),
                code_verifier: code_verifier.to_owned(),
            },
        };
    }

    if status == 403 || status == 404 {
        return DeviceCodePollResult::Pending;
    }

    // `error` is either a string code or `{ code }` (upstream
    // `typeof error === "object" ? error?.code : error`).
    let error_code = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|json| json.get("error").cloned())
        .and_then(|error| match error {
            Value::String(code) => Some(code),
            Value::Object(object) => object
                .get("code")
                .and_then(Value::as_str)
                .map(str::to_owned),
            _ => None,
        });
    if error_code.as_deref() == Some("deviceauth_authorization_pending") {
        return DeviceCodePollResult::Pending;
    }
    if error_code.as_deref() == Some("slow_down") {
        return DeviceCodePollResult::SlowDown {
            interval_seconds: None,
        };
    }

    DeviceCodePollResult::Failed {
        message: if body.is_empty() {
            format!("OpenAI Codex device auth failed with status {status}")
        } else {
            format!("OpenAI Codex device auth failed with status {status}: {body}")
        },
    }
}

/// `fetchWithLoginCancellation` — select the request against the login
/// signal; a cancelled signal yields `Login cancelled`.
async fn send_with_signal(
    request: reqwest::RequestBuilder,
    signal: Option<&CancellationToken>,
) -> Result<reqwest::Response, ModelsError> {
    let send = request.send();
    match signal {
        Some(token) => tokio::select! {
            () = token.cancelled() => Err(error(CANCEL_MESSAGE)),
            response = send => response.map_err(|request_error| error(request_error.to_string())),
        },
        None => send
            .await
            .map_err(|request_error| error(request_error.to_string())),
    }
}

/// `startOpenAICodexDeviceAuth` — POST `{client_id}` to the private
/// deviceauth usercode endpoint; a 404 means the server does not enable
/// device-code login.
async fn start_device_auth(
    client: &reqwest::Client,
    authority: &Option<String>,
    signal: Option<&CancellationToken>,
) -> Result<DeviceAuthInfo, ModelsError> {
    let body = json!({ "client_id": CLIENT_ID }).to_string();
    let response = send_with_signal(
        client
            .post(rewrite_url(authority, DEVICE_USER_CODE_URL))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body),
        signal,
    )
    .await?;
    let status = response.status();
    if !status.is_success() {
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(error(
                "OpenAI Codex device code login is not enabled for this server. Use browser login or verify the server URL.",
            ));
        }
        let response_body = response.text().await.unwrap_or_default();
        let suffix = if response_body.is_empty() {
            String::new()
        } else {
            format!(": {response_body}")
        };
        return Err(error(format!(
            "OpenAI Codex device code request failed with status {}{suffix}",
            status.as_u16(),
        )));
    }
    let json: Value = response
        .json()
        .await
        .map_err(|json_error| error(json_error.to_string()))?;

    // `interval` may be a number or a numeric string (`Number(...)`).
    let interval = match json.get("interval") {
        Some(Value::String(interval)) => {
            let trimmed = interval.trim();
            if trimmed.is_empty() {
                Some(0.0) // `Number("") === 0`
            } else {
                trimmed.parse::<f64>().ok()
            }
        }
        Some(value) => value.as_f64(),
        None => None,
    };
    let device_auth_id = json.get("device_auth_id").and_then(Value::as_str);
    let user_code = json.get("user_code").and_then(Value::as_str);
    // JS falsy semantics: empty strings are missing; the interval must be a
    // finite non-negative number.
    let (Some(device_auth_id), Some(user_code), Some(interval)) =
        (device_auth_id, user_code, interval)
    else {
        return Err(error(format!(
            "Invalid OpenAI Codex device code response: {json}"
        )));
    };
    if device_auth_id.is_empty() || user_code.is_empty() || !interval.is_finite() || interval < 0.0
    {
        return Err(error(format!(
            "Invalid OpenAI Codex device code response: {json}"
        )));
    }
    Ok(DeviceAuthInfo {
        device_auth_id: device_auth_id.to_owned(),
        user_code: user_code.to_owned(),
        interval_seconds: interval,
    })
}

/// `createState` — 16 random bytes as hex (`randomBytes(16).toString("hex")`).
fn create_state() -> Result<String, ModelsError> {
    use ring::rand::SecureRandom;
    let rng = ring::rand::SystemRandom::new();
    let mut bytes = [0u8; 16];
    rng.fill(&mut bytes)
        .map_err(|_| error("Failed to generate random OAuth state"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// `createAuthorizationFlow` result.
struct AuthorizationFlow {
    verifier: String,
    state: String,
    url: String,
}

/// `createAuthorizationFlow` — PKCE + state; the authorize URL carries
/// `id_token_add_organizations`, `codex_cli_simplified_flow` and the
/// `originator` (default `pi`), in upstream key order.
fn create_authorization_flow(originator: &str) -> Result<AuthorizationFlow, ModelsError> {
    let pkce = generate_pkce();
    let state = create_state()?;
    let query = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs([
            ("response_type", "code"),
            ("client_id", CLIENT_ID),
            ("redirect_uri", REDIRECT_URI),
            ("scope", SCOPE),
            ("code_challenge", pkce.challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("state", state.as_str()),
            ("id_token_add_organizations", "true"),
            ("codex_cli_simplified_flow", "true"),
            ("originator", originator),
        ])
        .finish();
    Ok(AuthorizationFlow {
        verifier: pkce.verifier,
        state,
        url: format!("{AUTHORIZE_URL}?{query}"),
    })
}

/// Apply the URL-rewriting test seam: `https://auth.openai.com/{path}` →
/// `http://{authority}/{path}`. Identity in production.
fn rewrite_url(authority: &Option<String>, url: &str) -> String {
    match authority {
        Some(authority) => url.replacen(AUTH_BASE_URL, &format!("http://{authority}"), 1),
        None => url.to_owned(),
    }
}

/// `exchangeAuthorizationCode` — form-encoded POST to `TOKEN_URL`.
async fn exchange_authorization_code(
    client: &reqwest::Client,
    authority: &Option<String>,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
    signal: Option<&CancellationToken>,
) -> Result<OAuthToken, ModelsError> {
    let response = send_with_signal(
        client
            .post(rewrite_url(authority, TOKEN_URL))
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .form(&[
                ("grant_type", "authorization_code"),
                ("client_id", CLIENT_ID),
                ("code", code),
                ("code_verifier", verifier),
                ("redirect_uri", redirect_uri),
            ]),
        signal,
    )
    .await?;
    read_token_response(response, "exchange").await
}

/// `refreshOpenAICodexToken` — `refresh_token` grant; no signal or timeout
/// (upstream plain `fetch` with the message-wrapped error).
async fn refresh_codex_token(
    client: &reqwest::Client,
    authority: &Option<String>,
    refresh_token: &str,
) -> Result<OAuthCredential, ModelsError> {
    let send = client
        .post(rewrite_url(authority, TOKEN_URL))
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", CLIENT_ID),
        ])
        .send();
    let response = match send.await {
        Ok(response) => response,
        Err(request_error) => {
            return Err(error(format!(
                "OpenAI Codex token refresh error: {request_error}"
            )));
        }
    };
    let token = read_token_response(response, "refresh").await?;
    credentials_from_token(token)
}

/// `openaiCodexOAuth` — the OpenAI Codex OAuth provider auth.
pub fn openai_codex_oauth() -> Arc<dyn OAuthAuth> {
    Arc::new(OpenAiCodexOAuth::new())
}

/// OpenAI Codex OAuth (`OAuthAuth`) implementation.
pub struct OpenAiCodexOAuth {
    client: reqwest::Client,
    /// Loopback authority for the URL-rewriting test seam (see module docs).
    authority: Option<String>,
    /// `CALLBACK_PORT` (test seam — see module docs).
    callback_port: u16,
}

impl Default for OpenAiCodexOAuth {
    fn default() -> Self {
        Self::new()
    }
}

impl OpenAiCodexOAuth {
    pub fn new() -> Self {
        Self::with_endpoints(None, CALLBACK_PORT)
    }

    fn with_endpoints(authority: Option<String>, callback_port: u16) -> Self {
        Self {
            client: reqwest::Client::new(),
            authority,
            callback_port,
        }
    }

    /// `loginOpenAICodexDeviceCode` — deviceauth usercode → notify
    /// `device_code` → poll the deviceauth token endpoint → exchange the
    /// authorization code with the device redirect URI.
    async fn login_device_code(
        &self,
        interaction: &dyn AuthInteraction,
    ) -> Result<OAuthCredential, ModelsError> {
        let signal = interaction.signal();
        let device = start_device_auth(&self.client, &self.authority, signal.as_ref()).await?;
        interaction.notify(AuthEvent::DeviceCode {
            user_code: device.user_code.clone(),
            verification_uri: DEVICE_VERIFICATION_URI.to_owned(),
            interval_seconds: Some(device.interval_seconds as u64),
            expires_in_seconds: Some(DEVICE_CODE_TIMEOUT_SECONDS as u64),
        });

        let success =
            poll_device_auth(&self.client, &self.authority, &device, signal.clone()).await?;
        let token = exchange_authorization_code(
            &self.client,
            &self.authority,
            &success.authorization_code,
            &success.code_verifier,
            DEVICE_REDIRECT_URI,
            signal.as_ref(),
        )
        .await?;
        credentials_from_token(token)
    }

    /// `loginOpenAICodex` — PKCE → shared callback server → notify
    /// `auth_url` → race the `manual_code` prompt against the callback →
    /// exchange.
    async fn login_browser(
        &self,
        interaction: &dyn AuthInteraction,
        options: Option<&LoginOptions>,
    ) -> Result<OAuthCredential, ModelsError> {
        let originator = options
            .and_then(|options| options.agent_name.as_deref())
            .unwrap_or(ORIGINATOR);
        let flow = create_authorization_flow(originator)?;
        // `startOAuthCallbackServer({...}).catch(() => undefined)`: port 1455
        // is shared with the Codex CLI, so a bind failure falls back to the
        // pasted redirect URL (R2.6.2).
        let complete: CompleteFn<String> = Arc::new(|code| Box::pin(async move { Ok(code) }));
        let callback = OAuthCallbackServer::start(OAuthCallbackServerOptions {
            provider_name: "OpenAI".to_owned(),
            host: default_callback_host(),
            port: self.callback_port,
            path: CALLBACK_PATH.to_owned(),
            // The advertised redirect uses `localhost` while the bind host is
            // `RPI_OAUTH_CALLBACK_HOST` (upstream `redirectHost` seam).
            redirect_host: Some("localhost".to_owned()),
            state: Some(flow.state.clone()),
            complete,
            signal: interaction.signal(),
            timeout: None,
        })
        .await
        .ok();

        interaction.notify(AuthEvent::AuthUrl {
            url: flow.url.clone(),
            instructions: Some(
                "A browser window should open. Complete login to finish.".to_owned(),
            ),
        });

        let result = async {
            let outcome = wait_for_callback_or_manual_input(
                interaction,
                callback.as_ref(),
                ManualPrompt {
                    message: "Complete login in your browser, or paste the authorization code / redirect URL here:"
                        .to_owned(),
                    placeholder: REDIRECT_URI.to_owned(),
                },
            )
            .await?;
            let code = match outcome {
                CallbackOrManual::Callback(code) => code,
                CallbackOrManual::Manual(input) => {
                    manual_code_from_input(&input, &flow.state)?
                        .ok_or_else(|| error("Missing authorization code"))?
                }
            };

            interaction.notify(AuthEvent::Progress {
                message: "Exchanging authorization code for tokens...".to_owned(),
            });
            let token = exchange_authorization_code(
                &self.client,
                &self.authority,
                &code,
                &flow.verifier,
                REDIRECT_URI,
                interaction.signal().as_ref(),
            )
            .await?;
            credentials_from_token(token)
        }
        .await;

        // `finally { manualAbort.abort(); server.close(); }`
        if let Some(callback) = callback {
            callback.close().await;
        }
        result
    }
}

/// `pollOpenAICodexDeviceAuth` — poll the deviceauth token endpoint; the
/// 15-minute timeout and the server-provided interval ride the shared
/// device-code framework.
async fn poll_device_auth(
    client: &reqwest::Client,
    authority: &Option<String>,
    device: &DeviceAuthInfo,
    signal: Option<CancellationToken>,
) -> Result<DeviceTokenSuccess, ModelsError> {
    poll_oauth_device_code_flow(DeviceCodePollOptions {
        interval_seconds: Some(device.interval_seconds),
        expires_in_seconds: Some(DEVICE_CODE_TIMEOUT_SECONDS),
        wait_before_first_poll: false,
        signal: signal.clone(),
        poll: || {
            let device = device.clone();
            let signal = signal.clone();
            async move {
                let body = json!({
                    "device_auth_id": device.device_auth_id,
                    "user_code": device.user_code,
                })
                .to_string();
                let response = match send_with_signal(
                    client
                        .post(rewrite_url(authority, DEVICE_TOKEN_URL))
                        .header(reqwest::header::CONTENT_TYPE, "application/json")
                        .body(body),
                    signal.as_ref(),
                )
                .await
                {
                    Ok(response) => response,
                    // Upstream lets the fetch error propagate out of the poll
                    // closure; the framework surface here is `Failed` (same
                    // message text).
                    Err(fetch_error) => {
                        return DeviceCodePollResult::Failed {
                            message: fetch_error.message,
                        };
                    }
                };
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                poll_response_result(status.as_u16(), &body)
            }
        },
    })
    .await
}

#[async_trait::async_trait]
impl OAuthAuth for OpenAiCodexOAuth {
    fn name(&self) -> &str {
        "OpenAI (ChatGPT Plus/Pro)"
    }

    /// `isSubscription: true` (providers/openai-codex.ts:15 @ 4181f66).
    fn is_subscription(&self) -> bool {
        true
    }

    /// `login` — select the login method first (`browser` default /
    /// `device_code`), then run the chosen flow.
    async fn login(
        &self,
        interaction: &dyn AuthInteraction,
        options: Option<&LoginOptions>,
    ) -> Result<OAuthCredential, ModelsError> {
        let method = interaction
            .prompt(AuthPrompt::Select {
                message: "Select OpenAI Codex login method:".to_owned(),
                options: vec![
                    SelectOption {
                        id: LOGIN_METHOD_BROWSER.to_owned(),
                        label: "Browser login (default)".to_owned(),
                        description: None,
                    },
                    SelectOption {
                        id: LOGIN_METHOD_DEVICE_CODE.to_owned(),
                        label: "Device code login (headless)".to_owned(),
                        description: None,
                    },
                ],
                signal: None,
            })
            .await?;
        match method.as_str() {
            LOGIN_METHOD_DEVICE_CODE => self.login_device_code(interaction).await,
            LOGIN_METHOD_BROWSER => self.login_browser(interaction, options).await,
            other => Err(error(format!("Unknown OpenAI Codex login method: {other}"))),
        }
    }

    /// `refresh: (credential) => refreshOpenAICodexToken(credential.refresh)`.
    /// Upstream has no per-request signal here; the trait's `signal` is
    /// accepted and ignored.
    async fn refresh(
        &self,
        credential: &OAuthCredential,
        _signal: Option<&CancellationToken>,
    ) -> Result<OAuthCredential, ModelsError> {
        refresh_codex_token(&self.client, &self.authority, &credential.refresh).await
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
    //! Test intents ported from `packages/ai/test/openai-codex-oauth.test.ts`
    //! @ pi 0.82.1 (2efa728); the mocked `fetch` becomes a loopback axum
    //! server behind the `authority` rewrite seam (module docs), and the
    //! fake-timer timing/interval assertions stay in `device_code.rs`'s
    //! fake-clock tests (the flow-level checks here assert the
    //! request/response mapping with a 1s interval instead).

    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::response::Json;
    use base64::Engine;
    use serde_json::json;
    use tokio::sync::oneshot;

    use super::super::super::types::BoxFutureSend;
    use super::*;

    // ----- mock OpenAI endpoints (upstream: `vi.stubGlobal("fetch")`) -----

    #[derive(Debug, Clone)]
    struct RecordedRequest {
        method: String,
        path: String,
        body: String,
    }

    impl RecordedRequest {
        /// `JSON.parse(String(init?.body))`.
        fn json_body(&self) -> Value {
            serde_json::from_str(&self.body).expect("json body")
        }

        /// `new URLSearchParams(String(init?.body))` → `form.get(name)`.
        fn form_get(&self, name: &str) -> Option<String> {
            url::form_urlencoded::parse(self.body.as_bytes())
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.into_owned())
        }
    }

    type Responder = Arc<dyn Fn(&RecordedRequest) -> (StatusCode, Value) + Send + Sync + 'static>;

    struct MockOpenAi {
        authority: String,
        requests: Arc<Mutex<Vec<RecordedRequest>>>,
        shutdown: Option<oneshot::Sender<()>>,
    }

    struct MockState {
        requests: Arc<Mutex<Vec<RecordedRequest>>>,
        responder: Responder,
    }

    impl MockOpenAi {
        async fn start(responder: Responder) -> Self {
            let requests: Arc<Mutex<Vec<RecordedRequest>>> = Arc::new(Mutex::new(Vec::new()));
            let state = Arc::new(MockState {
                requests: requests.clone(),
                responder,
            });
            let app = axum::Router::new().fallback(move |request: Request<Body>| {
                let state = state.clone();
                async move {
                    let recorded = RecordedRequest {
                        method: request.method().to_string(),
                        path: request.uri().path().to_owned(),
                        body: axum::body::to_bytes(request.into_body(), usize::MAX)
                            .await
                            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                            .unwrap_or_default(),
                    };
                    state.requests.lock().expect("lock").push(recorded.clone());
                    let (status, body) = (state.responder)(&recorded);
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

        fn oauth(&self) -> OpenAiCodexOAuth {
            OpenAiCodexOAuth::with_endpoints(Some(self.authority.clone()), free_port())
        }

        fn requests(&self) -> Vec<RecordedRequest> {
            self.requests.lock().expect("lock").clone()
        }

        fn requests_matching(&self, needle: &str) -> Vec<RecordedRequest> {
            self.requests()
                .into_iter()
                .filter(|request| request.path.contains(needle))
                .collect()
        }
    }

    impl Drop for MockOpenAi {
        fn drop(&mut self) {
            if let Some(shutdown) = self.shutdown.take() {
                let _ = shutdown.send(());
            }
        }
    }

    // ----- fake AuthInteraction -----

    #[derive(Clone, Default)]
    struct InteractionHandle {
        events: Arc<Mutex<Vec<AuthEvent>>>,
        prompts: Arc<Mutex<Vec<AuthPrompt>>>,
        manual_signals: Arc<Mutex<Vec<CancellationToken>>>,
    }

    impl InteractionHandle {
        fn events(&self) -> Vec<AuthEvent> {
            self.events.lock().expect("lock").clone()
        }

        fn auth_url(&self) -> Option<String> {
            self.events().into_iter().find_map(|event| match event {
                AuthEvent::AuthUrl { url, .. } => Some(url),
                _ => None,
            })
        }

        fn device_code_event(&self) -> Option<AuthEvent> {
            self.events()
                .into_iter()
                .find(|event| matches!(event, AuthEvent::DeviceCode { .. }))
        }

        fn auth_url_param(&self, name: &str) -> Option<String> {
            let url = url::Url::parse(&self.auth_url()?).ok()?;
            url.query_pairs()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.into_owned())
        }

        fn manual_signal(&self) -> Option<CancellationToken> {
            self.manual_signals.lock().expect("lock").first().cloned()
        }
    }

    type PromptHandler = Box<
        dyn Fn(InteractionHandle, AuthPrompt) -> BoxFutureSend<'static, Result<String, ModelsError>>
            + Send
            + Sync,
    >;

    struct FakeInteraction {
        handle: InteractionHandle,
        on_prompt: PromptHandler,
        signal: Option<CancellationToken>,
    }

    impl FakeInteraction {
        fn new(handle: InteractionHandle, on_prompt: PromptHandler) -> Self {
            Self {
                handle,
                on_prompt,
                signal: None,
            }
        }

        fn with_signal(mut self, signal: CancellationToken) -> Self {
            self.signal = Some(signal);
            self
        }

        /// Select answers `select_answer`; `manual_code` answers
        /// `manual_answer` (or errors with `manual_error`).
        fn scripted(
            handle: InteractionHandle,
            select_answer: &'static str,
            manual_answer: &'static str,
        ) -> Self {
            Self::new(
                handle,
                Box::new(move |_handle, prompt| {
                    Box::pin(async move {
                        match prompt {
                            AuthPrompt::Select { .. } => Ok(select_answer.to_owned()),
                            AuthPrompt::ManualCode { .. } => Ok(manual_answer.to_owned()),
                            _ => panic!("unexpected prompt: {prompt:?}"),
                        }
                    })
                }),
            )
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
            if let AuthPrompt::ManualCode {
                signal: Some(signal),
                ..
            } = &prompt
            {
                self.handle
                    .manual_signals
                    .lock()
                    .expect("lock")
                    .push(signal.clone());
            }
            self.handle
                .prompts
                .lock()
                .expect("lock")
                .push(prompt.clone());
            (self.on_prompt)(self.handle.clone(), prompt)
        }

        fn notify(&self, event: AuthEvent) {
            self.handle.events.lock().expect("lock").push(event);
        }
    }

    // ----- canned upstream response bodies -----

    /// `createAccessToken` — header/payload base64url, `.signature` tail.
    fn access_token(account_id: &str) -> String {
        let encode = |value: &Value| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(serde_json::to_string(value).expect("json"))
        };
        format!(
            "{}.{}.signature",
            encode(&json!({ "alg": "none" })),
            encode(&json!({ (JWT_CLAIM_PATH): { "chatgpt_account_id": account_id } })),
        )
    }

    fn token_response(access: &str) -> Value {
        json!({
            "access_token": access,
            "refresh_token": "refresh-token",
            "expires_in": 3600,
        })
    }

    /// Upstream `deviceAuthPendingResponse()` (403 +
    /// `deviceauth_authorization_pending`).
    fn device_auth_pending_response() -> (StatusCode, Value) {
        (
            StatusCode::FORBIDDEN,
            json!({
                "error": {
                    "message": "Device authorization is pending. Please try again.",
                    "type": "invalid_request_error",
                    "param": null,
                    "code": "deviceauth_authorization_pending",
                },
            }),
        )
    }

    fn device_auth_success_response() -> (StatusCode, Value) {
        (
            StatusCode::OK,
            json!({
                "authorization_code": "oauth-code",
                "code_challenge": "device-code-challenge",
                "code_verifier": "device-code-verifier",
            }),
        )
    }

    /// Pick a free port for the callback server (bind-then-drop race is
    /// acceptable in tests).
    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .expect("bind")
            .local_addr()
            .expect("addr")
            .port()
    }

    fn expect_manual_code(prompt: &AuthPrompt) {
        assert!(
            matches!(prompt, AuthPrompt::ManualCode { .. }),
            "unexpected prompt: {prompt:?}"
        );
    }

    /// Wrap a `manual_code`-only handler with the leading login-method
    /// selection: the flow prompts `select` before the browser flow starts.
    fn browser_flow(
        manual: impl Fn(
            InteractionHandle,
            AuthPrompt,
        ) -> BoxFutureSend<'static, Result<String, ModelsError>>
        + Send
        + Sync
        + 'static,
    ) -> PromptHandler {
        Box::new(move |handle, prompt| match prompt {
            AuthPrompt::Select { .. } => Box::pin(async move { Ok("browser".to_owned()) }),
            AuthPrompt::ManualCode { .. } => manual(handle, prompt),
            _ => panic!("unexpected prompt: {prompt:?}"),
        })
    }

    // ----- pure-function tests -----

    /// `parseAuthorizationInput` branches (no direct upstream test file;
    /// anchors the semantics the login tests rely on).
    #[test]
    fn parse_authorization_input_covers_all_branches() {
        // URL
        let parsed = parse_authorization_input(
            "http://localhost:1455/auth/callback?code=url-code&state=url-state",
        );
        assert_eq!(parsed.code.as_deref(), Some("url-code"));
        assert_eq!(parsed.state.as_deref(), Some("url-state"));

        // code#state
        let parsed = parse_authorization_input("hash-code#hash-state");
        assert_eq!(parsed.code.as_deref(), Some("hash-code"));
        assert_eq!(parsed.state.as_deref(), Some("hash-state"));

        // query containing `code=`
        let parsed = parse_authorization_input("state=query-state&code=query-code");
        assert_eq!(parsed.code.as_deref(), Some("query-code"));
        assert_eq!(parsed.state.as_deref(), Some("query-state"));

        // bare code (+ trim)
        let parsed = parse_authorization_input("  bare-code  ");
        assert_eq!(parsed.code.as_deref(), Some("bare-code"));
        assert_eq!(parsed.state, None);

        // empty
        assert_eq!(
            parse_authorization_input("   "),
            ParsedAuthorizationInput::default()
        );
    }

    /// `createAuthorizationFlow` — verbatim query params incl.
    /// `id_token_add_organizations` / `codex_cli_simplified_flow` /
    /// `originator`.
    #[test]
    fn create_authorization_flow_carries_upstream_query_params() {
        let flow = create_authorization_flow("pi").expect("flow");
        assert!(flow.url.starts_with(&format!("{AUTHORIZE_URL}?")));
        let url = url::Url::parse(&flow.url).expect("url");
        let param = |name: &str| {
            url.query_pairs()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.into_owned())
        };
        assert_eq!(param("response_type").as_deref(), Some("code"));
        assert_eq!(param("client_id").as_deref(), Some(CLIENT_ID));
        assert_eq!(param("redirect_uri").as_deref(), Some(REDIRECT_URI));
        assert_eq!(param("scope").as_deref(), Some(SCOPE));
        assert_eq!(param("code_challenge_method").as_deref(), Some("S256"));
        assert_eq!(param("id_token_add_organizations").as_deref(), Some("true"));
        assert_eq!(param("codex_cli_simplified_flow").as_deref(), Some("true"));
        assert_eq!(param("originator").as_deref(), Some("pi"));

        let challenge = param("code_challenge").expect("challenge");
        let state = param("state").expect("state");
        assert_eq!(challenge.len(), 43);
        // `randomBytes(16).toString("hex")` → 32 hex chars.
        assert_eq!(state.len(), 32);
        assert!(state.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(flow.state, state);
        assert_eq!(flow.verifier.len(), 43);

        // A custom originator is honored (upstream default parameter).
        let flow = create_authorization_flow("custom-originator").expect("flow");
        let url = url::Url::parse(&flow.url).expect("url");
        assert_eq!(
            url.query_pairs()
                .find(|(key, _)| key == "originator")
                .map(|(_, value)| value.into_owned())
                .as_deref(),
            Some("custom-originator")
        );
    }

    /// `decodeJwt` / `getAccountId` — claim extraction, missing/empty/garbage
    /// tokens, and the standard-alphabet (padded) atob input.
    #[test]
    fn get_account_id_extracts_the_jwt_claim() {
        assert_eq!(
            get_account_id(&access_token("account-123")).as_deref(),
            Some("account-123")
        );
        // Empty account id counts as missing (`length > 0`).
        assert_eq!(get_account_id(&access_token("")), None);
        // Missing claim.
        let missing = {
            let encode = |value: &Value| {
                base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .encode(serde_json::to_string(value).expect("json"))
            };
            format!(
                "{}.{}.signature",
                encode(&json!({ "alg": "none" })),
                encode(&json!({ "sub": "no-claim" })),
            )
        };
        assert_eq!(get_account_id(&missing), None);
        // Garbage / not a JWT.
        assert_eq!(get_account_id("not-a-jwt"), None);
        assert_eq!(get_account_id("a.b"), None);
        // Node `atob` input shape: standard base64 with padding.
        let standard = {
            let encode = |value: &Value| {
                base64::engine::general_purpose::STANDARD
                    .encode(serde_json::to_string(value).expect("json"))
            };
            format!(
                "{}.{}.signature",
                encode(&json!({ "alg": "none" })),
                encode(&json!({ (JWT_CLAIM_PATH): { "chatgpt_account_id": "padded" } })),
            )
        };
        assert_eq!(get_account_id(&standard).as_deref(), Some("padded"));
    }

    /// `credentialsFromToken` requires the accountId claim; the credential
    /// carries it in the extras.
    #[test]
    fn credentials_from_token_requires_account_id() {
        let credential = credentials_from_token(OAuthToken {
            access: access_token("account-456"),
            refresh: "refresh-token".to_owned(),
            expires: 1234,
        })
        .expect("credential");
        assert_eq!(credential.access, access_token("account-456"));
        assert_eq!(credential.refresh, "refresh-token");
        assert_eq!(credential.expires, 1234);
        assert_eq!(
            credential.extra.get("accountId"),
            Some(&json!("account-456"))
        );

        let error = credentials_from_token(OAuthToken {
            access: "no-claim-token".to_owned(),
            refresh: "r".to_owned(),
            expires: 0,
        })
        .expect_err("no account id");
        assert_eq!(error.message, "Failed to extract accountId from token");
    }

    /// The poll response mapping: complete / pending (403, 404,
    /// `deviceauth_authorization_pending`) / `slow_down` / failed with the
    /// status and body.
    #[test]
    fn poll_response_result_mapping() {
        let complete =
            poll_response_result(200, r#"{"authorization_code":"c","code_verifier":"v"}"#);
        match complete {
            DeviceCodePollResult::Complete { value } => {
                assert_eq!(value.authorization_code, "c");
                assert_eq!(value.code_verifier, "v");
            }
            _ => panic!("expected complete"),
        }

        // 200 but missing fields.
        let missing = poll_response_result(200, r#"{"authorization_code":"c"}"#);
        assert!(matches!(missing, DeviceCodePollResult::Failed { .. }));

        // 403 and 404 are pending regardless of the body.
        assert!(matches!(
            poll_response_result(403, r#"{"error":"access_denied"}"#),
            DeviceCodePollResult::Pending
        ));
        assert!(matches!(
            poll_response_result(404, "not ready"),
            DeviceCodePollResult::Pending
        ));

        // String and object error codes.
        assert!(matches!(
            poll_response_result(400, r#"{"error":"deviceauth_authorization_pending"}"#),
            DeviceCodePollResult::Pending
        ));
        assert!(matches!(
            poll_response_result(
                400,
                r#"{"error":{"code":"deviceauth_authorization_pending"}}"#
            ),
            DeviceCodePollResult::Pending
        ));
        assert!(matches!(
            poll_response_result(400, r#"{"error":"slow_down"}"#),
            DeviceCodePollResult::SlowDown { .. }
        ));

        // Unrecognized status → failed with the verbatim status + body.
        let failed = poll_response_result(
            500,
            r#"{"error":"server_error","error_description":"try again later"}"#,
        );
        match failed {
            DeviceCodePollResult::Failed { message } => assert_eq!(
                message,
                "OpenAI Codex device auth failed with status 500: {\"error\":\"server_error\",\"error_description\":\"try again later\"}"
            ),
            _ => panic!("expected failed"),
        }
        let failed = poll_response_result(500, "");
        match failed {
            DeviceCodePollResult::Failed { message } => {
                assert_eq!(message, "OpenAI Codex device auth failed with status 500");
            }
            _ => panic!("expected failed"),
        }
    }

    // ----- flow tests against the mock -----

    /// `logs in with the OpenAI Codex device code flow` — usercode → device
    /// event → pending poll → complete poll → exchange with the device
    /// redirect URI → credential with `accountId`.
    #[tokio::test]
    async fn device_login_happy_path_polls_and_exchanges() {
        let access = access_token("account-123");
        let access_for_responder = access.clone();
        let poll_count = Arc::new(AtomicUsize::new(0));
        let responder_polls = poll_count.clone();
        let responder: Responder =
            Arc::new(
                move |request: &RecordedRequest| match request.path.as_str() {
                    "/api/accounts/deviceauth/usercode" => {
                        assert_eq!(request.method, "POST");
                        assert_eq!(request.json_body(), json!({ "client_id": CLIENT_ID }));
                        (
                            StatusCode::OK,
                            json!({
                                "device_auth_id": "device-auth-id",
                                "user_code": "ABCD-1234",
                                "interval": "1",
                            }),
                        )
                    }
                    "/api/accounts/deviceauth/token" => {
                        assert_eq!(request.method, "POST");
                        assert_eq!(
                            request.json_body(),
                            json!({
                                "device_auth_id": "device-auth-id",
                                "user_code": "ABCD-1234",
                            })
                        );
                        if responder_polls.fetch_add(1, Ordering::SeqCst) == 0 {
                            device_auth_pending_response()
                        } else {
                            device_auth_success_response()
                        }
                    }
                    "/oauth/token" => {
                        assert_eq!(request.method, "POST");
                        assert_eq!(
                            request.form_get("grant_type").as_deref(),
                            Some("authorization_code")
                        );
                        assert_eq!(request.form_get("client_id").as_deref(), Some(CLIENT_ID));
                        assert_eq!(request.form_get("code").as_deref(), Some("oauth-code"));
                        assert_eq!(
                            request.form_get("redirect_uri").as_deref(),
                            Some(DEVICE_REDIRECT_URI)
                        );
                        assert_eq!(
                            request.form_get("code_verifier").as_deref(),
                            Some("device-code-verifier")
                        );
                        (StatusCode::OK, token_response(&access_for_responder))
                    }
                    other => panic!("unexpected request: {other}"),
                },
            );
        let mock = MockOpenAi::start(responder).await;
        let oauth = mock.oauth();
        let handle = InteractionHandle::default();
        let interaction = FakeInteraction::scripted(handle.clone(), "device_code", "");

        let credential = oauth.login(&interaction, None).await.expect("login");
        assert_eq!(credential.access, access);
        assert_eq!(credential.refresh, "refresh-token");
        assert_eq!(
            credential.extra.get("accountId"),
            Some(&json!("account-123"))
        );
        let skew = credential.expires - now_ms();
        assert!((skew - 3600 * 1000).abs() < 3000, "expires skew: {skew}");

        // Device-code event (openai-codex.ts:429-434).
        assert_eq!(
            handle.device_code_event(),
            Some(AuthEvent::DeviceCode {
                user_code: "ABCD-1234".to_owned(),
                verification_uri: DEVICE_VERIFICATION_URI.to_owned(),
                interval_seconds: Some(1),
                expires_in_seconds: Some(900),
            })
        );
        assert_eq!(poll_count.load(Ordering::SeqCst), 2);

        // Request order: usercode → poll → poll → token exchange.
        let requests = mock.requests();
        let paths: Vec<&str> = requests
            .iter()
            .map(|request| request.path.as_str())
            .collect();
        assert_eq!(
            paths,
            [
                "/api/accounts/deviceauth/usercode",
                "/api/accounts/deviceauth/token",
                "/api/accounts/deviceauth/token",
                "/oauth/token",
            ]
        );
    }

    /// `offers browser login first and uses the selected OpenAI Codex device
    /// code flow` — select prompt shape verbatim; no auth_url event.
    #[tokio::test]
    async fn offers_browser_login_first_and_uses_the_selected_device_code_flow() {
        let access = access_token("account-456");
        let responder: Responder =
            Arc::new(
                move |request: &RecordedRequest| match request.path.as_str() {
                    "/api/accounts/deviceauth/usercode" => {
                        assert_eq!(request.json_body(), json!({ "client_id": CLIENT_ID }));
                        (
                            StatusCode::OK,
                            json!({
                                "device_auth_id": "device-auth-id",
                                "user_code": "WXYZ-7890",
                                "interval": "5",
                            }),
                        )
                    }
                    "/api/accounts/deviceauth/token" => device_auth_success_response(),
                    "/oauth/token" => (StatusCode::OK, token_response(&access)),
                    other => panic!("unexpected request: {other}"),
                },
            );
        let mock = MockOpenAi::start(responder).await;
        let oauth = mock.oauth();
        let handle = InteractionHandle::default();
        let interaction = FakeInteraction::scripted(handle.clone(), "device_code", "");

        let credential = oauth.login(&interaction, None).await.expect("login");
        assert_eq!(
            credential.extra.get("accountId"),
            Some(&json!("account-456"))
        );
        assert!(
            handle
                .events()
                .iter()
                .all(|event| !matches!(event, AuthEvent::AuthUrl { .. })),
            "browser login must not start"
        );

        // Select prompt shape (openai-codex.ts:516-521).
        let prompts = handle.prompts.lock().expect("lock");
        assert_eq!(prompts.len(), 1);
        match &prompts[0] {
            AuthPrompt::Select {
                message,
                options,
                signal: None,
            } => {
                assert_eq!(message, "Select OpenAI Codex login method:");
                assert_eq!(
                    *options,
                    vec![
                        SelectOption {
                            id: "browser".to_owned(),
                            label: "Browser login (default)".to_owned(),
                            description: None,
                        },
                        SelectOption {
                            id: "device_code".to_owned(),
                            label: "Device code login (headless)".to_owned(),
                            description: None,
                        },
                    ]
                );
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
        assert_eq!(
            handle.device_code_event(),
            Some(AuthEvent::DeviceCode {
                user_code: "WXYZ-7890".to_owned(),
                verification_uri: DEVICE_VERIFICATION_URI.to_owned(),
                interval_seconds: Some(5),
                expires_in_seconds: Some(900),
            })
        );
    }

    /// `cancels when OpenAI Codex login method selection is cancelled`.
    #[tokio::test]
    async fn cancelled_method_selection_propagates() {
        let oauth = OpenAiCodexOAuth::new();
        let handle = InteractionHandle::default();
        let interaction = FakeInteraction::new(
            handle,
            Box::new(|_handle, _prompt| {
                Box::pin(
                    async move { Err(ModelsError::new(ModelsErrorCode::Oauth, "Login cancelled")) },
                )
            }),
        );

        let error = oauth
            .login(&interaction, None)
            .await
            .expect_err("cancelled");
        assert_eq!(error.message, "Login cancelled");
    }

    /// `cancels the OpenAI Codex device code flow while waiting`.
    #[tokio::test]
    async fn cancels_the_device_code_flow_while_waiting() {
        let responder: Responder =
            Arc::new(|request: &RecordedRequest| match request.path.as_str() {
                "/api/accounts/deviceauth/usercode" => (
                    StatusCode::OK,
                    json!({
                        "device_auth_id": "device-auth-id",
                        "user_code": "ABCD-1234",
                        "interval": "1",
                    }),
                ),
                "/api/accounts/deviceauth/token" => device_auth_pending_response(),
                other => panic!("unexpected request: {other}"),
            });
        let mock = MockOpenAi::start(responder).await;
        let oauth = mock.oauth();
        let handle = InteractionHandle::default();
        let signal = CancellationToken::new();
        let interaction = FakeInteraction::scripted(handle.clone(), "device_code", "")
            .with_signal(signal.clone());

        let login = tokio::spawn(async move { oauth.login(&interaction, None).await });
        // Wait until the first poll landed, then cancel.
        for _ in 0..1000 {
            if mock
                .requests_matching("/api/accounts/deviceauth/token")
                .len()
                == 1
            {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(
            mock.requests_matching("/api/accounts/deviceauth/token")
                .len(),
            1
        );
        signal.cancel();

        let error = login.await.expect("join").expect_err("cancelled");
        assert_eq!(error.message, "Login cancelled");
    }

    /// `treats OpenAI Codex device auth 403 and 404 responses as pending`.
    #[tokio::test]
    async fn treats_403_and_404_polls_as_pending() {
        let access = access_token("account-403-404");
        let poll_count = Arc::new(AtomicUsize::new(0));
        let responder_polls = poll_count.clone();
        let responder: Responder =
            Arc::new(
                move |request: &RecordedRequest| match request.path.as_str() {
                    "/api/accounts/deviceauth/usercode" => (
                        StatusCode::OK,
                        json!({
                            "device_auth_id": "device-auth-id",
                            "user_code": "ABCD-1234",
                            "interval": "1",
                        }),
                    ),
                    "/api/accounts/deviceauth/token" => {
                        match responder_polls.fetch_add(1, Ordering::SeqCst) {
                            0 => (
                                StatusCode::FORBIDDEN,
                                json!({ "error": "access_denied", "error_description": "denied" }),
                            ),
                            1 => (StatusCode::NOT_FOUND, Value::Null),
                            _ => device_auth_success_response(),
                        }
                    }
                    "/oauth/token" => (StatusCode::OK, token_response(&access)),
                    other => panic!("unexpected request: {other}"),
                },
            );
        let mock = MockOpenAi::start(responder).await;
        let oauth = mock.oauth();
        let handle = InteractionHandle::default();
        let interaction = FakeInteraction::scripted(handle.clone(), "device_code", "");

        let credential = oauth.login(&interaction, None).await.expect("login");
        assert_eq!(
            credential.extra.get("accountId"),
            Some(&json!("account-403-404"))
        );
        assert_eq!(poll_count.load(Ordering::SeqCst), 3);
    }

    /// `includes the response body in OpenAI Codex device auth poll
    /// failures`.
    #[tokio::test]
    async fn includes_the_response_body_in_device_auth_poll_failures() {
        let responder: Responder =
            Arc::new(|request: &RecordedRequest| match request.path.as_str() {
                "/api/accounts/deviceauth/usercode" => (
                    StatusCode::OK,
                    json!({
                        "device_auth_id": "device-auth-id",
                        "user_code": "ABCD-1234",
                        "interval": "5",
                    }),
                ),
                "/api/accounts/deviceauth/token" => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    json!({ "error": "server_error", "error_description": "try again later" }),
                ),
                other => panic!("unexpected request: {other}"),
            });
        let mock = MockOpenAi::start(responder).await;
        let oauth = mock.oauth();
        let handle = InteractionHandle::default();
        let interaction = FakeInteraction::scripted(handle.clone(), "device_code", "");

        let error = oauth.login(&interaction, None).await.expect_err("500");
        assert_eq!(
            error.message,
            "OpenAI Codex device auth failed with status 500: {\"error\":\"server_error\",\"error_description\":\"try again later\"}"
        );
    }

    /// Browser flow via the loopback callback: auth_url params, callback
    /// GET settles the race and cancels the pending manual prompt, exchange
    /// with the localhost redirect URI.
    #[tokio::test]
    async fn browser_login_completes_via_callback() {
        let access = access_token("account-cb");
        let responder: Responder = Arc::new(move |request: &RecordedRequest| {
            assert_eq!(request.path, "/oauth/token");
            (StatusCode::OK, token_response(&access))
        });
        let mock = MockOpenAi::start(responder).await;
        let callback_port = free_port();
        let oauth = OpenAiCodexOAuth::with_endpoints(Some(mock.authority.clone()), callback_port);
        let handle = InteractionHandle::default();
        let interaction = FakeInteraction::new(
            handle.clone(),
            browser_flow(|_handle, prompt| {
                Box::pin(async move {
                    expect_manual_code(&prompt);
                    let signal = match &prompt {
                        AuthPrompt::ManualCode {
                            signal: Some(signal),
                            ..
                        } => signal.clone(),
                        _ => unreachable!("prompt checked above"),
                    };
                    // Pend until the flow cancels the prompt.
                    signal.cancelled().await;
                    Err(ModelsError::new(ModelsErrorCode::Oauth, "Login cancelled"))
                })
            }),
        );
        let login = tokio::spawn(async move { oauth.login(&interaction, None).await });

        // Wait for the auth_url notification, then hit the callback.
        let mut state = None;
        for _ in 0..1000 {
            if let Some(value) = handle.auth_url_param("state") {
                state = Some(value);
                break;
            }
            tokio::task::yield_now().await;
        }
        let state = state.expect("auth_url state");
        // Auth URL carries the codex-specific params.
        assert_eq!(
            handle
                .auth_url_param("id_token_add_organizations")
                .as_deref(),
            Some("true")
        );
        assert_eq!(
            handle
                .auth_url_param("codex_cli_simplified_flow")
                .as_deref(),
            Some("true")
        );
        // ADR-0001 brand rename + #10433 default (upstream "pi").
        assert_eq!(handle.auth_url_param("originator").as_deref(), Some("rpi"));

        let response = reqwest::get(format!(
            "http://127.0.0.1:{callback_port}{CALLBACK_PATH}?code=callback-code&state={state}"
        ))
        .await
        .expect("callback response");
        assert_eq!(response.status(), StatusCode::OK);

        let credential = login.await.expect("join").expect("login");
        assert_eq!(
            credential.extra.get("accountId"),
            Some(&json!("account-cb"))
        );
        assert!(
            handle
                .manual_signal()
                .expect("manual signal")
                .is_cancelled()
        );

        let bodies = mock.requests_matching("/oauth/token");
        assert_eq!(bodies.len(), 1);
        assert_eq!(bodies[0].form_get("code").as_deref(), Some("callback-code"));
        assert_eq!(
            bodies[0].form_get("redirect_uri").as_deref(),
            Some(REDIRECT_URI)
        );
        assert!(
            bodies[0]
                .form_get("code_verifier")
                .is_some_and(|verifier| !verifier.is_empty())
        );
    }

    /// Browser flow via the manual prompt: a pasted redirect URL with the
    /// matching state is parsed and exchanged.
    #[tokio::test]
    async fn browser_login_completes_via_manual_code() {
        let access = access_token("account-manual");
        let responder: Responder = Arc::new(move |request: &RecordedRequest| {
            assert_eq!(request.path, "/oauth/token");
            (StatusCode::OK, token_response(&access))
        });
        let mock = MockOpenAi::start(responder).await;
        let oauth = mock.oauth();
        let handle = InteractionHandle::default();
        let interaction = FakeInteraction::new(
            handle.clone(),
            browser_flow(|handle, prompt| {
                Box::pin(async move {
                    expect_manual_code(&prompt);
                    let state = handle.auth_url_param("state").ok_or_else(|| {
                        ModelsError::new(ModelsErrorCode::Auth, "Missing OAuth state in auth URL")
                    })?;
                    Ok(format!("{REDIRECT_URI}?code=manual-code&state={state}"))
                })
            }),
        );

        let credential = oauth.login(&interaction, None).await.expect("login");
        assert_eq!(
            credential.extra.get("accountId"),
            Some(&json!("account-manual"))
        );
        let bodies = mock.requests_matching("/oauth/token");
        assert_eq!(bodies.len(), 1);
        assert_eq!(bodies[0].form_get("code").as_deref(), Some("manual-code"));
        assert_eq!(
            bodies[0].form_get("redirect_uri").as_deref(),
            Some(REDIRECT_URI)
        );
    }

    /// Manual input whose `state` does not match fails verbatim, with no
    /// exchange attempted.
    #[tokio::test]
    async fn manual_input_state_mismatch_fails() {
        let mock = MockOpenAi::start(Arc::new(|_request: &RecordedRequest| {
            panic!("no request expected")
        }))
        .await;
        let oauth = mock.oauth();
        let handle = InteractionHandle::default();
        let interaction = FakeInteraction::new(
            handle.clone(),
            browser_flow(|_handle, prompt| {
                Box::pin(async move {
                    expect_manual_code(&prompt);
                    Ok(format!("{REDIRECT_URI}?code=manual-code&state=wrong-state"))
                })
            }),
        );

        let error = oauth.login(&interaction, None).await.expect_err("mismatch");
        assert_eq!(error.message, "State mismatch");
        assert!(mock.requests().is_empty(), "no token exchange attempted");
    }

    /// Empty manual input fails with `Missing authorization code`.
    #[tokio::test]
    async fn missing_manual_code_fails() {
        let mock = MockOpenAi::start(Arc::new(|_request: &RecordedRequest| {
            panic!("no request expected")
        }))
        .await;
        let oauth = mock.oauth();
        let handle = InteractionHandle::default();
        let interaction = FakeInteraction::scripted(handle.clone(), "browser", "");

        let error = oauth.login(&interaction, None).await.expect_err("missing");
        assert_eq!(error.message, "Missing authorization code");
        assert!(mock.requests().is_empty());
    }

    /// `startLocalOAuthServer` bind failure settles the wait with `None` and
    /// the login falls back to the manual prompt (upstream
    /// `server.once("error")` branch).
    #[tokio::test]
    async fn browser_login_falls_back_to_manual_when_callback_port_is_taken() {
        let access = access_token("account-fallback");
        let responder: Responder = Arc::new(move |request: &RecordedRequest| {
            assert_eq!(request.path, "/oauth/token");
            (StatusCode::OK, token_response(&access))
        });
        let mock = MockOpenAi::start(responder).await;
        // Occupy the port the callback server would bind.
        let taken = std::net::TcpListener::bind("127.0.0.1:0")
            .expect("bind")
            .local_addr()
            .expect("addr")
            .port();
        let oauth = OpenAiCodexOAuth::with_endpoints(Some(mock.authority.clone()), taken);
        let handle = InteractionHandle::default();
        let interaction = FakeInteraction::new(
            handle.clone(),
            browser_flow(|handle, prompt| {
                Box::pin(async move {
                    expect_manual_code(&prompt);
                    let state = handle.auth_url_param("state").ok_or_else(|| {
                        ModelsError::new(ModelsErrorCode::Auth, "Missing OAuth state in auth URL")
                    })?;
                    Ok(format!("{REDIRECT_URI}?code=fallback-code&state={state}"))
                })
            }),
        );

        let credential = oauth.login(&interaction, None).await.expect("login");
        assert_eq!(
            credential.extra.get("accountId"),
            Some(&json!("account-fallback"))
        );
        let bodies = mock.requests_matching("/oauth/token");
        assert_eq!(bodies.len(), 1);
        assert_eq!(bodies[0].form_get("code").as_deref(), Some("fallback-code"));
    }

    /// `refresh` — `refresh_token` grant, no scope, new accountId from the
    /// new access token.
    #[tokio::test]
    async fn refresh_exchanges_the_refresh_token() {
        let access = access_token("account-refreshed");
        let access_for_responder = access.clone();
        let responder: Responder = Arc::new(move |request: &RecordedRequest| {
            assert_eq!(request.path, "/oauth/token");
            (StatusCode::OK, token_response(&access_for_responder))
        });
        let mock = MockOpenAi::start(responder).await;
        let oauth = mock.oauth();
        let credential = OAuthCredential {
            refresh: "refresh-token".to_owned(),
            access: "old-access-token".to_owned(),
            expires: 0,
            extra: Map::new(),
        };

        let refreshed = oauth.refresh(&credential, None).await.expect("refresh");
        assert_eq!(refreshed.access, access);
        assert_eq!(refreshed.refresh, "refresh-token");
        assert_eq!(
            refreshed.extra.get("accountId"),
            Some(&json!("account-refreshed"))
        );

        let bodies = mock.requests_matching("/oauth/token");
        assert_eq!(bodies.len(), 1);
        assert_eq!(
            bodies[0].form_get("grant_type").as_deref(),
            Some("refresh_token")
        );
        assert_eq!(
            bodies[0].form_get("refresh_token").as_deref(),
            Some("refresh-token")
        );
        assert_eq!(bodies[0].form_get("client_id").as_deref(), Some(CLIENT_ID));
    }

    /// `does not write token refresh failures to stderr` — the 401 body is
    /// part of the error message (`OpenAI Codex token refresh failed (401)`).
    #[tokio::test]
    async fn refresh_failure_includes_status_and_body() {
        let responder: Responder = Arc::new(|request: &RecordedRequest| {
            assert_eq!(request.path, "/oauth/token");
            (
                StatusCode::UNAUTHORIZED,
                json!({
                    "error": {
                        "message": "Could not validate your token. Please try signing in again.",
                        "type": "invalid_request_error",
                    },
                }),
            )
        });
        let mock = MockOpenAi::start(responder).await;
        let oauth = mock.oauth();
        let credential = OAuthCredential {
            refresh: "invalid-refresh-token".to_owned(),
            access: "invalid-access-token".to_owned(),
            expires: 0,
            extra: Map::new(),
        };

        let error = oauth.refresh(&credential, None).await.expect_err("401");
        assert!(
            error
                .message
                .starts_with("OpenAI Codex token refresh failed (401): "),
            "unexpected message: {}",
            error.message
        );
        assert!(
            error
                .message
                .contains("Could not validate your token. Please try signing in again."),
            "unexpected message: {}",
            error.message
        );
    }

    /// `toAuth` maps the access token to `api_key`.
    #[tokio::test]
    async fn to_auth_maps_access_to_api_key() {
        let oauth = OpenAiCodexOAuth::new();
        let credential = OAuthCredential {
            refresh: "r".to_owned(),
            access: "codex-access-token".to_owned(),
            expires: i64::MAX,
            extra: Map::new(),
        };
        let auth = oauth.to_auth(&credential).await.expect("to_auth");
        assert_eq!(auth.api_key.as_deref(), Some("codex-access-token"));
    }

    /// An unknown login method id fails verbatim.
    #[tokio::test]
    async fn unknown_login_method_fails() {
        let mock = MockOpenAi::start(Arc::new(|_request: &RecordedRequest| {
            panic!("no request expected")
        }))
        .await;
        let oauth = mock.oauth();
        let handle = InteractionHandle::default();
        let interaction = FakeInteraction::scripted(handle.clone(), "other", "");

        let error = oauth.login(&interaction, None).await.expect_err("unknown");
        assert_eq!(error.message, "Unknown OpenAI Codex login method: other");
    }
}
