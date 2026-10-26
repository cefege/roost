//! Port of `packages/ai/src/auth/oauth/openrouter.ts` @ pi 0.82.1 (2efa728)
//! — OpenRouter OAuth: PKCE exchange for a permanent, user-controlled API
//! key rather than an expiring access/refresh token pair (refresh is a
//! no-op). The callback is handled by a one-shot loopback server on an
//! ephemeral port, raced against a `manual_code` prompt so remote/headless
//! sessions can paste the redirect URL when the browser cannot reach the
//! loopback server.
//!
//! Test seams (upstream stubs the global `fetch` and drives the loopback
//! callback with `nativeFetch`; here the seam is a constructor field,
//! minimal-intrusion — same precedent as `anthropic.rs`'s `token_url`):
//! - `token_url`: the mock key-exchange endpoint (`TOKEN_URL` upstream
//!   constant); the callback server already binds an ephemeral port, so no
//!   port seam is needed — tests parse `callback_url` out of the `auth_url`
//!   event and hit it directly.
//!
//! Intentional differences:
//! - `crypto.randomUUID()` becomes a `ring`-generated UUIDv4 (same 36-char
//!   shape; `radius.rs` precedent);
//! - the upstream `node:http` server becomes `axum` (coding-standards
//!   appendix A), reusing the `oauth_page` HTML from `super::callback_page`;
//!   branch order and page copy are verbatim;
//! - the 30s token-exchange timeout and the 5-minute login timeout are
//!   `tokio::time::timeout` races instead of an `AbortController` /
//!   `setTimeout` timer (dropping the request future aborts it);
//! - `expires` maps `Number.MAX_SAFE_INTEGER` to the exact i64 value
//!   9007199254740991;
//! - the `loginLabel` ("Sign in with OpenRouter") has no `OAuthAuth` slot
//!   and is not ported (deviation D-032).

use std::sync::Arc;

use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;

use super::super::interaction::{AuthEvent, AuthInteraction};
use super::super::resolve::{ModelsError, ModelsErrorCode};
use super::super::types::{LoginOptions, ModelAuth, OAuthAuth, OAuthCredential};
use super::callback_server::{
    CallbackOrManual, CompleteFn, ManualPrompt, OAuthCallbackServer, OAuthCallbackServerOptions,
    default_callback_host, wait_for_callback_or_manual_input,
};
use super::device_code::CANCEL_MESSAGE;
use super::pkce::generate_pkce;

/// `AUTHORIZE_URL`.
const AUTHORIZE_URL: &str = "https://openrouter.ai/auth";
/// `TOKEN_URL`.
const TOKEN_URL: &str = "https://openrouter.ai/api/v1/auth/keys";
/// `LOGIN_TIMEOUT_MS`.
const LOGIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5 * 60);
/// `TOKEN_EXCHANGE_TIMEOUT_MS`.
const TOKEN_EXCHANGE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// `Number.MAX_SAFE_INTEGER` — the permanent key never expires.
const EXPIRES_NEVER: i64 = 9_007_199_254_740_991;

