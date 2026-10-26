//! T13 W5 integration tests: the `github-copilot` and `radius` OAuth flows
//! (`auth/oauth/{github_copilot,radius}.rs`, ports of
//! `packages/ai/src/auth/oauth/{github-copilot,radius}.ts` @ pi 0.82.1
//! 2efa728) wired into the W4 factory surface.
//!
//! Covered here (cross-module, through the public factory API):
//! - both factories expose the real flow (not the W4 `PendingOAuth` stub)
//!   under the upstream display name;
//! - the flow-produced credential shape (`availableModelIds` /
//!   `enterpriseUrl` extras) drives the factory's `filter_models` narrowing —
//!   the provider half of `github-copilot-oauth.test.ts`'s "filters models
//!   to the authenticated account picker catalog" (the runtime availability
//!   path is covered in rpi's `model_runtime.rs`
//!   `get_available_applies_copilot_filter_models`);
//! - `to_auth` derives the per-account Copilot base URL from the credential;
//! - a Radius device-code login runs end to end through the OAuth object the
//!   factory built for a custom gateway (mock gateway on loopback — no real
//!   network).
//!
//! Flow-level behavior (device-code polling, enterprise domains,
//! policy-enable, browser callback, error mappings) is covered by the
//! file-internal unit tests against mock endpoints.

use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::Json;
use rpi_ai::auth::oauth::{RadiusOAuthOptions, create_radius_oauth};
use rpi_ai::auth::types::BoxFutureSend;
use rpi_ai::auth::{
    AuthEvent, AuthInteraction, AuthPrompt, Credential, ModelsError, OAuthCredential,
};
use rpi_ai::models::Provider;
use rpi_ai::providers::github_copilot::github_copilot_provider;
use rpi_ai::providers::radius::{RadiusProviderOptions, radius_provider, radius_provider_with};
use serde_json::{Map, Value, json};

/// A credential exactly as `GitHubCopilotOAuth::login`/`refresh` produces it
/// (extras: `enterpriseUrl`, `availableModelIds`).
fn copilot_flow_credential(available_ids: Value) -> Credential {
    let mut extra = Map::new();
    extra.insert("enterpriseUrl".to_owned(), json!("company.ghe.com"));
    extra.insert("availableModelIds".to_owned(), available_ids);
    Credential::OAuth(OAuthCredential {
        refresh: "ghu_refresh_token".to_owned(),
        access: "tid=test;exp=9999999999;proxy-ep=proxy.individual.githubcopilot.com;".to_owned(),
        expires: i64::MAX,
        extra,
    })
}

/// The github-copilot factory now exposes the real device-code flow under
/// the upstream display name.
#[test]
fn copilot_factory_oauth_is_the_real_flow() {
    let provider = github_copilot_provider();
    let oauth = provider.auth().oauth.as_ref().expect("oauth");
    assert_eq!(oauth.name(), "GitHub Copilot");
}

/// The radius factory wires `RadiusOAuth` for the normalized gateway under
/// the provider display name (default and custom options).
#[test]
fn radius_factory_oauth_targets_the_normalized_gateway() {
    let provider = radius_provider();
    let oauth = provider.auth().oauth.as_ref().expect("oauth");
    assert_eq!(oauth.name(), "Radius");

    let provider = radius_provider_with(RadiusProviderOptions {
        id: Some("radius-eu".to_owned()),
        name: Some("Radius EU".to_owned()),
        gateway: Some("radius.eu.example.com/".to_owned()),
    });
    assert_eq!(provider.gateway(), "https://radius.eu.example.com");
    let oauth = provider.auth().oauth.as_ref().expect("oauth");
    assert_eq!(oauth.name(), "Radius EU");
}

/// `availableModelIds` written by the Copilot login/refresh narrows the
/// factory catalog via `filter_models` (intersection, catalog order kept);
/// malformed/absent lists leave the catalog untouched.
#[test]
fn copilot_available_model_ids_drive_factory_filtering() {
    let provider = github_copilot_provider();
    let models = provider.get_models();
    assert!(!models.is_empty());

    let kept = models[0].id.clone();
    let credential = copilot_flow_credential(json!([kept, "ghost-model"]));
    let filtered = provider.filter_models(models.clone(), Some(&credential));
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].id, models[0].id);

    // A flow credential without the picker list leaves the catalog as-is.
    let mut extra = Map::new();
    extra.insert("enterpriseUrl".to_owned(), json!("company.ghe.com"));
    let credential = Credential::OAuth(OAuthCredential {
        refresh: "r".to_owned(),
        access: "a".to_owned(),
        expires: i64::MAX,
        extra,
    });
    assert_eq!(
        provider.filter_models(models.clone(), Some(&credential)),
        models
    );
}

