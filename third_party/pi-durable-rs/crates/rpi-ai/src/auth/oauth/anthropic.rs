//! Port of `packages/ai/src/auth/oauth/anthropic.ts` @ pi 0.82.1 (2efa728) —
//! Anthropic OAuth flow (Claude Pro/Max): PKCE + localhost callback raced
//! against a `manual_code` prompt, token exchange and refresh.
//!
//! Test seams (upstream uses constants + a mocked `fetch`; here the seams are
//! constructor fields, minimal-intrusion):
//! - `token_url`: the mock token endpoint (`TOKEN_URL` upstream constant);
//! - `callback_port`: `0`-style free ports let tests avoid colliding on the
//!   fixed port (`CALLBACK_PORT` upstream constant; upstream tests run
//!   sequentially and bind the real port).
//!
//! Intentional differences:
//! - `formatErrorDetails` approximates the JS `name: message; code=...;
//!   cause=...` chain from a `reqwest::Error` (no `errno`/`stack` fields
//!   exist on the Rust side);
//! - token JSON requires `access_token`/`refresh_token`/`expires_in` —
//!   upstream's `JSON.parse` + cast would tolerate missing fields and produce
//!   `undefined` entries; failing the parse is the faithful-in-spirit choice;
//! - the callback wait is a `tokio::select!` race rather than the upstream
//!   promise-graph (same settle semantics: first outcome cancels the other).

use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;

use super::super::interaction::{AuthEvent, AuthInteraction, AuthPrompt, SelectOption};
use super::super::resolve::{ModelsError, ModelsErrorCode};
use super::super::types::{LoginOptions, ModelAuth, OAuthAuth, OAuthCredential};
use super::callback_server::{
    CallbackOrManual, CompleteFn, ManualPrompt, OAuthCallbackServer, OAuthCallbackServerOptions,
    default_callback_host, wait_for_callback_or_manual_input,
};
use super::pkce::generate_pkce;

/// `CLIENT_ID` — upstream: `decode("OWQxYzI1MGEtZTYxYi00NGQ5LTg4ZWQtNTk0NGQxOTYyZjVl")`
/// (base64/atob of the UUID string below).
const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
/// `AUTHORIZE_URL`.
const AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
/// `TOKEN_URL`.
const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
/// `CALLBACK_PORT`.
const CALLBACK_PORT: u16 = 53692;
/// `CALLBACK_PATH`.
const CALLBACK_PATH: &str = "/callback";
/// `REDIRECT_URI` = `http://localhost:${CALLBACK_PORT}${CALLBACK_PATH}`.
const REDIRECT_URI: &str = "http://localhost:53692/callback";
/// `SCOPES` (verbatim).
const SCOPES: &str = "org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";
/// `COPY_CODE_REDIRECT_URI` — the platform-hosted copy-code callback
/// (`7a11fe1c7`; a non-loopback redirect exception for headless/remote
/// browsers).
const COPY_CODE_REDIRECT_URI: &str = "https://platform.claude.com/oauth/code/callback";
/// `ANTHROPIC_BROWSER_LOGIN_METHOD`.
const LOGIN_METHOD_BROWSER: &str = "browser";
/// `ANTHROPIC_COPY_CODE_LOGIN_METHOD`.
const LOGIN_METHOD_COPY_CODE: &str = "copy_code";
/// `AbortSignal.timeout(30_000)`.
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// `expires = Date.now() + expires_in * 1000 - 5 * 60 * 1000`.
const EXPIRY_SKEW_MS: i64 = 5 * 60 * 1000;

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
pub struct ParsedAuthorizationInput {
    pub code: Option<String>,
    pub state: Option<String>,
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

/// `formatErrorDetails` approximation: `name: message; cause=...` walked from
/// the `std::error::Error::source` chain. JS-specific fields (`code`, `errno`,
/// `stack`) have no reqwest counterpart and are omitted.
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

/// `anthropicOAuth` — the Anthropic OAuth provider auth.
pub fn anthropic_oauth() -> Arc<dyn OAuthAuth> {
    Arc::new(AnthropicOAuth::new())
}

/// Anthropic OAuth (`OAuthAuth`) implementation.
pub struct AnthropicOAuth {
    client: reqwest::Client,
    /// `TOKEN_URL` (test seam — see module docs).
    token_url: String,
    /// `CALLBACK_PORT` (test seam — see module docs).
    callback_port: u16,
}

impl Default for AnthropicOAuth {
    fn default() -> Self {
        Self::new()
    }
}

impl AnthropicOAuth {
    pub fn new() -> Self {
        Self::with_endpoints(TOKEN_URL, CALLBACK_PORT)
    }

    fn with_endpoints(token_url: impl Into<String>, callback_port: u16) -> Self {
        let client = match reqwest::Client::builder().timeout(REQUEST_TIMEOUT).build() {
            Ok(client) => client,
            // Unreachable with this configuration; fall back rather than
            // panic (no unwrap in non-test code).
            Err(_) => reqwest::Client::new(),
        };
        Self {
            client,
            token_url: token_url.into(),
            callback_port,
        }
    }

