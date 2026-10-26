//! Port of `packages/ai/src/auth/oauth/callback-server.ts` @ pi abe508e1b
//! (v1.1.0) — the loopback OAuth redirect handler shared by the Anthropic,
//! OpenAI Codex, OpenRouter and Radius browser sign-in flows.
//!
//! Intentional differences: the upstream `node:http` server becomes an axum
//! router (coding-standards appendix A); the upstream promise graph becomes
//! `watch`-channel settlement plus `tokio::select!`; `AbortSignal` becomes a
//! [`CancellationToken`]. The page HTML lives in [`super::callback_page`]
//! (upstream `utils/oauth-page.ts`).
//!
//! Bind failures propagate as `Err` (upstream `server.once("error")` before
//! `listen` resolves); call sites that fall back to pasted input catch the
//! error, exactly like upstream `.catch(() => undefined)`.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{Method, Response, StatusCode, Uri};
use axum::response::{Html, IntoResponse};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use super::super::interaction::{AuthInteraction, AuthPrompt};
use super::super::resolve::{ModelsError, ModelsErrorCode};
use super::super::types::BoxFutureSend;
use super::callback_page::{oauth_error_html, oauth_success_html};

/// Default bind host; upstream `getProviderEnvValue("PI_OAUTH_CALLBACK_HOST")
/// || "127.0.0.1"` with the ADR-0001 `RPI_` prefix.
const CALLBACK_HOST_ENV: &str = "RPI_OAUTH_CALLBACK_HOST";

/// Upstream: `getProviderEnvValue("PI_OAUTH_CALLBACK_HOST") || "127.0.0.1"`
/// (ADR-0001 §2 `RPI_` prefix).
pub fn default_callback_host() -> String {
    std::env::var(CALLBACK_HOST_ENV)
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "127.0.0.1".to_owned())
}

/// `complete` — finishes the sign-in with the received code before the browser
/// page is sent, so the page can show exchange failures. Pass an identity
/// closure to exchange the code later.
pub type CompleteFn<T> =
    Arc<dyn Fn(String) -> BoxFutureSend<'static, Result<T, ModelsError>> + Send + Sync>;

/// `OAuthCallbackServerOptions<T>`.
pub struct OAuthCallbackServerOptions<T> {
    /// Provider name used on the browser page, for example `OpenAI`.
    pub provider_name: String,
    /// Address to listen on.
    pub host: String,
    /// Port to listen on; `0` picks a free port.
    pub port: u16,
    pub path: String,
    /// Host in `redirectUri` when it differs from `host`, for example
    /// `localhost`.
    pub redirect_host: Option<String>,
    /// Expected `state` parameter. `None` when the provider does not send one.
    pub state: Option<String>,
    pub complete: CompleteFn<T>,
    pub signal: Option<CancellationToken>,
    /// Upstream `timeoutMs`.
    pub timeout: Option<Duration>,
}

/// Settled outcome of the one-shot wait.
#[derive(Debug, Clone)]
enum CallbackOutcome<T> {
    /// The provider redirected with a code and `complete` succeeded.
    Completed(T),
    /// `cancel()` handed the login over to manual input.
    Cancelled,
    /// Provider error redirect, `complete` failure, abort, or timeout.
    Failed(ModelsError),
}

struct CallbackState<T> {
    provider_name: String,
    expected_state: Option<String>,
    complete: CompleteFn<T>,
    path: String,
    /// Settle-once channel: outer `None` = waiting; `Some(outcome)` = done.
    settle: watch::Sender<Option<CallbackOutcome<T>>>,
    settled: AtomicBool,
    claimed: AtomicBool,
}

impl<T> CallbackState<T> {
    /// `finish` — settle once, first caller wins.
    fn finish(&self, outcome: CallbackOutcome<T>) {
        if self
            .settled
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            self.settle.send_replace(Some(outcome));
        }
    }

    /// Claim the one allowed callback. `swap` publishes and reads the
    /// previous value atomically; the old `load` guard + `store` pair let
    /// two concurrent callbacks both proceed to the token exchange
    /// (v0.1.6 review P2-1). The atomicity is pinned by the
    /// `claim_is_atomic_under_concurrency` unit test.
    fn try_claim(&self) -> bool {
        !self.claimed.swap(true, Ordering::SeqCst)
    }
}

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

