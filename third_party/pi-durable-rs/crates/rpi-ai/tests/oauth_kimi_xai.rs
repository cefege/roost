//! T13 W5 integration tests: the `kimi-coding` and `xai` OAuth flows
//! (`auth/oauth/{kimi_coding,xai}.rs`, ports of
//! `packages/ai/src/auth/oauth/{kimi-coding,xai}.ts` @ pi 0.82.1 2efa728)
//! wired into the W4 factory surface.
//!
//! Covered here (cross-module, through the public API):
//! - both factories expose the real flow under the upstream display name
//!   (the W4 `oauth: None` placeholders are gone; D-029 / D-031 closed);
//! - `to_auth` mapping through the factory-built OAuth object (Bearer header
//!   for kimi-coding, api key for xai);
//! - the load.ts counterpart (`auth/oauth/load.rs`) resolves the new flows
//!   by provider id;
//! - one end-to-end device-code login per flow through the public
//!   constructors + test seams (`with_oauth_host` / `with_authority`)
//!   against a loopback mock (no real network).
//!
//! Flow-level behavior (polling error mappings, refresh retries / rotation,
//! cancellation, env override) is covered by the file-internal unit tests
//! in `auth/oauth/{kimi_coding,xai}.rs`.

use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::Json;
use rpi_ai::auth::oauth::kimi_coding::KimiCodingOAuth;
use rpi_ai::auth::oauth::load::load_oauth_flow;
use rpi_ai::auth::oauth::xai::XaiOAuth;
use rpi_ai::auth::types::BoxFutureSend;
use rpi_ai::auth::{
    AuthEvent, AuthInteraction, AuthPrompt, ModelsError, OAuthAuth, OAuthCredential,
};
use rpi_ai::providers::kimi_coding::kimi_coding_provider;
use rpi_ai::providers::xai::xai_provider;
use serde_json::{Map, Value, json};

// ---------------------------------------------------------------------------
// Factory wiring (the W4 `oauth: None` placeholder tests in the group files
// were replaced by these)
// ---------------------------------------------------------------------------

/// The kimi-coding factory now exposes the real device-code flow under the
/// upstream display name.
#[test]
fn kimi_coding_factory_oauth_is_the_real_flow() {
    let provider = kimi_coding_provider();
    let oauth = provider.auth().oauth.as_ref().expect("oauth");
    assert_eq!(oauth.name(), "Kimi Code (subscription)");
}

/// The xai factory now exposes the real device-code flow under the upstream
/// display name.
#[test]
fn xai_factory_oauth_is_the_real_flow() {
    let provider = xai_provider();
    let oauth = provider.auth().oauth.as_ref().expect("oauth");
    assert_eq!(oauth.name(), "xAI (Grok/X subscription)");
}

/// `toAuth` through the factory-built objects: kimi-coding → Bearer header,
/// xai → api key.
#[tokio::test]
async fn factory_to_auth_mappings() {
    let credential = OAuthCredential {
        refresh: "r".to_owned(),
        access: "access-token".to_owned(),
        expires: i64::MAX,
        extra: Map::new(),
    };

    let provider = kimi_coding_provider();
    let kimi = provider.auth().oauth.as_ref().expect("oauth");
    let auth = kimi.to_auth(&credential).await.expect("to_auth");
    assert_eq!(auth.api_key, None);
    let headers = auth.headers.expect("headers");
    assert_eq!(
        headers.get("Authorization"),
        Some(&Some("Bearer access-token".to_owned()))
    );

    let provider = xai_provider();
    let xai = provider.auth().oauth.as_ref().expect("oauth");
    let auth = xai.to_auth(&credential).await.expect("to_auth");
    assert_eq!(auth.api_key.as_deref(), Some("access-token"));
}

/// The load.ts counterpart resolves the newly wired flows by provider id.
#[test]
fn load_registry_resolves_the_new_flows() {
    assert_eq!(
        load_oauth_flow("kimi-coding")
            .expect("kimi-coding loader")
            .name(),
        "Kimi Code (subscription)"
    );
    assert_eq!(
        load_oauth_flow("xai").expect("xai loader").name(),
        "xAI (Grok/X subscription)"
    );
}

// ---------------------------------------------------------------------------
// End-to-end device-code login through the public constructors + seams
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct RecordedRequest {
    path: String,
    body: String,
}

impl RecordedRequest {
    fn form_get(&self, name: &str) -> Option<String> {
        url::form_urlencoded::parse(self.body.as_bytes())
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.into_owned())
    }
}

type Responder = Arc<dyn Fn(&RecordedRequest) -> (StatusCode, Value) + Send + Sync + 'static>;