/// `toAuth` (github-copilot.ts:373-378) derives the per-account base URL:
/// the token's `proxy-ep` wins over the enterprise fallback.
#[tokio::test]
async fn copilot_to_auth_derives_per_account_base_url_through_the_factory() {
    let provider = github_copilot_provider();
    let oauth = provider.auth().oauth.as_ref().expect("oauth").clone();

    // proxy-ep present → api.individual.githubcopilot.com (token wins over
    // the enterprise fallback).
    let auth = oauth
        .to_auth(match &copilot_flow_credential(json!([])) {
            Credential::OAuth(credential) => credential,
            _ => unreachable!(),
        })
        .await
        .expect("to_auth");
    assert_eq!(
        auth.base_url.as_deref(),
        Some("https://api.individual.githubcopilot.com")
    );

    // No proxy-ep → the enterprise `copilot-api.` fallback.
    let mut extra = Map::new();
    extra.insert("enterpriseUrl".to_owned(), json!("company.ghe.com"));
    let credential = OAuthCredential {
        refresh: "r".to_owned(),
        access: "no-proxy-ep".to_owned(),
        expires: i64::MAX,
        extra,
    };
    let auth = oauth.to_auth(&credential).await.expect("to_auth");
    assert_eq!(
        auth.base_url.as_deref(),
        Some("https://copilot-api.company.ghe.com")
    );
}

// ---------------------------------------------------------------------------
// Radius device-code login through the factory-built OAuth object
// ---------------------------------------------------------------------------

struct FakeInteraction {
    events: Arc<Mutex<Vec<AuthEvent>>>,
}

impl AuthInteraction for FakeInteraction {
    fn prompt<'a>(&'a self, prompt: AuthPrompt) -> BoxFutureSend<'a, Result<String, ModelsError>> {
        assert!(
            matches!(prompt, AuthPrompt::Select { .. }),
            "unexpected prompt: {prompt:?}"
        );
        Box::pin(async move { Ok("device-code".to_owned()) })
    }

    fn notify(&self, event: AuthEvent) {
        self.events.lock().expect("lock").push(event);
    }
}

/// `createRadiusOAuth` + factory wiring: the OAuth object the provider
/// carries targets the provider's (normalized) gateway — a device-code login
/// against a loopback mock gateway completes end to end.
#[tokio::test]
async fn radius_device_login_runs_through_the_factory_oauth() {
    let requests: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let app = {
        let requests = requests.clone();
        axum::Router::new().fallback(move |request: Request<Body>| {
            let requests = requests.clone();
            async move {
                let path = request.uri().path().to_owned();
                requests.lock().expect("lock").push(path.clone());
                let body = match path.as_str() {
                    "/v1/oauth/device" => json!({
                        "device_code": "device-code",
                        "user_code": "ABCD-1234",
                        "verification_uri": "https://radius-ui.example/pair",
                        "expires_in": 600,
                        "interval": 5,
                    }),
                    "/v1/oauth/token" => json!({
                        "access_token": "access-token",
                        "refresh_token": "refresh-token",
                        "expires_in": 3600,
                        "scope": "gateway offline_access",
                    }),
                    other => panic!("unexpected request: {other}"),
                };
                (StatusCode::OK, Json(body))
            }
        })
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let gateway_url = format!("http://{addr}");

    let provider = radius_provider_with(RadiusProviderOptions {
        id: None,
        name: None,
        gateway: Some(gateway_url.clone()),
    });
    let oauth = provider.auth().oauth.as_ref().expect("oauth").clone();
    let events: Arc<Mutex<Vec<AuthEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let interaction = FakeInteraction {
        events: events.clone(),
    };

    let credential = oauth.login(&interaction, None).await.expect("login");
    assert_eq!(credential.access, "access-token");
    assert_eq!(credential.refresh, "refresh-token");
    assert_eq!(
        credential.extra.get("scope"),
        Some(&json!("gateway offline_access"))
    );

    assert_eq!(
        requests.lock().expect("lock").as_slice(),
        &["/v1/oauth/device", "/v1/oauth/token"]
    );
    assert_eq!(
        events.lock().expect("lock").as_slice(),
        &[AuthEvent::DeviceCode {
            user_code: "ABCD-1234".to_owned(),
            verification_uri: "https://radius-ui.example/pair".to_owned(),
            interval_seconds: Some(5),
            expires_in_seconds: Some(600),
        }]
    );

    // The standalone constructor normalizes the gateway identically.
    let standalone = create_radius_oauth(RadiusOAuthOptions {
        name: "Radius".to_owned(),
        gateway: "radius.example.com/".to_owned(),
    });
    assert_eq!(standalone.name(), "Radius");
}