/// `sendPage` — `content-type: text/html; charset=utf-8`,
/// `cache-control: no-store`.
fn callback_response(status: StatusCode, html: String) -> Response<Body> {
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

async fn handle_callback<T: Clone + Send + Sync + 'static>(
    State(state): State<Arc<CallbackState<T>>>,
    request: Request,
) -> Response<Body> {
    let uri = request.uri().clone();
    if request.method() != Method::GET || uri.path() != state.path {
        return callback_response(
            StatusCode::NOT_FOUND,
            oauth_error_html("Callback route not found.", None),
        );
    }
    let params = query_params(&uri);
    if let Some(expected) = &state.expected_state
        && params.get("state").map(String::as_str) != Some(expected.as_str())
    {
        return callback_response(
            StatusCode::BAD_REQUEST,
            oauth_error_html("State mismatch.", None),
        );
    }
    if state.claimed.load(Ordering::SeqCst) || state.settled.load(Ordering::SeqCst) {
        return callback_response(
            StatusCode::CONFLICT,
            oauth_error_html("This sign-in has already been handled.", None),
        );
    }
    if let Some(error) = params.get("error").filter(|value| !value.is_empty()) {
        let description = params
            .get("error_description")
            .map(String::as_str)
            .unwrap_or(error);
        let heading = format!("{} authorization failed.", state.provider_name);
        state.finish(CallbackOutcome::Failed(ModelsError::new(
            ModelsErrorCode::Oauth,
            format!(
                "{} authorization failed: {description}",
                state.provider_name
            ),
        )));
        return callback_response(
            StatusCode::BAD_REQUEST,
            oauth_error_html(&heading, Some(description)),
        );
    }
    let Some(code) = params.get("code").filter(|value| !value.is_empty()) else {
        return callback_response(
            StatusCode::BAD_REQUEST,
            oauth_error_html("Missing authorization code.", None),
        );
    };
    // Claim the callback atomically (v0.1.6 review P2-1): a load-then-store
    // check let two concurrent callbacks with the same code both proceed to
    // the token exchange.
    if !state.try_claim() {
        return callback_response(
            StatusCode::CONFLICT,
            oauth_error_html("This sign-in has already been handled.", None),
        );
    }
    let complete = state.complete.clone();
    match complete(code.clone()).await {
        Ok(value) => {
            let message = format!(
                "Signed in to {}. You may now close this page.",
                state.provider_name
            );
            state.finish(CallbackOutcome::Completed(value));
            callback_response(StatusCode::OK, oauth_success_html(&message))
        }
        Err(error) => {
            let heading = format!("{} sign-in failed.", state.provider_name);
            state.finish(CallbackOutcome::Failed(error.clone()));
            callback_response(
                StatusCode::BAD_GATEWAY,
                oauth_error_html(&heading, Some(&error.message)),
            )
        }
    }
}

/// `OAuthCallbackServer<T>`.
pub struct OAuthCallbackServer<T> {
    state: Arc<CallbackState<T>>,
    local_addr: SocketAddr,
    path: String,
    redirect_host: String,
    shutdown: CancellationToken,
    serve: Option<tokio::task::JoinHandle<()>>,
}