fn error(message: impl Into<String>) -> ModelsError {
    ModelsError::new(ModelsErrorCode::Oauth, message.into())
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

/// `parseAuthorizationInput` — code only (OpenRouter has no OAuth state):
/// URL `code` param / query containing `code=` / bare code. Empty input
/// yields `None`.
fn parse_authorization_input(input: &str) -> Option<String> {
    let value = input.trim();
    if value.is_empty() {
        return None;
    }

    if let Ok(url) = url::Url::parse(value) {
        return url
            .query_pairs()
            .find(|(key, _)| key == "code")
            .map(|(_, code)| code.into_owned());
    }

    if value.contains("code=") {
        return url::form_urlencoded::parse(value.as_bytes())
            .find(|(key, _)| key == "code")
            .map(|(_, code)| code.into_owned());
    }

    Some(value.to_owned())
}

/// `errorDetail` — `error_description` / `message` / `error` / nested
/// `error.message` (in that order).
fn error_detail(body: &Value) -> Option<String> {
    if let Some(description) = body.get("error_description").and_then(Value::as_str) {
        return Some(description.to_owned());
    }
    if let Some(message) = body.get("message").and_then(Value::as_str) {
        return Some(message.to_owned());
    }
    if let Some(error) = body.get("error").and_then(Value::as_str) {
        return Some(error.to_owned());
    }
    if let Some(message) = body
        .get("error")
        .and_then(Value::as_object)
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
    {
        return Some(message.to_owned());
    }
    None
}

/// `exchangeAuthorizationCode` — JSON POST of `{ code, code_verifier,
/// code_challenge_method: "S256" }` to the key endpoint; the response's
/// `key` becomes the permanent credential. The 30s exchange timeout and the
/// login signal are raced against the request.
async fn exchange_authorization_code(
    client: &reqwest::Client,
    token_url: &str,
    code: &str,
    verifier: &str,
    signal: Option<&CancellationToken>,
) -> Result<OAuthCredential, ModelsError> {
    // `if (signal?.aborted) throw new Error("Login cancelled")`.
    if signal.is_some_and(|token| token.is_cancelled()) {
        return Err(error(CANCEL_MESSAGE));
    }

    let body = json!({
        "code": code,
        "code_verifier": verifier,
        "code_challenge_method": "S256",
    })
    .to_string();
    let send = async {
        tokio::time::timeout(
            TOKEN_EXCHANGE_TIMEOUT,
            client
                .post(token_url)
                .header(reqwest::header::ACCEPT, "application/json")
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body)
                .send(),
        )
        .await
        .map_err(|_| error("OpenRouter OAuth token exchange timed out"))?
        .map_err(|request_error| error(request_error.to_string()))
    };
    let response = match signal {
        Some(token) => tokio::select! {
            () = token.cancelled() => return Err(error(CANCEL_MESSAGE)),
            response = send => response?,
        },
        None => send.await?,
    };

    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    // `JSON.parse` failure is only fatal on a success status; an unparseable
    // non-2xx body leaves the detail lookup empty (upstream `body = {}`).
    let mut body = Value::Null;
    match serde_json::from_str::<Value>(&text) {
        Ok(value) if value.is_object() => body = value,
        Ok(_) => {}
        Err(_) if status.is_success() => {
            return Err(error("OpenRouter OAuth returned invalid JSON"));
        }
        Err(_) => {}
    }

    if !status.is_success() {
        let detail = error_detail(&body);
        return Err(error(format!(
            "OpenRouter OAuth key exchange failed (HTTP {}){}",
            status.as_u16(),
            detail
                .map(|detail| format!(": {detail}"))
                .unwrap_or_default(),
        )));
    }

    let key = match body.get("key").and_then(Value::as_str) {
        Some(key) if !key.is_empty() => key,
        _ => return Err(error("OpenRouter OAuth response carries no \"key\"")),
    };
    Ok(OAuthCredential {
        refresh: String::new(),
        access: key.to_owned(),
        expires: EXPIRES_NEVER,
        extra: Map::new(),
    })
}

/// `openRouterOAuth` — the OpenRouter OAuth provider auth.
pub fn openrouter_oauth() -> Arc<dyn OAuthAuth> {
    Arc::new(OpenRouterOAuth::new())
}

/// OpenRouter OAuth (`OAuthAuth`) implementation.
pub struct OpenRouterOAuth {
    client: reqwest::Client,
    /// `TOKEN_URL` (test seam — see module docs).
    token_url: String,
}

impl Default for OpenRouterOAuth {
    fn default() -> Self {
        Self::new()
    }
}

impl OpenRouterOAuth {
    pub fn new() -> Self {
        Self::with_token_url(TOKEN_URL)
    }