    /// `postJson` — POST a JSON body, return the raw response text. `Err`
    /// carries the upstream-shaped detail message (`formatErrorDetails`
    /// output, or the `HTTP request failed. ...` message itself).
    async fn post_json(&self, url: &str, body: &Value) -> Result<String, String> {
        let body_text =
            serde_json::to_string(body).map_err(|json_error| format!("Error: {json_error}"))?;
        let response = self
            .client
            .post(url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::ACCEPT, "application/json")
            .body(body_text)
            .send()
            .await
            .map_err(|request_error| format_error_details(&request_error))?;

        let status = response.status();
        let response_body = response
            .text()
            .await
            .map_err(|body_error| format_error_details(&body_error))?;

        if !status.is_success() {
            return Err(format!(
                "HTTP request failed. status={}; url={url}; body={response_body}",
                status.as_u16(),
            ));
        }

        Ok(response_body)
    }

    /// `exchangeAuthorizationCode`.
    async fn exchange_authorization_code(
        &self,
        code: &str,
        state: &str,
        verifier: &str,
        redirect_uri: &str,
    ) -> Result<OAuthCredential, ModelsError> {
        let body = json!({
            "grant_type": "authorization_code",
            "client_id": CLIENT_ID,
            "code": code,
            "state": state,
            "redirect_uri": redirect_uri,
            "code_verifier": verifier,
        });
        let response_body = self.post_json(&self.token_url, &body).await.map_err(|details| {
            error(format!(
                "Token exchange request failed. url={}; redirect_uri={redirect_uri}; response_type=authorization_code; details={details}",
                self.token_url,
            ))
        })?;

        let token_data: TokenResponse =
            serde_json::from_str(&response_body).map_err(|json_error| {
                error(format!(
                    "Token exchange returned invalid JSON. url={}; body={response_body}; details=SyntaxError: {json_error}",
                    self.token_url,
                ))
            })?;

        Ok(OAuthCredential {
            refresh: token_data.refresh_token,
            access: token_data.access_token,
            expires: now_ms() + token_data.expires_in * 1000 - EXPIRY_SKEW_MS,
            extra: Map::new(),
        })
    }