impl<T: Clone + Send + Sync + 'static> OAuthCallbackServer<T> {
    /// `startOAuthCallbackServer` — bind and serve. Bind errors (for example
    /// the port already in use) propagate; the shared server never picks a
    /// different port itself (upstream test asserts `EADDRINUSE` rejection).
    pub async fn start(options: OAuthCallbackServerOptions<T>) -> Result<Self, ModelsError> {
        let provider_name = options.provider_name;
        let state = options.state;
        let signal = options.signal;
        if signal.as_ref().is_some_and(CancellationToken::is_cancelled) {
            return Err(ModelsError::new(ModelsErrorCode::Oauth, "Login cancelled"));
        }
        let listener = tokio::net::TcpListener::bind((options.host.as_str(), options.port))
            .await
            .map_err(|bind_error| {
                ModelsError::new(
                    ModelsErrorCode::Oauth,
                    format!(
                        "OAuth callback server failed to bind {}:{}: {bind_error}",
                        options.host, options.port
                    ),
                )
            })?;
        let local_addr = listener.local_addr().map_err(|address_error| {
            ModelsError::new(
                ModelsErrorCode::Oauth,
                format!("OAuth callback server failed to read its address: {address_error}"),
            )
        })?;

        let (settle, _) = watch::channel(None);
        let state = Arc::new(CallbackState {
            provider_name: provider_name.clone(),
            expected_state: state,
            complete: options.complete,
            path: options.path.clone(),
            settle,
            settled: AtomicBool::new(false),
            claimed: AtomicBool::new(false),
        });

        let app = axum::Router::new()
            .fallback(handle_callback::<T>)
            .with_state(state.clone());
        let shutdown = CancellationToken::new();
        let serve_shutdown = shutdown.clone();
        let serve_state = state.clone();
        let serve = tokio::spawn(async move {
            let result = axum::serve(listener, app)
                .with_graceful_shutdown(serve_shutdown.cancelled_owned())
                .await;
            if let Err(serve_error) = result {
                // Upstream `server.on("error", error => finish({ error }))`:
                // a post-listen server error settles the wait instead of
                // leaving it pending forever.
                tracing::warn!(%serve_error, "OAuth callback server terminated with an error");
                serve_state.finish(CallbackOutcome::Failed(ModelsError::new(
                    ModelsErrorCode::Oauth,
                    format!("OAuth callback server terminated: {serve_error}"),
                )));
            }
        });

        // `signal?.addEventListener("abort", onAbort)`: the upstream promise
        // rejects with "Login cancelled".
        if let Some(signal) = signal {
            let state = state.clone();
            tokio::spawn(async move {
                signal.cancelled().await;
                state.finish(CallbackOutcome::Failed(ModelsError::new(
                    ModelsErrorCode::Oauth,
                    "Login cancelled",
                )));
            });
        }
        // `setTimeout(() => finish(error), timeoutMs)`.
        if let Some(timeout) = options.timeout {
            let state = state.clone();
            let provider_name = state.provider_name.clone();
            let timeout_shutdown = shutdown.clone();
            tokio::spawn(async move {
                // `setTimeout(...)` + `clearTimeout` on settle: the timer is
                // dropped once the server shuts down so a settled login does
                // not leave a sleeping task behind.
                tokio::select! {
                    _ = timeout_shutdown.cancelled() => {}
                    _ = tokio::time::sleep(timeout) => {
                        state.finish(CallbackOutcome::Failed(ModelsError::new(
                            ModelsErrorCode::Oauth,
                            format!("{provider_name} sign-in timed out"),
                        )));
                    }
                }
            });
        }

        let redirect_host = options
            .redirect_host
            .unwrap_or_else(|| options.host.clone());
        Ok(Self {
            state,
            local_addr,
            path: options.path,
            redirect_host,
            shutdown,
            serve: Some(serve),
        })
    }

    /// `redirectUri` — `http://{redirectHost}:{port}{path}`, bracketing IPv6.
    pub fn redirect_uri(&self) -> String {
        let host = &self.redirect_host;
        let authority = if host.contains(':') {
            format!("[{host}]")
        } else {
            host.clone()
        };
        format!("http://{authority}:{}{}", self.local_addr.port(), self.path)
    }

    /// Actually bound address (relevant when started on port 0).
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// `wait()` — resolves with the result of `complete`, or `None` after
    /// `cancel()` / `close()` handed the login over. Errs when the provider
    /// redirected with an error, `complete` failed, the signal aborted, or the
    /// timeout elapsed.
    pub async fn wait(&self) -> Result<Option<T>, ModelsError> {
        let mut rx = self.state.settle.subscribe();
        if let Some(outcome) = rx.borrow().clone() {
            return outcome.into_result();
        }
        loop {
            if rx.changed().await.is_err() {
                return Ok(None); // sender dropped (server closed)
            }
            if let Some(outcome) = rx.borrow_and_update().clone() {
                return outcome.into_result();
            }
        }
    }

    /// `cancel()` — stop waiting for the browser unless a callback is already
    /// being completed.
    pub fn cancel(&self) {
        if !self.state.claimed.load(Ordering::SeqCst) {
            self.state.finish(CallbackOutcome::Cancelled);
        }
    }

    /// `close()` — stop accepting connections. Graceful shutdown also closes
    /// idle keep-alive connections, the axum equivalent of the upstream
    /// `closeAllConnections()` call that keeps a browser's spare connection
    /// from serving a later login's callback.
    pub async fn close(mut self) {
        self.state.finish(CallbackOutcome::Failed(ModelsError::new(
            ModelsErrorCode::Oauth,
            "OAuth callback server closed",
        )));
        self.shutdown.cancel();
        if let Some(serve) = self.serve.take() {
            let _ = serve.await;
        }
    }
}