/// Loopback mock for the kimi-coding auth host (`/api/oauth/*`).
struct MockKimiAuth {
    url: String,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

impl MockKimiAuth {
    async fn start(responder: Responder) -> Self {
        let requests: Arc<Mutex<Vec<RecordedRequest>>> = Arc::new(Mutex::new(Vec::new()));
        let handler_requests = requests.clone();
        let app = axum::Router::new().fallback(move |request: Request<Body>| {
            let requests = handler_requests.clone();
            let responder = responder.clone();
            async move {
                let recorded = RecordedRequest {
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
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
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
}

impl Drop for MockKimiAuth {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }
}

/// Loopback mock for the xai auth host (authority seam keeps the real host
/// in the path).
struct MockXaiAuth {
    authority: String,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

impl MockXaiAuth {
    async fn start(responder: Responder) -> Self {
        let requests: Arc<Mutex<Vec<RecordedRequest>>> = Arc::new(Mutex::new(Vec::new()));
        let handler_requests = requests.clone();
        let app = axum::Router::new().fallback(move |request: Request<Body>| {
            let requests = handler_requests.clone();
            let responder = responder.clone();
            async move {
                let recorded = RecordedRequest {
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
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
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
}

impl Drop for MockXaiAuth {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }
}

struct FakeInteraction {
    events: Arc<Mutex<Vec<AuthEvent>>>,
}

impl AuthInteraction for FakeInteraction {
    fn prompt<'a>(&'a self, prompt: AuthPrompt) -> BoxFutureSend<'a, Result<String, ModelsError>> {
        // Both flows are prompt-free (upstream: "should not prompt").
        Box::pin(async move {
            Err(ModelsError::new(
                rpi_ai::auth::ModelsErrorCode::Auth,
                format!("Unexpected prompt: {prompt:?}"),
            ))
        })
    }

    fn notify(&self, event: AuthEvent) {
        self.events.lock().expect("lock").push(event);
    }
}

/// A kimi-coding device-code login completes end to end through the
/// factory-style constructor + `oauth_host` seam.
#[tokio::test]
async fn kimi_coding_device_login_runs_end_to_end() {
    let polls = Arc::new(Mutex::new(0usize));
    let responder_polls = polls.clone();
    let responder: Responder = Arc::new(move |request: &RecordedRequest| {
        let path = request.path.as_str();
        if path.ends_with("/api/oauth/device_authorization") {
            assert_eq!(
                request.form_get("client_id").as_deref(),
                Some("17e5f671-d194-4dfb-9706-5516cb48c098")
            );
            return (
                StatusCode::OK,
                json!({
                    "user_code": "ABCD-1234",
                    "device_code": "device-code-123",
                    "verification_uri": "https://www.kimi.com/code",
                    "verification_uri_complete": "https://www.kimi.com/code?user_code=ABCD-1234",
                    "interval": 1,
                    "expires_in": 600,
                }),
            );
        }
        if path.ends_with("/api/oauth/token") {
            let mut count = responder_polls.lock().expect("lock");
            *count += 1;
            return match *count {
                1 => (
                    StatusCode::BAD_REQUEST,
                    json!({ "error": "authorization_pending" }),
                ),
                _ => (
                    StatusCode::OK,
                    json!({ "access_token": "access-token", "refresh_token": "refresh-token", "expires_in": 3600 }),
                ),
            };
        }
        panic!("unexpected request: {path}");
    });
    let mock = MockKimiAuth::start(responder).await;
    let oauth = mock.oauth();
    let events: Arc<Mutex<Vec<AuthEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let interaction = FakeInteraction {
        events: events.clone(),
    };

    let credential = oauth.login(&interaction, None).await.expect("login");
    assert_eq!(credential.access, "access-token");
    assert_eq!(credential.refresh, "refresh-token");
    assert_eq!(
        events.lock().expect("lock").as_slice(),
        &[AuthEvent::DeviceCode {
            user_code: "ABCD-1234".to_owned(),
            verification_uri: "https://www.kimi.com/code?user_code=ABCD-1234".to_owned(),
            interval_seconds: Some(1),
            expires_in_seconds: Some(600),
        }]
    );
    assert_eq!(mock.requests.lock().expect("lock").len(), 3);
}

/// An xai device-code login completes end to end through the
/// factory-style constructor + `authority` seam.
#[tokio::test]
async fn xai_device_login_runs_end_to_end() {
    let responder: Responder = Arc::new(|request: &RecordedRequest| {
        let path = request.path.as_str();
        if path.ends_with("/oauth2/device/code") {
            assert_eq!(
                request.form_get("client_id").as_deref(),
                Some("b1a00492-073a-47ea-816f-4c329264a828")
            );
            assert_eq!(request.form_get("referrer").as_deref(), Some("pi"));
            return (
                StatusCode::OK,
                json!({
                    "device_code": "device-code",
                    "user_code": "ABCD-1234",
                    "verification_uri": "https://accounts.x.ai/oauth2/device",
                    "expires_in": 900,
                    "interval": 1,
                }),
            );
        }
        if path.ends_with("/oauth2/token") {
            return (
                StatusCode::OK,
                json!({
                    "access_token": "access-token",
                    "refresh_token": "refresh-token",
                    "expires_in": 21_600,
                }),
            );
        }
        panic!("unexpected request: {path}");
    });
    let mock = MockXaiAuth::start(responder).await;
    let oauth = mock.oauth();
    let events: Arc<Mutex<Vec<AuthEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let interaction = FakeInteraction {
        events: events.clone(),
    };

    let credential = oauth.login(&interaction, None).await.expect("login");
    assert_eq!(credential.access, "access-token");
    assert_eq!(credential.refresh, "refresh-token");
    assert_eq!(
        events.lock().expect("lock").as_slice(),
        &[AuthEvent::DeviceCode {
            user_code: "ABCD-1234".to_owned(),
            verification_uri: "https://accounts.x.ai/oauth2/device".to_owned(),
            interval_seconds: Some(1),
            expires_in_seconds: Some(900),
        }]
    );
    let requests = mock.requests.lock().expect("lock").clone();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].path, "/auth.x.ai/oauth2/device/code");
    assert_eq!(requests[1].path, "/auth.x.ai/oauth2/token");
}