    /// `refreshAnthropicToken`.
    async fn refresh_anthropic_token(
        &self,
        refresh_token: &str,
    ) -> Result<OAuthCredential, ModelsError> {
        let body = json!({
            "grant_type": "refresh_token",
            "client_id": CLIENT_ID,
            "refresh_token": refresh_token,
        });
        let response_body = self
            .post_json(&self.token_url, &body)
            .await
            .map_err(|details| {
                error(format!(
                    "Anthropic token refresh request failed. url={}; details={details}",
                    self.token_url,
                ))
            })?;

        let token_data: TokenResponse =
            serde_json::from_str(&response_body).map_err(|json_error| {
                error(format!(
                    "Anthropic token refresh returned invalid JSON. url={}; body={response_body}; details=SyntaxError: {json_error}",
                    self.token_url,
                ))
            })?;

        Ok(OAuthCredential {
            refresh: token_data.refresh_token,
            access: token_data.access_token,
            expires: now_ms() + token_data.expires_in * 1000 - EXPIRY_SKEW_MS,
            extra: Map::new(),
        })
    }
    /// `loginAnthropic` (`7a11fe1c7`): PKCE → shared callback server
    /// (`expected_state = verifier`, bind failure degrades to pasted input)
    /// → notify `auth_url` → race the `manual_code` prompt against the
    /// callback → token exchange.
    async fn login_browser(
        &self,
        interaction: &dyn AuthInteraction,
    ) -> Result<OAuthCredential, ModelsError> {
        let pkce = generate_pkce();
        let verifier = pkce.verifier;
        let challenge = pkce.challenge;

        // `startOAuthCallbackServer`. #10571 (`8d8ae2fc2`): try the
        // preferred port first so forwarded setups keep working, then let
        // the OS pick a free loopback port when it cannot be bound (e.g.
        // Hyper-V/WSL port exclusions on Windows). Without a callback
        // server, login continues with the pasted redirect URL.
        let complete: CompleteFn<String> = Arc::new(|code| Box::pin(async move { Ok(code) }));
        let start_callback_server = |port: u16| {
            let verifier = verifier.clone();
            let complete = complete.clone();
            let signal = interaction.signal();
            async move {
                OAuthCallbackServer::start(OAuthCallbackServerOptions {
                    provider_name: "Anthropic".to_owned(),
                    host: default_callback_host(),
                    port,
                    path: CALLBACK_PATH.to_owned(),
                    // The advertised redirect uses `localhost` while the bind
                    // host is `RPI_OAUTH_CALLBACK_HOST` (upstream
                    // `redirectHost` seam).
                    redirect_host: Some("localhost".to_owned()),
                    state: Some(verifier),
                    complete,
                    signal,
                    timeout: None,
                })
                .await
            }
        };
        let callback = match start_callback_server(self.callback_port).await {
            Ok(server) => Some(server),
            Err(_) => start_callback_server(0).await.ok(),
        };
        // The authorize URL, paste prompt and token exchange use the bound
        // redirect URI.
        let redirect_uri = callback
            .as_ref()
            .map(OAuthCallbackServer::redirect_uri)
            .unwrap_or_else(|| REDIRECT_URI.to_owned());

        let result = async {
            // `authParams` in upstream key order; `URLSearchParams.toString()`
            // form-urlencodes (space → `+`).
            let query = url::form_urlencoded::Serializer::new(String::new())
                .extend_pairs([
                    ("code", "true"),
                    ("client_id", CLIENT_ID),
                    ("response_type", "code"),
                    ("redirect_uri", redirect_uri.as_str()),
                    ("scope", SCOPES),
                    ("code_challenge", challenge.as_str()),
                    ("code_challenge_method", "S256"),
                    ("state", verifier.as_str()),
                ])
                .finish();
            interaction.notify(AuthEvent::AuthUrl {
                url: format!("{AUTHORIZE_URL}?{query}"),
                instructions: Some(
                    "Complete login in your browser. If the browser is on another machine, paste the final redirect URL here."
                        .to_owned(),
                ),
            });

            let outcome = wait_for_callback_or_manual_input(
                interaction,
                callback.as_ref(),
                ManualPrompt {
                    message: "Complete login in your browser, or paste the authorization code / redirect URL here:"
                        .to_owned(),
                    placeholder: redirect_uri.clone(),
                },
            )
            .await?;
            let (code, state) = match outcome {
                // The shared server already validated `state`; upstream uses
                // the PKCE verifier as the exchange `state` for the callback
                // branch too.
                CallbackOrManual::Callback(code) => (code, verifier.clone()),
                CallbackOrManual::Manual(input) => {
                    finalize_manual_input(parse_authorization_input(&input), &verifier)?
                }
            };

            interaction.notify(AuthEvent::Progress {
                message: "Exchanging authorization code for tokens...".to_owned(),
            });
            self.exchange_authorization_code(&code, &state, &verifier, &redirect_uri)
                .await
        }
        .await;

        // `finally { manualAbort.abort(); callback?.close(); }`
        if let Some(callback) = callback {
            callback.close().await;
        }
        result
    }

    /// `loginAnthropicCopyCode` (`7a11fe1c7`, #10194): the platform-hosted
    /// copy-code redirect for headless/remote browsers. Shares the PKCE and
    /// token-exchange surface with the browser flow; only the callback
    /// channel differs.
    async fn login_copy_code(
        &self,
        interaction: &dyn AuthInteraction,
    ) -> Result<OAuthCredential, ModelsError> {
        let pkce = generate_pkce();
        let verifier = pkce.verifier;
        let challenge = pkce.challenge;
        // `authParams` in upstream key order.
        let query = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs([
                ("code", "true"),
                ("client_id", CLIENT_ID),
                ("response_type", "code"),
                ("redirect_uri", COPY_CODE_REDIRECT_URI),
                ("scope", SCOPES),
                ("code_challenge", challenge.as_str()),
                ("code_challenge_method", "S256"),
                ("state", verifier.as_str()),
            ])
            .finish();
        interaction.notify(AuthEvent::AuthUrl {
            url: format!("{AUTHORIZE_URL}?{query}"),
            instructions: Some(
                "Complete login in your browser, then copy the code Anthropic shows and paste it here."
                    .to_owned(),
            ),
        });

        let input = interaction
            .prompt(AuthPrompt::ManualCode {
                message: "Paste the code Anthropic shows after you sign in:".to_owned(),
                placeholder: Some("code#state".to_owned()),
                signal: interaction.signal(),
            })
            .await?;
        let parsed = parse_authorization_input(&input);
        if let Some(state) = parsed.state.as_deref()
            && !state.is_empty()
            && state != verifier
        {
            return Err(error("OAuth state mismatch"));
        }
        let code = parsed
            .code
            .filter(|code| !code.is_empty())
            .ok_or_else(|| error("Missing authorization code"))?;
        interaction.notify(AuthEvent::Progress {
            message: "Exchanging authorization code for tokens...".to_owned(),
        });
        self.exchange_authorization_code(
            &code,
            parsed.state.as_deref().unwrap_or(&verifier),
            &verifier,
            COPY_CODE_REDIRECT_URI,
        )
        .await
    }
}

/// Token endpoint JSON shape (`{ access_token, refresh_token, expires_in }`;
/// the refresh response's optional `scope` is ignored, as upstream).
#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    expires_in: i64,
}