impl<T> CallbackOutcome<T> {
    fn into_result(self) -> Result<Option<T>, ModelsError> {
        match self {
            CallbackOutcome::Completed(value) => Ok(Some(value)),
            CallbackOutcome::Cancelled => Ok(None),
            CallbackOutcome::Failed(error) => Err(error),
        }
    }
}

impl<T> Drop for OAuthCallbackServer<T> {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

/// `waitForCallbackOrManualInput` result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallbackOrManual<T> {
    /// The loopback callback completed first.
    Callback(T),
    /// The user pasted the code or redirect URL.
    Manual(String),
}

/// `waitForCallbackOrManualInput` prompt copy.
#[derive(Debug, Clone)]
pub struct ManualPrompt {
    pub message: String,
    pub placeholder: String,
}

/// Wait for the browser callback, or for the user to paste the code or
/// redirect URL when the browser cannot reach the loopback server (for example
/// over SSH). Without a callback server only the manual prompt is used.
pub async fn wait_for_callback_or_manual_input<T: Clone + Send + Sync + 'static>(
    interaction: &dyn AuthInteraction,
    callback: Option<&OAuthCallbackServer<T>>,
    prompt: ManualPrompt,
) -> Result<CallbackOrManual<T>, ModelsError> {
    let manual_cancel = CancellationToken::new();
    let manual = interaction.prompt(AuthPrompt::ManualCode {
        message: prompt.message,
        placeholder: Some(prompt.placeholder),
        signal: Some(manual_cancel.clone()),
    });
    tokio::pin!(manual);
    let mut manual_outcome: Option<Result<String, ModelsError>> = None;

    if let Some(server) = callback {
        let wait = server.wait();
        tokio::pin!(wait);
        let mut wait_outcome: Option<Result<Option<T>, ModelsError>> = None;
        tokio::select! {
            result = &mut wait => wait_outcome = Some(result),
            input = &mut manual => {
                // `callback?.cancel()` — hand the login over to manual input.
                server.cancel();
                manual_outcome = Some(input);
            }
        }
        match wait_outcome {
            // `if (value !== undefined) return { type: "callback", value }`.
            Some(Ok(Some(value))) => {
                manual_cancel.cancel();
                return Ok(CallbackOrManual::Callback(value));
            }
            // A callback rejection propagates; the pending prompt is aborted.
            Some(Err(error)) => {
                manual_cancel.cancel();
                return Err(error);
            }
            // Cancelled (manual handover) or closed: fall through to the
            // manual input, awaiting the prompt when it is still pending.
            Some(Ok(None)) | None => {}
        }
    }

    let input = match manual_outcome {
        Some(outcome) => outcome,
        None => manual.await,
    };
    manual_cancel.cancel();
    input.map(CallbackOrManual::Manual)
}

#[cfg(test)]
mod tests {
    //! Test intents ported from `packages/ai/test/oauth-callback-server.test.ts`
    //! @ pi abe508e1b (v1.1.0), same names in snake_case. The upstream mocked
    //! fetch becomes a `reqwest` request against the loopback server; fake
    //! timers for the timeout case become a short real timeout.

    use std::sync::Mutex;

    use super::super::super::interaction::AuthEvent;
    use super::*;

    fn identity_complete() -> CompleteFn<String> {
        Arc::new(|code| Box::pin(async move { Ok(code) }))
    }

    async fn start_test_server(state: Option<&str>) -> (OAuthCallbackServer<String>, String) {
        let server = OAuthCallbackServer::start(OAuthCallbackServerOptions {
            provider_name: "Test".to_owned(),
            host: "127.0.0.1".to_owned(),
            port: 0,
            path: "/callback".to_owned(),
            redirect_host: None,
            state: state.map(str::to_owned),
            complete: identity_complete(),
            signal: None,
            timeout: None,
        })
        .await
        .expect("bind");
        let base = format!("http://{}", server.local_addr());
        (server, base)
    }

    #[tokio::test]
    async fn stray_request_and_unknown_route_return_404() {
        let (server, base) = start_test_server(Some("expected")).await;
        let response = reqwest::get(format!("{base}/other"))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(
            response
                .text()
                .await
                .expect("body")
                .contains("Callback route not found.")
        );
        server.close().await;
    }