    fn with_token_url(token_url: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            token_url: token_url.into(),
        }
    }

    /// `exchangeAuthorizationCode` bound to this instance's endpoint.
    async fn exchange(
        &self,
        code: &str,
        verifier: &str,
        signal: Option<&CancellationToken>,
    ) -> Result<OAuthCredential, ModelsError> {
        exchange_authorization_code(&self.client, &self.token_url, code, verifier, signal).await
    }

    /// `loginOpenRouter`.
    async fn login_openrouter(
        &self,
        interaction: &dyn AuthInteraction,
    ) -> Result<OAuthCredential, ModelsError> {
        let pkce = generate_pkce();
        let callback_path = format!("/oauth/callback/{}", random_uuid()?);
        let client = self.client.clone();
        let token_url = self.token_url.clone();
        let verifier = pkce.verifier.clone();
        let signal_for_exchange = interaction.signal();
        let complete: CompleteFn<OAuthCredential> = Arc::new(move |code: String| {
            let client = client.clone();
            let token_url = token_url.clone();
            let verifier = verifier.clone();
            let signal = signal_for_exchange.clone();
            Box::pin(async move {
                exchange_authorization_code(&client, &token_url, &code, &verifier, signal.as_ref())
                    .await
            })
        });
        // `startOAuthCallbackServer` (upstream has no catch): port 0 and a
        // random path make a bind conflict practically impossible; the
        // 5-minute login timeout rides the shared server.
        let callback = OAuthCallbackServer::start(OAuthCallbackServerOptions {
            provider_name: "OpenRouter".to_owned(),
            host: default_callback_host(),
            port: 0,
            path: callback_path,
            redirect_host: None,
            state: None,
            complete,
            signal: interaction.signal(),
            timeout: Some(LOGIN_TIMEOUT),
        })
        .await?;
        let callback_url = callback.redirect_uri();

        let result = async {
            // `authorizeUrl.search = new URLSearchParams({...}).toString()`.
            let query = url::form_urlencoded::Serializer::new(String::new())
                .extend_pairs([
                    ("callback_url", callback_url.as_str()),
                    ("code_challenge", pkce.challenge.as_str()),
                    ("code_challenge_method", "S256"),
                ])
                .finish();
            interaction.notify(AuthEvent::Progress {
                message: format!("Listening for OpenRouter OAuth callback on {callback_url}"),
            });
            interaction.notify(AuthEvent::AuthUrl {
                url: format!("{AUTHORIZE_URL}?{query}"),
                instructions: Some(
                    "Complete sign-in in your browser. If the browser is on another machine, paste the final redirect URL here."
                        .to_owned(),
                ),
            });

            let outcome = wait_for_callback_or_manual_input(
                interaction,
                Some(&callback),
                ManualPrompt {
                    message: "Complete sign-in in your browser, or paste the authorization code / redirect URL here:"
                        .to_owned(),
                    placeholder: callback_url.clone(),
                },
            )
            .await?;
            match outcome {
                CallbackOrManual::Callback(credential) => Ok(credential),
                CallbackOrManual::Manual(input) => {
                    // `if (manualError) throw manualError;` — the helper
                    // already propagated prompt errors.
                    let code = parse_authorization_input(&input)
                        .ok_or_else(|| error("Missing authorization code"))?;
                    interaction.notify(AuthEvent::Progress {
                        message: "Exchanging authorization code for an API key...".to_owned(),
                    });
                    self.exchange(&code, &pkce.verifier, interaction.signal().as_ref())
                        .await
                }
            }
        }
        .await;

        // `finally { manualAbort.abort(); callback.close(); }`
        callback.close().await;
        result
    }
}

#[async_trait::async_trait]
impl OAuthAuth for OpenRouterOAuth {
    fn name(&self) -> &str {
        "OpenRouter OAuth"
    }

    async fn login(
        &self,
        interaction: &dyn AuthInteraction,
        _options: Option<&LoginOptions>,
    ) -> Result<OAuthCredential, ModelsError> {
        self.login_openrouter(interaction).await
    }