/// `loginAnthropic` manual-input handling: JS truthiness applied to the
/// parsed pieces (`OAuth state mismatch` / `Missing authorization code` /
/// `Missing OAuth state` verbatim).
fn finalize_manual_input(
    parsed: ParsedAuthorizationInput,
    verifier: &str,
) -> Result<(String, String), ModelsError> {
    if let Some(state) = parsed.state.as_deref()
        && !state.is_empty()
        && state != verifier
    {
        return Err(error("OAuth state mismatch"));
    }
    let code = parsed.code.filter(|code| !code.is_empty());
    let state = parsed.state.or_else(|| Some(verifier.to_owned()));
    let code = code.ok_or_else(|| error("Missing authorization code"))?;
    let state = state
        .filter(|state| !state.is_empty())
        .ok_or_else(|| error("Missing OAuth state"))?;
    Ok((code, state))
}

#[async_trait::async_trait]
impl OAuthAuth for AnthropicOAuth {
    fn name(&self) -> &str {
        "Anthropic (Claude Pro/Max)"
    }

    /// `isSubscription: true` (providers/anthropic.ts:52 @ 4181f66).
    fn is_subscription(&self) -> bool {
        true
    }

    /// `login` — select the Anthropic login method first (`browser` default /
    /// `copy_code`), then run the chosen flow (`7a11fe1c7`).
    async fn login(
        &self,
        interaction: &dyn AuthInteraction,
        _options: Option<&LoginOptions>,
    ) -> Result<OAuthCredential, ModelsError> {
        let method = interaction
            .prompt(AuthPrompt::Select {
                message: "Select Anthropic login method:".to_owned(),
                options: vec![
                    SelectOption {
                        id: LOGIN_METHOD_BROWSER.to_owned(),
                        label: "Browser login (default)".to_owned(),
                        description: None,
                    },
                    SelectOption {
                        id: LOGIN_METHOD_COPY_CODE.to_owned(),
                        label: "Copy code login (headless)".to_owned(),
                        description: None,
                    },
                ],
                signal: None,
            })
            .await?;
        match method.as_str() {
            LOGIN_METHOD_COPY_CODE => self.login_copy_code(interaction).await,
            LOGIN_METHOD_BROWSER => self.login_browser(interaction).await,
            other => Err(error(format!("Unknown Anthropic login method: {other}"))),
        }
    }

    /// `refresh: (credential) => refreshAnthropicToken(credential.refresh)`.
    /// Upstream has no per-request signal here (fetch timeout only); the
    /// trait's `signal` is accepted and ignored, as upstream ignores aborts.
    async fn refresh(
        &self,
        credential: &OAuthCredential,
        _signal: Option<&CancellationToken>,
    ) -> Result<OAuthCredential, ModelsError> {
        self.refresh_anthropic_token(&credential.refresh).await
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
    use std::sync::Mutex;

    use axum::http::StatusCode;
    use axum::response::Json;
    use axum::routing::{Router, post};
    use tokio::sync::oneshot;

    use super::super::super::types::BoxFutureSend;
    use super::*;

    // ----- mock token endpoint (upstream: `vi.stubGlobal("fetch", ...)`) -----

    struct MockTokenEndpoint {
        url: String,
        requests: Arc<Mutex<Vec<Value>>>,
        shutdown: Option<oneshot::Sender<()>>,
    }

    impl MockTokenEndpoint {
        async fn start(status: StatusCode, response: Value) -> Self {
            let requests: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
            let handler_requests = requests.clone();
            let app = Router::new().route(
                "/v1/oauth/token",
                post(move |Json(body): Json<Value>| {
                    let requests = handler_requests.clone();
                    let response = response.clone();
                    async move {
                        requests.lock().expect("lock").push(body);
                        (status, Json(response))
                    }
                }),
            );
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
                url: format!("http://{addr}/v1/oauth/token"),
                requests,
                shutdown: Some(tx),
            }
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

    // ----- fake AuthInteraction -----

    /// Recorded login surface, shared between the test and the prompt
    /// handler closure.
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

        fn manual_signal(&self) -> Option<CancellationToken> {
            self.manual_signals.lock().expect("lock").first().cloned()
        }

        fn auth_url_param(&self, name: &str) -> Option<String> {
            let url = url::Url::parse(&self.auth_url()?).ok()?;
            url.query_pairs()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.into_owned())
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
        /// Answer used for the login-method select (`browser` by default).
        login_method: String,
    }

    impl FakeInteraction {
        fn new(handle: InteractionHandle, on_prompt: PromptHandler) -> Self {
            Self {
                handle,
                on_prompt,
                login_method: LOGIN_METHOD_BROWSER.to_owned(),
            }
        }

        fn with_login_method(
            handle: InteractionHandle,
            login_method: &str,
            on_prompt: PromptHandler,
        ) -> Self {
            Self {
                handle,
                on_prompt,
                login_method: login_method.to_owned(),
            }
        }
    }

    impl AuthInteraction for FakeInteraction {
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
            // The login-method select is answered by the harness; every test
            // closure keeps handling only the flow-specific prompts.
            if matches!(prompt, AuthPrompt::Select { .. }) {
                let login_method = self.login_method.clone();
                return Box::pin(async move { Ok(login_method) });
            }
            (self.on_prompt)(self.handle.clone(), prompt)
        }

        fn notify(&self, event: AuthEvent) {
            self.handle.events.lock().expect("lock").push(event);
        }
    }