    #[tokio::test]
    async fn missing_state_parameter_returns_400() {
        let (server, base) = start_test_server(Some("expected")).await;
        let response = reqwest::get(format!("{base}/callback?code=the-code"))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(
            response
                .text()
                .await
                .expect("body")
                .contains("State mismatch.")
        );
        server.close().await;
    }

    /// v0.1.6 review P2-1: concurrent callbacks carrying the same code
    /// must not both run the token exchange. A multi-thread runtime plus a
    /// burst makes the old load-then-store check race in practice; the
    /// claim primitive itself is pinned deterministically by
    /// `claim_is_atomic_under_concurrency`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_callbacks_exchange_the_code_once() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = Arc::new(AtomicUsize::new(0));
        let complete_calls = calls.clone();
        let complete: CompleteFn<String> = Arc::new(move |code| {
            let calls = complete_calls.clone();
            Box::pin(async move {
                calls.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                Ok(code)
            })
        });
        let server = OAuthCallbackServer::start(OAuthCallbackServerOptions {
            provider_name: "Test".to_owned(),
            host: "127.0.0.1".to_owned(),
            port: 0,
            path: "/callback".to_owned(),
            redirect_host: None,
            state: Some("expected".to_owned()),
            complete,
            signal: None,
            timeout: None,
        })
        .await
        .expect("bind");
        let base = format!("http://{}", server.local_addr());
        let url = format!("{base}/callback?code=the-code&state=expected");
        let mut tasks = Vec::new();
        for _ in 0..8 {
            tasks.push(tokio::spawn(reqwest::get(url.clone())));
        }
        let mut ok = 0usize;
        let mut conflict = 0usize;
        for task in tasks {
            let response = task.await.expect("join").expect("response");
            match response.status() {
                StatusCode::OK => ok += 1,
                StatusCode::CONFLICT => conflict += 1,
                other => panic!("unexpected callback status {other}"),
            }
        }
        assert_eq!(ok, 1, "exactly one callback wins");
        assert_eq!(conflict, 7, "every other callback is rejected as claimed");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "the token exchange must run exactly once"
        );
        server.wait().await.expect("wait").expect("completed");
        server.close().await;
    }

    #[tokio::test]
    async fn state_mismatch_returns_400_without_settling() {
        let (server, base) = start_test_server(Some("expected")).await;
        let response = reqwest::get(format!("{base}/callback?code=c&state=wrong"))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        server.cancel();
        assert_eq!(server.wait().await.expect("wait"), None);
        server.close().await;
    }

    #[tokio::test]
    async fn success_settles_with_complete_result() {
        let (server, base) = start_test_server(Some("expected")).await;
        let response = reqwest::get(format!("{base}/callback?code=the-code&state=expected"))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.text().await.expect("body");
        assert!(body.contains("Signed in to Test. You may now close this page."));
        // v1.0.0 three-color badge (oauth-callback-server.test.ts:74-76).
        assert!(body.contains("fill=\"#F09082\""));
        assert!(body.contains("fill=\"#4D9ABF\""));
        assert!(body.contains("fill=\"#F1BE58\""));
        assert_eq!(
            server.wait().await.expect("wait"),
            Some("the-code".to_owned())
        );
        server.close().await;
    }

    #[tokio::test]
    async fn error_parameter_returns_400_with_description_and_settles_failed() {
        let (server, base) = start_test_server(Some("expected")).await;
        let response = reqwest::get(format!(
            "{base}/callback?state=expected&error=access_denied&error_description=Nope"
        ))
        .await
        .expect("response");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = response.text().await.expect("body");
        assert!(body.contains("Test authorization failed."));
        assert!(body.contains("Nope"));
        let error = server.wait().await.expect_err("settled failed");
        assert!(error.message.contains("Test authorization failed: Nope"));
        server.close().await;
    }

    #[tokio::test]
    async fn second_callback_returns_409() {
        let (server, base) = start_test_server(Some("expected")).await;
        let first = reqwest::get(format!("{base}/callback?code=one&state=expected"))
            .await
            .expect("response");
        assert_eq!(first.status(), StatusCode::OK);
        let second = reqwest::get(format!("{base}/callback?code=two&state=expected"))
            .await
            .expect("response");
        assert_eq!(second.status(), StatusCode::CONFLICT);
        assert!(
            second
                .text()
                .await
                .expect("body")
                .contains("This sign-in has already been handled.")
        );
        assert_eq!(server.wait().await.expect("wait"), Some("one".to_owned()));
        server.close().await;
    }

    #[tokio::test]
    async fn complete_failure_returns_502_and_settles_failed() {
        let complete: CompleteFn<String> = Arc::new(|_| {
            Box::pin(async { Err(ModelsError::new(ModelsErrorCode::Oauth, "exchange boom")) })
        });
        let server = OAuthCallbackServer::start(OAuthCallbackServerOptions {
            provider_name: "Test".to_owned(),
            host: "127.0.0.1".to_owned(),
            port: 0,
            path: "/callback".to_owned(),
            redirect_host: None,
            state: None,
            complete,
            signal: None,
            timeout: None,
        })
        .await
        .expect("bind");
        let response = reqwest::get(format!(
            "http://{}/callback?code=the-code",
            server.local_addr()
        ))
        .await
        .expect("response");
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert!(
            response
                .text()
                .await
                .expect("body")
                .contains("Test sign-in failed.")
        );
        let error = server.wait().await.expect_err("settled failed");
        assert_eq!(error.message, "exchange boom");
        server.close().await;
    }

    #[tokio::test]
    async fn cancel_settles_none_and_abort_settles_failed() {
        let (server, _base) = start_test_server(Some("expected")).await;
        server.cancel();
        assert_eq!(server.wait().await.expect("wait"), None);
        server.close().await;

        let signal = CancellationToken::new();
        let server = OAuthCallbackServer::start(OAuthCallbackServerOptions {
            provider_name: "Test".to_owned(),
            host: "127.0.0.1".to_owned(),
            port: 0,
            path: "/callback".to_owned(),
            redirect_host: None,
            state: None,
            complete: identity_complete(),
            signal: Some(signal.clone()),
            timeout: None,
        })
        .await
        .expect("bind");
        signal.cancel();
        let error = server.wait().await.expect_err("aborted");
        assert_eq!(error.message, "Login cancelled");
        server.close().await;
    }

    #[tokio::test]
    async fn already_aborted_signal_rejects_start() {
        let signal = CancellationToken::new();
        signal.cancel();
        let result = OAuthCallbackServer::start(OAuthCallbackServerOptions {
            provider_name: "Test".to_owned(),
            host: "127.0.0.1".to_owned(),
            port: 0,
            path: "/callback".to_owned(),
            redirect_host: None,
            state: None,
            complete: identity_complete(),
            signal: Some(signal),
            timeout: None,
        })
        .await;
        assert!(matches!(result, Err(error) if error.message == "Login cancelled"));
    }

    /// A browser spare connection must not survive `close()`: upstream calls
    /// `closeAllConnections()` so a later login never receives its callback
    /// over this server's idle keep-alive connection (02eed88fd).
    #[tokio::test]
    async fn close_terminates_idle_keep_alive_connections() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let (server, base) = start_test_server(Some("expected")).await;
        let mut stream = tokio::net::TcpStream::connect(server.local_addr())
            .await
            .expect("connect");
        stream
            .write_all(
                format!(
                    "GET /callback?code=x&state=expected HTTP/1.1\r\nHost: {base}\r\nConnection: keep-alive\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .expect("write");
        let mut buffer = [0u8; 1024];
        let read = stream.read(&mut buffer).await.expect("first read");
        assert!(read > 0, "server responded before close");

        server.close().await;
        let closed = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match stream.read(&mut buffer).await {
                    Ok(0) | Err(_) => return true,
                    Ok(_) => continue, // drain the response body
                }
            }
        })
        .await;
        assert_eq!(
            closed,
            Ok(true),
            "idle keep-alive connection must be closed by close()"
        );
    }

    /// Non-GET requests never complete the sign-in (upstream callback-server
    /// test).
    #[tokio::test]
    async fn post_request_returns_404() {
        let (server, base) = start_test_server(Some("expected")).await;
        let client = reqwest::Client::new();
        let response = client
            .post(format!("{base}/callback?code=c&state=expected"))
            .send()
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        server.close().await;
    }

    /// `cancel()` after a claimed callback is a no-op (the claim wins).
    #[tokio::test]
    async fn cancel_after_claimed_does_not_settle() {
        let (server, base) = start_test_server(Some("expected")).await;
        let response = reqwest::get(format!("{base}/callback?code=the-code&state=expected"))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        server.cancel();
        assert_eq!(
            server.wait().await.expect("wait"),
            Some("the-code".to_owned())
        );
        server.close().await;
    }

    /// A callback arriving after `cancel()` is refused with 409 and does not
    /// revive the wait.
    #[tokio::test]
    async fn late_callback_after_cancel_returns_409() {
        let (server, base) = start_test_server(Some("expected")).await;
        server.cancel();
        assert_eq!(server.wait().await.expect("wait"), None);
        let response = reqwest::get(format!("{base}/callback?code=late&state=expected"))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::CONFLICT);
        server.close().await;
    }

    #[tokio::test]
    async fn timeout_settles_failed() {
        let server = OAuthCallbackServer::start(OAuthCallbackServerOptions {
            provider_name: "Test".to_owned(),
            host: "127.0.0.1".to_owned(),
            port: 0,
            path: "/callback".to_owned(),
            redirect_host: None,
            state: None,
            complete: identity_complete(),
            signal: None,
            timeout: Some(Duration::from_millis(20)),
        })
        .await
        .expect("bind");
        let error = server.wait().await.expect_err("timed out");
        assert_eq!(error.message, "Test sign-in timed out");
        server.close().await;
    }

    #[tokio::test]
    async fn port_in_use_rejects_start_instead_of_rebinding() {
        let (server, _base) = start_test_server(Some("expected")).await;
        let port = server.local_addr().port();
        let result = OAuthCallbackServer::start(OAuthCallbackServerOptions {
            provider_name: "Test".to_owned(),
            host: "127.0.0.1".to_owned(),
            port,
            path: "/callback".to_owned(),
            redirect_host: None,
            state: None,
            complete: identity_complete(),
            signal: None,
            timeout: None,
        })
        .await;
        assert!(result.is_err());
        server.close().await;
    }

    /// `redirectUri` keeps the advertised host (upstream `redirectHost` seam).
    #[tokio::test]
    async fn redirect_uri_uses_the_redirect_host() {
        let server = OAuthCallbackServer::start(OAuthCallbackServerOptions {
            provider_name: "Test".to_owned(),
            host: "127.0.0.1".to_owned(),
            port: 0,
            path: "/callback".to_owned(),
            redirect_host: Some("localhost".to_owned()),
            state: None,
            complete: identity_complete(),
            signal: None,
            timeout: None,
        })
        .await
        .expect("bind");
        assert_eq!(
            server.redirect_uri(),
            format!("http://localhost:{}/callback", server.local_addr().port())
        );
        server.close().await;
    }

    struct RecordingInteraction {
        prompts: Mutex<Vec<AuthPrompt>>,
        answer: Result<String, ModelsError>,
    }

    impl AuthInteraction for RecordingInteraction {
        fn prompt<'a>(
            &'a self,
            prompt: AuthPrompt,
        ) -> BoxFutureSend<'a, Result<String, ModelsError>> {
            Box::pin(async move {
                self.prompts
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(prompt);
                self.answer.clone()
            })
        }

        fn notify(&self, _event: AuthEvent) {}
    }

    /// The manual prompt settles first: `cancel()` resolves the callback wait
    /// with `None` and the pasted input wins.
    #[tokio::test]
    async fn manual_input_wins_when_the_prompt_settles_first() {
        let (server, _base) = start_test_server(Some("expected")).await;
        let interaction = RecordingInteraction {
            prompts: Mutex::new(Vec::new()),
            answer: Ok("the-code".to_owned()),
        };
        let result = wait_for_callback_or_manual_input(
            &interaction,
            Some(&server),
            ManualPrompt {
                message: "paste".to_owned(),
                placeholder: "url".to_owned(),
            },
        )
        .await
        .expect("manual");
        assert_eq!(result, CallbackOrManual::Manual("the-code".to_owned()));
        // The prompt was cancelled once the race settled (upstream
        // `finally { manualAbort.abort(); }`).
        let signal = {
            let prompts = interaction
                .prompts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            prompts[0].signal().expect("per-prompt signal")
        };
        assert!(signal.is_cancelled());
        server.close().await;
    }

    /// Without a callback server the helper only uses the manual prompt.
    #[tokio::test]
    async fn manual_only_without_callback_server() {
        let interaction = RecordingInteraction {
            prompts: Mutex::new(Vec::new()),
            answer: Ok("pasted".to_owned()),
        };
        let result = wait_for_callback_or_manual_input(
            &interaction,
            None::<&OAuthCallbackServer<String>>,
            ManualPrompt {
                message: "paste".to_owned(),
                placeholder: "url".to_owned(),
            },
        )
        .await
        .expect("manual");
        assert_eq!(result, CallbackOrManual::Manual("pasted".to_owned()));
    }

    /// A rejected manual prompt surfaces once the callback wait resolves.
    #[tokio::test]
    async fn manual_prompt_error_propagates() {
        let (server, _base) = start_test_server(Some("expected")).await;
        let interaction = RecordingInteraction {
            prompts: Mutex::new(Vec::new()),
            answer: Err(ModelsError::new(ModelsErrorCode::Oauth, "Login cancelled")),
        };
        let error = wait_for_callback_or_manual_input(
            &interaction,
            Some(&server),
            ManualPrompt {
                message: "paste".to_owned(),
                placeholder: "url".to_owned(),
            },
        )
        .await
        .expect_err("prompt error");
        assert_eq!(error.message, "Login cancelled");
        server.close().await;
    }

    /// The browser callback wins: the helper returns the completed value and
    /// the pending manual prompt is cancelled.
    #[tokio::test]
    async fn browser_callback_wins_over_the_pending_prompt() {
        let (server, base) = start_test_server(Some("expected")).await;
        let interaction = Arc::new(PendingInteraction::default());
        let interaction_for_task = interaction.clone();
        let base_for_task = base.clone();
        let task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            reqwest::get(format!(
                "{base_for_task}/callback?code=winner&state=expected"
            ))
            .await
            .expect("response");
        });
        let result = {
            let interaction: &dyn AuthInteraction = interaction_for_task.as_ref();
            wait_for_callback_or_manual_input(
                interaction,
                Some(&server),
                ManualPrompt {
                    message: "paste".to_owned(),
                    placeholder: "url".to_owned(),
                },
            )
            .await
            .expect("callback")
        };
        assert_eq!(result, CallbackOrManual::Callback("winner".to_owned()));
        task.await.expect("request task");
        server.close().await;
    }

    /// Interaction whose prompt never settles until its per-prompt signal is
    /// cancelled (mirrors the mounted login dialog waiting for input).
    #[derive(Default)]
    struct PendingInteraction {
        prompts: Mutex<Vec<AuthPrompt>>,
    }

    impl AuthInteraction for PendingInteraction {
        fn prompt<'a>(
            &'a self,
            prompt: AuthPrompt,
        ) -> BoxFutureSend<'a, Result<String, ModelsError>> {
            Box::pin(async move {
                let signal = prompt.signal().expect("per-prompt signal");
                self.prompts
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(prompt);
                signal.cancelled().await;
                Err(ModelsError::new(ModelsErrorCode::Oauth, "Login cancelled"))
            })
        }

        fn notify(&self, _event: AuthEvent) {}
    }

    /// A minimal `CallbackState` for the claim primitive test.
    fn claim_test_state() -> Arc<CallbackState<()>> {
        let (settle, _receiver) = watch::channel(None);
        Arc::new(CallbackState {
            provider_name: "Test".to_owned(),
            expected_state: None,
            complete: Arc::new(|_code: String| Box::pin(async { Ok(()) })),
            path: "/callback".to_owned(),
            settle,
            settled: AtomicBool::new(false),
            claimed: AtomicBool::new(false),
        })
    }

    /// The claim is a single atomic swap: exactly one racer may win. The
    /// old load-guard + store shape admitted several winners under a
    /// barrier release, which is what let two callbacks exchange the code
    /// twice.
    #[test]
    fn claim_is_atomic_under_concurrency() {
        use std::sync::Barrier;
        use std::sync::atomic::AtomicUsize;
        const RACERS: usize = 32;
        for round in 0..128 {
            let state = claim_test_state();
            let barrier = Arc::new(Barrier::new(RACERS));
            let winners = Arc::new(AtomicUsize::new(0));
            let mut handles = Vec::new();
            for _ in 0..RACERS {
                let state = state.clone();
                let barrier = barrier.clone();
                let winners = winners.clone();
                handles.push(std::thread::spawn(move || {
                    barrier.wait();
                    if state.try_claim() {
                        winners.fetch_add(1, Ordering::SeqCst);
                    }
                }));
            }
            for handle in handles {
                handle.join().expect("racer thread");
            }
            assert_eq!(winners.load(Ordering::SeqCst), 1, "round {round}");
            assert!(state.claimed.load(Ordering::SeqCst));
        }
    }
}