    /// `refresh` — no-op: OpenRouter exchanged the authorization code for a
    /// permanent key, so there is nothing to refresh (upstream returns the
    /// credential unchanged).
    async fn refresh(
        &self,
        credential: &OAuthCredential,
        _signal: Option<&CancellationToken>,
    ) -> Result<OAuthCredential, ModelsError> {
        Ok(credential.clone())
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
    //! Test intents ported from `packages/ai/test/openrouter-oauth.test.ts`
    //! @ pi 0.82.1 (2efa728); the mocked `fetch` becomes a loopback axum
    //! token endpoint behind the `token_url` seam (module docs), and the
    //! loopback callback is driven through the `callback_url` advertised in
    //! the `auth_url` event (the server binds an ephemeral port, so no port
    //! seam is needed).

    use std::sync::Mutex;

    use super::super::super::interaction::AuthPrompt;
    use axum::extract::Json;
    use axum::http::StatusCode;
    use axum::routing::post;
    use base64::Engine;
    use serde_json::json;
    use sha2::Digest;
    use tokio::sync::oneshot;

    use super::super::super::types::BoxFutureSend;
    use super::*;

    // ----- mock key-exchange endpoint (upstream: `vi.stubGlobal("fetch")`) -----

    type Responder =
        Arc<dyn Fn(&Value) -> BoxFutureSend<'static, (StatusCode, String)> + Send + Sync + 'static>;

    struct MockTokenEndpoint {
        url: String,
        requests: Arc<Mutex<Vec<Value>>>,
        shutdown: Option<oneshot::Sender<()>>,
    }

    impl MockTokenEndpoint {
        async fn start(responder: Responder) -> Self {
            let requests: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
            let handler_requests = requests.clone();
            let app = axum::Router::new().route(
                "/v1/auth/keys",
                post(move |Json(body): Json<Value>| {
                    let requests = handler_requests.clone();
                    let responder = responder.clone();
                    async move {
                        requests.lock().expect("lock").push(body.clone());
                        let (status, text) = responder(&body).await;
                        let mut response =
                            axum::response::Response::new(axum::body::Body::from(text));
                        *response.status_mut() = status;
                        response
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
                url: format!("http://{addr}/v1/auth/keys"),
                requests,
                shutdown: Some(tx),
            }
        }

        fn oauth(&self) -> OpenRouterOAuth {
            OpenRouterOAuth::with_token_url(self.url.clone())
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

    fn key_response(key: &str) -> String {
        json!({ "key": key }).to_string()
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

        /// The `manual_code` prompt answers `answer` (a `ManualCode` prompt
        /// is the only prompt this flow issues).
        fn scripted_manual(handle: InteractionHandle, answer: &'static str) -> Self {
            Self::new(
                handle,
                Box::new(move |_handle, prompt| {
                    Box::pin(async move {
                        expect_manual_code(&prompt);
                        Ok(answer.to_owned())
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

    fn expect_manual_code(prompt: &AuthPrompt) {
        assert!(
            matches!(prompt, AuthPrompt::ManualCode { .. }),
            "unexpected prompt: {prompt:?}"
        );
    }

    /// Wait until the auth_url notification carries the `callback_url`
    /// parameter; returns the callback URL.
    async fn wait_for_callback_url(handle: &InteractionHandle) -> String {
        for _ in 0..1000 {
            if let Some(callback_url) = handle.auth_url_param("callback_url") {
                return callback_url;
            }
            tokio::task::yield_now().await;
        }
        panic!("no auth_url notification");
    }

    // ----- pure-function tests -----

    /// `parseAuthorizationInput` branches (code only).
    #[test]
    fn parse_authorization_input_covers_all_branches() {
        // URL
        let parsed =
            parse_authorization_input("http://127.0.0.1:45678/oauth/callback/abc?code=url-code");
        assert_eq!(parsed.as_deref(), Some("url-code"));

        // Query string containing `code=`
        let parsed = parse_authorization_input("state=1&code=query-code");
        assert_eq!(parsed.as_deref(), Some("query-code"));

        // Bare code (+ trim)
        let parsed = parse_authorization_input("  bare-code  ");
        assert_eq!(parsed.as_deref(), Some("bare-code"));

        // Empty
        assert_eq!(parse_authorization_input("   "), None);
    }

    /// `errorDetail` precedence: `error_description` / `message` / `error` /
    /// nested `error.message`.
    #[test]
    fn error_detail_precedence() {
        assert_eq!(
            error_detail(&json!({
                "error_description": "desc",
                "message": "msg",
                "error": { "message": "nested" },
            })),
            Some("desc".to_owned())
        );
        assert_eq!(
            error_detail(&json!({ "message": "msg", "error": { "message": "nested" } })),
            Some("msg".to_owned())
        );
        assert_eq!(
            error_detail(&json!({ "error": "plain" })),
            Some("plain".to_owned())
        );
        assert_eq!(
            error_detail(&json!({ "error": { "message": "nested" } })),
            Some("nested".to_owned())
        );
        assert_eq!(error_detail(&json!({ "error": ["not-an-object"] })), None);
        assert_eq!(error_detail(&json!({})), None);
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

    /// The exchange mints a permanent credential (`key`, empty refresh,
    /// `Number.MAX_SAFE_INTEGER` expiry) and posts the PKCE fields.
    #[tokio::test]
    async fn exchange_mints_permanent_key_credential() {
        let mock = MockTokenEndpoint::start(Arc::new(|body: &Value| {
            let body = body.clone();
            Box::pin(async move {
                assert_eq!(
                    body,
                    json!({
                        "code": "the-code",
                        "code_verifier": "the-verifier",
                        "code_challenge_method": "S256",
                    })
                );
                (StatusCode::OK, key_response("sk-or-test"))
            })
        }))
        .await;
        let oauth = mock.oauth();

        let credential = oauth
            .exchange("the-code", "the-verifier", None)
            .await
            .expect("exchange");
        assert_eq!(credential.access, "sk-or-test");
        assert_eq!(credential.refresh, "");
        assert_eq!(credential.expires, EXPIRES_NEVER);
        assert!(credential.extra.is_empty());
    }

    /// `reports token exchange failures ...` — the parsed error detail rides
    /// the failure message.
    #[tokio::test]
    async fn exchange_failure_includes_error_detail() {
        let mock = MockTokenEndpoint::start(Arc::new(|_body: &Value| {
            Box::pin(async move {
                (
                    StatusCode::FORBIDDEN,
                    json!({ "error": { "message": "invalid code" } }).to_string(),
                )
            })
        }))
        .await;
        let oauth = mock.oauth();

        let error = oauth
            .exchange("bad-code", "verifier", None)
            .await
            .expect_err("403");
        assert_eq!(
            error.message,
            "OpenRouter OAuth key exchange failed (HTTP 403): invalid code"
        );
    }

    /// `rejects a successful response that does not contain a key`.
    #[tokio::test]
    async fn exchange_rejects_missing_key() {
        let mock = MockTokenEndpoint::start(Arc::new(|_body: &Value| {
            Box::pin(async move { (StatusCode::OK, json!({ "user_id": "user-1" }).to_string()) })
        }))
        .await;
        let oauth = mock.oauth();

        let error = oauth
            .exchange("code", "verifier", None)
            .await
            .expect_err("no key");
        assert_eq!(
            error.message,
            "OpenRouter OAuth response carries no \"key\""
        );
    }

    /// A success response whose body is not JSON at all errors verbatim
    /// (`JSON.parse` throws upstream).
    #[tokio::test]
    async fn exchange_rejects_invalid_json_on_success() {
        let mock = MockTokenEndpoint::start(Arc::new(|_body: &Value| {
            Box::pin(async move { (StatusCode::OK, "not json".to_owned()) })
        }))
        .await;
        let oauth = mock.oauth();

        let error = oauth
            .exchange("code", "verifier", None)
            .await
            .expect_err("invalid json");
        assert_eq!(error.message, "OpenRouter OAuth returned invalid JSON");
    }

    /// `refresh` is a no-op: the credential is returned unchanged (permanent
    /// key, no network).
    #[tokio::test]
    async fn refresh_is_a_noop() {
        let oauth = OpenRouterOAuth::new();
        let credential = OAuthCredential {
            refresh: String::new(),
            access: "sk-or-permanent".to_owned(),
            expires: EXPIRES_NEVER,
            extra: Map::new(),
        };

        let refreshed = oauth.refresh(&credential, None).await.expect("refresh");
        assert_eq!(refreshed, credential);
    }

    /// `toAuth` maps the permanent key to `api_key`.
    #[tokio::test]
    async fn to_auth_maps_access_to_api_key() {
        let oauth = OpenRouterOAuth::new();
        let credential = OAuthCredential {
            refresh: String::new(),
            access: "sk-or-test".to_owned(),
            expires: EXPIRES_NEVER,
            extra: Map::new(),
        };
        let auth = oauth.to_auth(&credential).await.expect("to_auth");
        assert_eq!(auth.api_key.as_deref(), Some("sk-or-test"));
    }

    // ----- login flow tests -----

    /// `runs PKCE on a one-shot loopback callback and exchanges the code for
    /// a permanent API key` — authorize URL params, callback round trip,
    /// exchange body, S256 challenge cross-check, single exchange, manual
    /// prompt cancelled.
    #[tokio::test]
    async fn login_exchanges_via_the_loopback_callback() {
        let mock = MockTokenEndpoint::start(Arc::new(|_body: &Value| {
            Box::pin(async move { (StatusCode::OK, key_response("sk-or-test")) })
        }))
        .await;
        let oauth = mock.oauth();
        let handle = InteractionHandle::default();
        let interaction = FakeInteraction::new(
            handle.clone(),
            Box::new(|_handle, prompt| {
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
        let callback_url = wait_for_callback_url(&handle).await;

        // Authorize URL shape (openrouter.ts:251-257).
        let authorize_url = url::Url::parse(&handle.auth_url().expect("auth_url")).expect("url");
        let expected_origin = url::Url::parse("https://openrouter.ai")
            .expect("origin url")
            .origin();
        assert_eq!(authorize_url.origin(), expected_origin);
        assert_eq!(authorize_url.path(), "/auth");
        assert_eq!(
            handle.auth_url_param("code_challenge_method").as_deref(),
            Some("S256")
        );
        let challenge = handle.auth_url_param("code_challenge").expect("challenge");

        // Callback URL shape: loopback host, random path.
        let callback = url::Url::parse(&callback_url).expect("callback url");
        assert_eq!(callback.host_str(), Some("127.0.0.1"));
        let path = callback.path().to_owned();
        assert!(
            path.starts_with("/oauth/callback/") && path.len() == "/oauth/callback/".len() + 36,
            "unexpected callback path: {path}"
        );

        let response = reqwest::get(format!("{callback_url}?code=authorization-code"))
            .await
            .expect("callback response");
        assert_eq!(response.status(), StatusCode::OK);

        let credential = login.await.expect("join").expect("login");
        assert_eq!(credential.access, "sk-or-test");
        assert_eq!(credential.refresh, "");
        assert_eq!(credential.expires, EXPIRES_NEVER);
        assert!(
            handle
                .manual_signal()
                .expect("manual signal")
                .is_cancelled()
        );

        // Exchange body + PKCE cross-check (base64url(sha256(verifier))).
        let bodies = mock.bodies();
        assert_eq!(bodies.len(), 1, "exactly one token request");
        assert_eq!(bodies[0]["code"], "authorization-code");
        assert_eq!(bodies[0]["code_challenge_method"], "S256");
        let verifier = bodies[0]["code_verifier"].as_str().expect("verifier");
        let digest = sha2::Sha256::digest(verifier.as_bytes());
        assert_eq!(
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest),
            challenge
        );

        // Progress notification precedes the auth_url.
        let events = handle.events();
        let progress = events
            .iter()
            .find(|event| matches!(event, AuthEvent::Progress { .. }))
            .expect("progress");
        assert!(matches!(progress, AuthEvent::Progress { .. }));
        assert!(
            events
                .iter()
                .position(|event| matches!(event, AuthEvent::Progress { .. }))
                < events
                    .iter()
                    .position(|event| matches!(event, AuthEvent::AuthUrl { .. }))
        );
    }

    /// `mints a key from a pasted redirect URL when the loopback callback
    /// never arrives` — the manual prompt's redirect URL is parsed.
    #[tokio::test]
    async fn login_mints_key_from_pasted_redirect_url() {
        let mock = MockTokenEndpoint::start(Arc::new(|_body: &Value| {
            Box::pin(async move { (StatusCode::OK, key_response("sk-or-manual")) })
        }))
        .await;
        let oauth = mock.oauth();
        let handle = InteractionHandle::default();
        let interaction = FakeInteraction::new(
            handle.clone(),
            Box::new(|handle, prompt| {
                Box::pin(async move {
                    expect_manual_code(&prompt);
                    let callback_url = handle.auth_url_param("callback_url").ok_or_else(|| {
                        ModelsError::new(ModelsErrorCode::Auth, "Missing callback URL in auth URL")
                    })?;
                    Ok(format!("{callback_url}?code=manual-code"))
                })
            }),
        );

        let credential = oauth.login(&interaction, None).await.expect("login");
        assert_eq!(credential.access, "sk-or-manual");
        assert_eq!(credential.refresh, "");
        let bodies = mock.bodies();
        assert_eq!(bodies.len(), 1);
        assert_eq!(bodies[0]["code"], "manual-code");
        assert_eq!(bodies[0]["code_challenge_method"], "S256");
    }

    /// `accepts a bare authorization code from the manual prompt` — trimmed,
    /// no URL shape required.
    #[tokio::test]
    async fn login_accepts_bare_manual_code() {
        let mock = MockTokenEndpoint::start(Arc::new(|_body: &Value| {
            Box::pin(async move { (StatusCode::OK, key_response("sk-or-manual")) })
        }))
        .await;
        let oauth = mock.oauth();
        let handle = InteractionHandle::default();
        let interaction = FakeInteraction::scripted_manual(handle.clone(), "  manual-code  ");

        let credential = oauth.login(&interaction, None).await.expect("login");
        assert_eq!(credential.access, "sk-or-manual");
        let bodies = mock.bodies();
        assert_eq!(bodies.len(), 1);
        assert_eq!(bodies[0]["code"], "manual-code");
    }

    /// `rejects empty manual input without exchanging a code`.
    #[tokio::test]
    async fn login_rejects_empty_manual_input() {
        let mock = MockTokenEndpoint::start(Arc::new(|_body: &Value| {
            Box::pin(async move { (StatusCode::OK, key_response("sk-or-unexpected")) })
        }))
        .await;
        let oauth = mock.oauth();
        let handle = InteractionHandle::default();
        let interaction = FakeInteraction::scripted_manual(handle.clone(), "   ");

        let error = oauth.login(&interaction, None).await.expect_err("empty");
        assert_eq!(error.message, "Missing authorization code");
        assert!(mock.bodies().is_empty(), "no exchange attempted");
    }

    /// `fails login when the manual prompt is cancelled`.
    #[tokio::test]
    async fn login_fails_when_manual_prompt_is_cancelled() {
        let mock = MockTokenEndpoint::start(Arc::new(|_body: &Value| {
            Box::pin(async move { (StatusCode::OK, key_response("sk-or-unexpected")) })
        }))
        .await;
        let oauth = mock.oauth();
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
        assert!(mock.bodies().is_empty(), "no exchange attempted");
    }

    /// `rejects before opening a callback server when login is already
    /// cancelled` — no events are emitted.
    #[tokio::test]
    async fn login_rejects_when_already_cancelled() {
        let mock = MockTokenEndpoint::start(Arc::new(|_body: &Value| {
            Box::pin(async move { (StatusCode::OK, key_response("sk-or-unexpected")) })
        }))
        .await;
        let oauth = mock.oauth();
        let handle = InteractionHandle::default();
        let signal = CancellationToken::new();
        signal.cancel();
        let interaction = FakeInteraction::scripted_manual(handle.clone(), "").with_signal(signal);

        let error = oauth
            .login(&interaction, None)
            .await
            .expect_err("cancelled");
        assert_eq!(error.message, "Login cancelled");
        assert!(
            handle.events().is_empty(),
            "cancelled login must not emit events"
        );
        assert!(mock.bodies().is_empty());
    }

    /// `closes the pending callback when login is cancelled` — the abort
    /// settles the wait and the server shuts down.
    #[tokio::test]
    async fn login_abort_closes_the_callback() {
        let mock = MockTokenEndpoint::start(Arc::new(|_body: &Value| {
            Box::pin(async move { (StatusCode::OK, key_response("sk-or-unexpected")) })
        }))
        .await;
        let oauth = mock.oauth();
        let handle = InteractionHandle::default();
        let signal = CancellationToken::new();
        let interaction = FakeInteraction::new(
            handle.clone(),
            Box::new(|_handle, prompt| {
                Box::pin(async move {
                    expect_manual_code(&prompt);
                    let signal = match &prompt {
                        AuthPrompt::ManualCode {
                            signal: Some(signal),
                            ..
                        } => signal.clone(),
                        _ => unreachable!("prompt checked above"),
                    };
                    signal.cancelled().await;
                    Err(ModelsError::new(ModelsErrorCode::Oauth, "Login cancelled"))
                })
            }),
        )
        .with_signal(signal.clone());

        let login = tokio::spawn(async move { oauth.login(&interaction, None).await });
        let callback_url = wait_for_callback_url(&handle).await;

        signal.cancel();
        let error = login.await.expect("join").expect_err("cancelled");
        assert_eq!(error.message, "Login cancelled");

        // The finally block closed the server: the callback is unreachable.
        let get = reqwest::get(format!("{callback_url}?code=too-late")).await;
        assert!(get.is_err(), "callback server must be closed");
        assert!(mock.bodies().is_empty(), "no exchange attempted");
    }

    /// `reports token exchange failures through both the callback page and
    /// login`.
    #[tokio::test]
    async fn exchange_failure_reports_through_page_and_login() {
        let mock = MockTokenEndpoint::start(Arc::new(|_body: &Value| {
            Box::pin(async move {
                (
                    StatusCode::FORBIDDEN,
                    json!({ "error": { "message": "invalid code" } }).to_string(),
                )
            })
        }))
        .await;
        let oauth = mock.oauth();
        let handle = InteractionHandle::default();
        let interaction = FakeInteraction::new(
            handle.clone(),
            Box::new(|_handle, prompt| {
                Box::pin(async move {
                    expect_manual_code(&prompt);
                    let signal = match &prompt {
                        AuthPrompt::ManualCode {
                            signal: Some(signal),
                            ..
                        } => signal.clone(),
                        _ => unreachable!("prompt checked above"),
                    };
                    signal.cancelled().await;
                    Err(ModelsError::new(ModelsErrorCode::Oauth, "Login cancelled"))
                })
            }),
        );

        let login = tokio::spawn(async move { oauth.login(&interaction, None).await });
        let callback_url = wait_for_callback_url(&handle).await;

        let callback_response = reqwest::get(format!("{callback_url}?code=bad-code"))
            .await
            .expect("callback response");
        assert_eq!(callback_response.status(), StatusCode::BAD_GATEWAY);

        let error = login.await.expect("join").expect_err("exchange failed");
        assert_eq!(
            error.message,
            "OpenRouter OAuth key exchange failed (HTTP 403): invalid code"
        );
    }

    /// `allows only one token exchange for a callback` — a second callback
    /// request gets a 409 while the first exchange is still in flight.
    #[tokio::test]
    async fn second_callback_gets_409_and_only_one_exchange() {
        // Gate the first exchange: the responder awaits the release oneshot,
        // so the exchange stays in flight (claimed) until the test has seen
        // the 409.
        let (started_tx, started_rx) = oneshot::channel::<()>();
        let (release_tx, release_rx) = oneshot::channel::<()>();
        let started_tx = Arc::new(tokio::sync::Mutex::new(Some(started_tx)));
        let release_rx = Arc::new(tokio::sync::Mutex::new(Some(release_rx)));
        let responder: Responder = Arc::new(move |_body: &Value| {
            let started_tx = started_tx.clone();
            let release_rx = release_rx.clone();
            Box::pin(async move {
                if let Some(started) = started_tx.lock().await.take() {
                    let _ = started.send(());
                }
                if let Some(release) = release_rx.lock().await.take() {
                    let _ = release.await;
                }
                (StatusCode::OK, key_response("sk-or-test"))
            })
        });
        let mock = MockTokenEndpoint::start(responder).await;
        let oauth = mock.oauth();
        let handle = InteractionHandle::default();
        let interaction = FakeInteraction::new(
            handle.clone(),
            Box::new(|_handle, prompt| {
                Box::pin(async move {
                    expect_manual_code(&prompt);
                    let signal = match &prompt {
                        AuthPrompt::ManualCode {
                            signal: Some(signal),
                            ..
                        } => signal.clone(),
                        _ => unreachable!("prompt checked above"),
                    };
                    signal.cancelled().await;
                    Err(ModelsError::new(ModelsErrorCode::Oauth, "Login cancelled"))
                })
            }),
        );

        let login = tokio::spawn(async move { oauth.login(&interaction, None).await });
        let callback_url = wait_for_callback_url(&handle).await;

        let first = tokio::spawn({
            let callback_url = callback_url.clone();
            async move {
                reqwest::get(format!("{callback_url}?code=authorization-code"))
                    .await
                    .expect("first callback response")
            }
        });
        // Wait until the first exchange is in flight (claimed).
        started_rx.await.expect("exchange started");

        let second = reqwest::get(format!("{callback_url}?code=second-code"))
            .await
            .expect("second callback response");
        assert_eq!(second.status(), StatusCode::CONFLICT);

        release_tx.send(()).expect("release");
        let credential = login.await.expect("join").expect("login");
        assert_eq!(credential.access, "sk-or-test");

        let first = first.await.expect("join");
        assert_eq!(first.status(), StatusCode::OK);
        assert_eq!(mock.bodies().len(), 1, "exactly one token exchange");
    }

    /// A callback carrying `error` denies the login and renders the denial
    /// page (`error_description ?? error`).
    #[tokio::test]
    async fn callback_error_param_denies_login() {
        let mock = MockTokenEndpoint::start(Arc::new(|_body: &Value| {
            Box::pin(async move { (StatusCode::OK, key_response("sk-or-unexpected")) })
        }))
        .await;
        let oauth = mock.oauth();
        let handle = InteractionHandle::default();
        let interaction = FakeInteraction::new(
            handle.clone(),
            Box::new(|_handle, prompt| {
                Box::pin(async move {
                    expect_manual_code(&prompt);
                    let signal = match &prompt {
                        AuthPrompt::ManualCode {
                            signal: Some(signal),
                            ..
                        } => signal.clone(),
                        _ => unreachable!("prompt checked above"),
                    };
                    signal.cancelled().await;
                    Err(ModelsError::new(ModelsErrorCode::Oauth, "Login cancelled"))
                })
            }),
        );

        let login = tokio::spawn(async move { oauth.login(&interaction, None).await });
        let callback_url = wait_for_callback_url(&handle).await;

        let response = reqwest::get(format!(
            "{callback_url}?error=access_denied&error_description=denied"
        ))
        .await
        .expect("callback response");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let page = response.text().await.expect("page");
        // Converged on the shared server page (`4df157433`): the heading is
        // `${provider} authorization failed.` with the description as details.
        assert!(page.contains("OpenRouter authorization failed."));
        assert!(page.contains("denied"));

        let error = login.await.expect("join").expect_err("denied");
        assert_eq!(error.message, "OpenRouter authorization failed: denied");
        assert!(mock.bodies().is_empty(), "no exchange attempted");
    }

    /// A callback without a code renders a 400 but does not settle the wait;
    /// a subsequent valid callback still completes the login.
    #[tokio::test]
    async fn callback_without_code_does_not_settle_the_wait() {
        let mock = MockTokenEndpoint::start(Arc::new(|_body: &Value| {
            Box::pin(async move { (StatusCode::OK, key_response("sk-or-test")) })
        }))
        .await;
        let oauth = mock.oauth();
        let handle = InteractionHandle::default();
        let interaction = FakeInteraction::new(
            handle.clone(),
            Box::new(|_handle, prompt| {
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
        let callback_url = wait_for_callback_url(&handle).await;

        let response = reqwest::get(format!("{callback_url}?state=no-code"))
            .await
            .expect("callback response");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(
            response
                .text()
                .await
                .expect("page")
                .contains("Missing authorization code.")
        );

        // The wait was not settled: a valid callback completes the login.
        let response = reqwest::get(format!("{callback_url}?code=callback-code"))
            .await
            .expect("callback response");
        assert_eq!(response.status(), StatusCode::OK);

        let credential = login.await.expect("join").expect("login");
        assert_eq!(credential.access, "sk-or-test");
        assert_eq!(mock.bodies().len(), 1);
        assert_eq!(mock.bodies()[0]["code"], "callback-code");
    }
}