    fn token_response(access: &str, refresh: &str) -> Value {
        json!({
            "access_token": access,
            "refresh_token": refresh,
            "expires_in": 3600,
        })
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

    fn oauth_with(endpoint: &MockTokenEndpoint, callback_port: u16) -> AnthropicOAuth {
        AnthropicOAuth::with_endpoints(endpoint.url.clone(), callback_port)
    }

    fn expect_manual_code(prompt: &AuthPrompt) {
        assert!(
            matches!(prompt, AuthPrompt::ManualCode { .. }),
            "unexpected prompt: {prompt:?}"
        );
    }

    /// `parseAuthorizationInput` branches (no direct upstream test file;
    /// anchors the semantics the login tests rely on).
    #[test]
    fn parse_authorization_input_covers_all_branches() {
        // URL
        let parsed = parse_authorization_input(
            "http://localhost:53692/callback?code=url-code&state=url-state",
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

    /// `keeps the localhost redirect_uri for manual callback login`.
    #[tokio::test]
    async fn keeps_the_localhost_redirect_uri_for_manual_callback_login() {
        let endpoint = MockTokenEndpoint::start(
            StatusCode::OK,
            token_response("access-token", "refresh-token"),
        )
        .await;
        let oauth = oauth_with(&endpoint, free_port());
        let handle = InteractionHandle::default();
        let interaction = FakeInteraction::new(
            handle.clone(),
            Box::new(|handle, prompt| {
                Box::pin(async move {
                    expect_manual_code(&prompt);
                    let state = handle.auth_url_param("state").ok_or_else(|| {
                        ModelsError::new(ModelsErrorCode::Auth, "Missing OAuth state in auth URL")
                    })?;
                    let redirect_uri = handle.auth_url_param("redirect_uri").ok_or_else(|| {
                        ModelsError::new(
                            ModelsErrorCode::Auth,
                            "Missing OAuth redirect_uri in auth URL",
                        )
                    })?;
                    Ok(format!("{redirect_uri}?code=manual-code&state={state}"))
                })
            }),
        );

        let credential = oauth.login(&interaction, None).await.expect("login");
        assert_eq!(credential.access, "access-token");
        assert_eq!(credential.refresh, "refresh-token");

        let bodies = endpoint.bodies();
        assert_eq!(bodies.len(), 1, "exactly one token request");
        let body = &bodies[0];
        assert_eq!(body["grant_type"], "authorization_code");
        assert_eq!(body["client_id"], CLIENT_ID);
        assert_eq!(body["code"], "manual-code");
        // #10571: the exchange uses the callback server's bound redirect URI,
        // which the auth URL announced (localhost host + bound port).
        assert_eq!(
            body["redirect_uri"].as_str(),
            handle.auth_url_param("redirect_uri").as_deref()
        );
        assert!(
            body["redirect_uri"]
                .as_str()
                .is_some_and(|uri| uri.starts_with("http://localhost:")),
            "localhost redirect: {body}"
        );
        assert_eq!(
            body["state"].as_str(),
            handle.auth_url_param("state").as_deref()
        );
        assert!(
            body["code_verifier"]
                .as_str()
                .is_some_and(|v| !v.is_empty())
        );
    }

    /// #10571 (`8d8ae2fc2`): when the preferred port cannot be bound, login
    /// falls back to an OS-chosen loopback port and uses that redirect URI.
    #[tokio::test]
    async fn falls_back_to_a_free_port_when_the_preferred_port_is_taken() {
        let endpoint = MockTokenEndpoint::start(
            StatusCode::OK,
            token_response("access-token", "refresh-token"),
        )
        .await;
        // Hold the preferred port for the duration of the test.
        let occupied = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let occupied_port = occupied.local_addr().expect("addr").port();
        let oauth = oauth_with(&endpoint, occupied_port);
        let handle = InteractionHandle::default();
        let interaction = FakeInteraction::new(
            handle.clone(),
            Box::new(|handle, prompt| {
                Box::pin(async move {
                    expect_manual_code(&prompt);
                    let state = handle.auth_url_param("state").ok_or_else(|| {
                        ModelsError::new(ModelsErrorCode::Auth, "Missing OAuth state in auth URL")
                    })?;
                    let redirect_uri = handle.auth_url_param("redirect_uri").ok_or_else(|| {
                        ModelsError::new(
                            ModelsErrorCode::Auth,
                            "Missing OAuth redirect_uri in auth URL",
                        )
                    })?;
                    Ok(format!("{redirect_uri}?code=manual-code&state={state}"))
                })
            }),
        );

        oauth.login(&interaction, None).await.expect("login");
        let redirect = handle
            .auth_url_param("redirect_uri")
            .expect("redirect_uri in the auth URL");
        assert!(
            redirect.starts_with("http://localhost:"),
            "localhost redirect: {redirect}"
        );
        assert!(
            !redirect.contains(&format!(":{occupied_port}/")),
            "must not reuse the taken port: {redirect}"
        );
        let bodies = endpoint.bodies();
        assert_eq!(bodies.len(), 1, "exactly one token request");
        assert_eq!(bodies[0]["redirect_uri"].as_str(), Some(redirect.as_str()));
    }

    /// `omits scope from refresh token requests`.
    #[tokio::test]
    async fn omits_scope_from_refresh_token_requests() {
        let endpoint = MockTokenEndpoint::start(
            StatusCode::OK,
            token_response("new-access-token", "new-refresh-token"),
        )
        .await;
        let oauth = oauth_with(&endpoint, CALLBACK_PORT);
        let credential = OAuthCredential {
            refresh: "refresh-token".to_owned(),
            access: "old-access-token".to_owned(),
            expires: 0,
            extra: Map::new(),
        };

        let refreshed = oauth.refresh(&credential, None).await.expect("refresh");
        assert_eq!(refreshed.access, "new-access-token");
        assert_eq!(refreshed.refresh, "new-refresh-token");

        let bodies = endpoint.bodies();
        assert_eq!(bodies.len(), 1);
        let body = &bodies[0];
        assert_eq!(body["grant_type"], "refresh_token");
        assert!(body["client_id"].as_str().is_some_and(|v| !v.is_empty()));
        assert_eq!(body["refresh_token"], "refresh-token");
        assert!(body.get("scope").is_none(), "refresh must not send scope");
    }

    /// `7a11fe1c7`: the copy-code flow uses the platform redirect and the
    /// pasted `code#state`, sharing PKCE/token exchange with the browser flow.
    #[tokio::test]
    async fn copy_code_uses_the_platform_redirect_and_pasted_code_state() {
        let endpoint =
            MockTokenEndpoint::start(StatusCode::OK, token_response("access", "refresh")).await;
        let oauth = oauth_with(&endpoint, free_port());
        let handle = InteractionHandle::default();
        let interaction = FakeInteraction::with_login_method(
            handle.clone(),
            LOGIN_METHOD_COPY_CODE,
            Box::new(|handle, prompt| {
                Box::pin(async move {
                    expect_manual_code(&prompt);
                    let state = handle.auth_url_param("state").ok_or_else(|| {
                        ModelsError::new(ModelsErrorCode::Auth, "Missing OAuth state in auth URL")
                    })?;
                    Ok(format!("pasted-code#{state}"))
                })
            }),
        );

        let credential = oauth.login(&interaction, None).await.expect("login");
        assert_eq!(credential.access, "access");

        // The authorization URL points at the platform-hosted redirect.
        assert_eq!(
            handle.auth_url_param("redirect_uri").as_deref(),
            Some(COPY_CODE_REDIRECT_URI)
        );
        let prompts = handle.prompts.lock().expect("lock").clone();
        let select = prompts
            .iter()
            .find_map(|prompt| match prompt {
                AuthPrompt::Select { options, .. } => Some(options.clone()),
                _ => None,
            })
            .expect("login-method select");
        assert_eq!(select[0].id, LOGIN_METHOD_BROWSER, "browser is the default");
        assert_eq!(select[1].id, LOGIN_METHOD_COPY_CODE);
        let manual = prompts
            .iter()
            .find_map(|prompt| match prompt {
                AuthPrompt::ManualCode { placeholder, .. } => Some(placeholder.clone()),
                _ => None,
            })
            .expect("manual code prompt");
        assert_eq!(manual.as_deref(), Some("code#state"));

        // Token exchange stays isomorphic with the browser flow except the
        // redirect URI and the state carried through from the paste.
        let bodies = endpoint.bodies();
        assert_eq!(bodies.len(), 1);
        let body = &bodies[0];
        assert_eq!(body["grant_type"], "authorization_code");
        assert_eq!(body["client_id"], CLIENT_ID);
        assert_eq!(body["code"], "pasted-code");
        assert_eq!(body["redirect_uri"], COPY_CODE_REDIRECT_URI);
        assert_eq!(
            body["state"].as_str(),
            handle.auth_url_param("state").as_deref()
        );
        assert!(
            body["code_verifier"]
                .as_str()
                .is_some_and(|v| !v.is_empty())
        );
    }

    /// `7a11fe1c7`: a pasted state that does not match the PKCE verifier is
    /// rejected before any token request.
    #[tokio::test]
    async fn copy_code_rejects_a_state_mismatch() {
        let endpoint =
            MockTokenEndpoint::start(StatusCode::OK, token_response("access", "refresh")).await;
        let oauth = oauth_with(&endpoint, free_port());
        let handle = InteractionHandle::default();
        let interaction = FakeInteraction::with_login_method(
            handle.clone(),
            LOGIN_METHOD_COPY_CODE,
            Box::new(|_handle, prompt| {
                Box::pin(async move {
                    expect_manual_code(&prompt);
                    Ok("pasted-code#not-the-verifier".to_owned())
                })
            }),
        );
        let failure = oauth
            .login(&interaction, None)
            .await
            .expect_err("state mismatch");
        assert_eq!(failure.message, "OAuth state mismatch");
        assert!(endpoint.bodies().is_empty(), "no token exchange attempted");
    }

    /// `7a11fe1c7`: malformed pastes without a code are rejected.
    #[tokio::test]
    async fn copy_code_rejects_a_missing_code() {
        let endpoint =
            MockTokenEndpoint::start(StatusCode::OK, token_response("access", "refresh")).await;
        let oauth = oauth_with(&endpoint, free_port());
        let handle = InteractionHandle::default();
        let interaction = FakeInteraction::with_login_method(
            handle.clone(),
            LOGIN_METHOD_COPY_CODE,
            Box::new(|_handle, prompt| {
                Box::pin(async move {
                    expect_manual_code(&prompt);
                    Ok("   ".to_owned())
                })
            }),
        );
        let failure = oauth
            .login(&interaction, None)
            .await
            .expect_err("missing code");
        assert_eq!(failure.message, "Missing authorization code");
        assert!(endpoint.bodies().is_empty());
    }

    /// An unknown login-method selection fails verbatim.
    #[tokio::test]
    async fn unknown_login_method_fails() {
        let endpoint =
            MockTokenEndpoint::start(StatusCode::OK, token_response("access", "refresh")).await;
        let oauth = oauth_with(&endpoint, free_port());
        let handle = InteractionHandle::default();
        let interaction = FakeInteraction::with_login_method(
            handle,
            "carrier-pigeon",
            Box::new(|_handle, _prompt| {
                Box::pin(async move { Err(ModelsError::new(ModelsErrorCode::Auth, "unused")) })
            }),
        );
        let failure = oauth.login(&interaction, None).await.expect_err("unknown");
        assert_eq!(
            failure.message,
            "Unknown Anthropic login method: carrier-pigeon"
        );
    }

    /// `anthropicOAuth.login resolves through the manual_code prompt and aborts it after settling`.
    #[tokio::test]
    async fn login_resolves_through_the_manual_code_prompt_and_aborts_it_after_settling() {
        let endpoint =
            MockTokenEndpoint::start(StatusCode::OK, token_response("access", "refresh")).await;
        let oauth = oauth_with(&endpoint, free_port());
        let handle = InteractionHandle::default();
        let interaction = FakeInteraction::new(
            handle.clone(),
            Box::new(|_handle, prompt| {
                Box::pin(async move {
                    expect_manual_code(&prompt);
                    Ok("the-code".to_owned())
                })
            }),
        );

        let credential = oauth.login(&interaction, None).await.expect("login");
        assert_eq!(credential.access, "access");
        assert!(
            handle
                .events()
                .iter()
                .any(|event| matches!(event, AuthEvent::AuthUrl { .. }))
        );
        assert!(
            handle
                .prompts
                .lock()
                .expect("lock")
                .iter()
                .any(|prompt| matches!(prompt, AuthPrompt::ManualCode { .. }))
        );
        // The prompt's signal is cancelled once login settles, so UIs can
        // dismiss it.
        assert!(
            handle
                .manual_signal()
                .expect("manual signal")
                .is_cancelled()
        );
    }

    /// Manual input whose `state` does not match the verifier fails verbatim.
    #[tokio::test]
    async fn manual_input_state_mismatch_fails() {
        let endpoint = MockTokenEndpoint::start(StatusCode::OK, token_response("a", "r")).await;
        let oauth = oauth_with(&endpoint, free_port());
        let handle = InteractionHandle::default();
        let interaction = FakeInteraction::new(
            handle.clone(),
            Box::new(|handle, prompt| {
                Box::pin(async move {
                    expect_manual_code(&prompt);
                    let redirect_uri = handle.auth_url_param("redirect_uri").expect("redirect_uri");
                    Ok(format!("{redirect_uri}?code=manual-code&state=wrong-state"))
                })
            }),
        );

        let error = oauth.login(&interaction, None).await.expect_err("mismatch");
        assert_eq!(error.message, "OAuth state mismatch");
        assert!(endpoint.bodies().is_empty(), "no token exchange attempted");
    }

    /// Callback path: the localhost server settles the race and the pending
    /// manual prompt is cancelled.
    #[tokio::test]
    async fn callback_path_completes_and_cancels_the_manual_prompt() {
        let endpoint =
            MockTokenEndpoint::start(StatusCode::OK, token_response("cb-access", "cb-refresh"))
                .await;
        let callback_port = free_port();
        let oauth = oauth_with(&endpoint, callback_port);
        let handle = InteractionHandle::default();
        let interaction = FakeInteraction::new(
            handle.clone(),
            Box::new(|_handle, prompt| {
                Box::pin(async move {
                    let signal = match &prompt {
                        AuthPrompt::ManualCode {
                            signal: Some(signal),
                            ..
                        } => signal.clone(),
                        _ => panic!("unexpected prompt: {prompt:?}"),
                    };
                    // Pend until the flow cancels the prompt.
                    signal.cancelled().await;
                    Err(ModelsError::new(ModelsErrorCode::Oauth, "Login cancelled"))
                })
            }),
        );

        let login = tokio::spawn(async move { oauth.login(&interaction, None).await });

        // Wait for the auth_url notification (carries state = verifier).
        let mut state = None;
        for _ in 0..1000 {
            if let Some(value) = handle.auth_url_param("state") {
                state = Some(value);
                break;
            }
            tokio::task::yield_now().await;
        }
        let state = state.expect("auth_url state");

        let response = reqwest::get(format!(
            "http://127.0.0.1:{callback_port}/callback?code=callback-code&state={state}"
        ))
        .await
        .expect("callback response");
        assert_eq!(response.status(), StatusCode::OK);

        let credential = login.await.expect("join").expect("login");
        assert_eq!(credential.access, "cb-access");
        assert_eq!(credential.refresh, "cb-refresh");

        let bodies = endpoint.bodies();
        assert_eq!(bodies.len(), 1);
        assert_eq!(bodies[0]["code"], "callback-code");
        assert_eq!(bodies[0]["state"].as_str(), Some(state.as_str()));
        assert!(
            handle
                .manual_signal()
                .expect("manual signal")
                .is_cancelled()
        );
    }

    /// A cancelled/failed manual prompt propagates its error.
    #[tokio::test]
    async fn prompt_cancellation_propagates() {
        let endpoint = MockTokenEndpoint::start(StatusCode::OK, token_response("a", "r")).await;
        let oauth = oauth_with(&endpoint, free_port());
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
        assert!(endpoint.bodies().is_empty());
    }

    /// `HTTP request failed. status=...; url=...; body=...` inside the
    /// refresh wrapper message.
    #[tokio::test]
    async fn refresh_http_error_shape_matches_upstream() {
        let endpoint =
            MockTokenEndpoint::start(StatusCode::BAD_REQUEST, Value::String("bad".to_owned()))
                .await;
        let oauth = oauth_with(&endpoint, CALLBACK_PORT);
        let credential = OAuthCredential {
            refresh: "refresh-token".to_owned(),
            access: "old".to_owned(),
            expires: 0,
            extra: Map::new(),
        };

        let error = oauth
            .refresh(&credential, None)
            .await
            .expect_err("http 400");
        assert!(
            error.message.starts_with(&format!(
                "Anthropic token refresh request failed. url={}; details=HTTP request failed. status=400; url={}; body=",
                endpoint.url, endpoint.url
            )),
            "unexpected message: {}",
            error.message
        );
    }

    /// `... returned invalid JSON. url=...; body=...; details=...` shape.
    #[tokio::test]
    async fn refresh_invalid_json_shape_matches_upstream() {
        let endpoint = MockTokenEndpoint::start(
            StatusCode::OK,
            Value::String("not the token json".to_owned()),
        )
        .await;
        let oauth = oauth_with(&endpoint, CALLBACK_PORT);
        let credential = OAuthCredential {
            refresh: "refresh-token".to_owned(),
            access: "old".to_owned(),
            expires: 0,
            extra: Map::new(),
        };

        let error = oauth
            .refresh(&credential, None)
            .await
            .expect_err("bad json");
        assert!(
            error.message.starts_with(&format!(
                "Anthropic token refresh returned invalid JSON. url={}; body=",
                endpoint.url
            )),
            "unexpected message: {}",
            error.message
        );
        assert!(error.message.contains("details=SyntaxError: "));
    }

    /// `toAuth` maps the access token to `api_key`.
    #[tokio::test]
    async fn to_auth_maps_access_to_api_key() {
        let oauth = AnthropicOAuth::new();
        let credential = OAuthCredential {
            refresh: "r".to_owned(),
            access: "sk-ant-oat01-token".to_owned(),
            expires: i64::MAX,
            extra: Map::new(),
        };
        let auth = oauth.to_auth(&credential).await.expect("to_auth");
        assert_eq!(auth.api_key.as_deref(), Some("sk-ant-oat01-token"));
    }
}
