//! T13 W5 integration tests: the `openai-codex` and `openrouter` OAuth flows
//! (`auth/oauth/{openai_codex,openrouter}.rs`, ports of
//! `packages/ai/src/auth/oauth/{openai-codex,openrouter}.ts` @ pi 0.82.1
//! 2efa728) wired into the W4 factory surface.
//!
//! Covered here (cross-module, through the public factory API):
//! - the openai-codex factory exposes the real OAuth-only flow (not the W4
//!   empty-`ProviderAuth` placeholder) under the upstream display name;
//! - the openrouter factory exposes the real flow (not the W4 `PendingOAuth`
//!   stub) alongside the API-key channel;
//! - `to_auth` maps the access token / permanent key to `api_key`;
//! - openrouter's `refresh` is a no-op that returns the credential unchanged
//!   (permanent key — no network).
//!
//! Flow-level behavior (PKCE callback, deviceauth bypass, poll mapping,
//! refresh, manual-code fallbacks) is covered by the file-internal unit
//! tests against mock endpoints.

use rpi_ai::auth::OAuthCredential;
use rpi_ai::providers::openai_codex::openai_codex_provider;
use rpi_ai::providers::openrouter::openrouter_provider;
use serde_json::Map;

/// The openai-codex factory now exposes the real OAuth-only flow under the
/// upstream display name (D-030 closed).
#[test]
fn codex_factory_oauth_is_the_real_flow() {
    let provider = openai_codex_provider();
    // Upstream has no api-key channel (`lazyOAuth` only).
    assert!(provider.auth().api_key.is_none());
    let oauth = provider.auth().oauth.as_ref().expect("oauth");
    assert_eq!(oauth.name(), "OpenAI (ChatGPT Plus/Pro)");
}

/// `toAuth` (openai-codex.ts:535-537) maps the access token to `api_key`.
#[tokio::test]
async fn codex_to_auth_maps_access_to_api_key_through_the_factory() {
    let provider = openai_codex_provider();
    let oauth = provider.auth().oauth.as_ref().expect("oauth").clone();
    let credential = OAuthCredential {
        refresh: "r".to_owned(),
        access: "codex-access-token".to_owned(),
        expires: i64::MAX,
        extra: Map::new(),
    };
    let auth = oauth.to_auth(&credential).await.expect("to_auth");
    assert_eq!(auth.api_key.as_deref(), Some("codex-access-token"));
}

/// The openrouter factory exposes the real flow (not the W4 `PendingOAuth`
/// stub) under the upstream display name, alongside the API-key channel.
#[test]
fn openrouter_factory_oauth_is_the_real_flow() {
    let provider = openrouter_provider();
    assert!(provider.auth().api_key.is_some(), "api-key channel stays");
    let oauth = provider.auth().oauth.as_ref().expect("oauth");
    assert_eq!(oauth.name(), "OpenRouter OAuth");
}

/// `refresh` (openrouter.ts:305-307) is a no-op — the permanent key
/// credential comes back unchanged, with no network involved.
#[tokio::test]
async fn openrouter_refresh_is_a_noop_through_the_factory() {
    let provider = openrouter_provider();
    let oauth = provider.auth().oauth.as_ref().expect("oauth").clone();
    let credential = OAuthCredential {
        refresh: String::new(),
        access: "sk-or-permanent".to_owned(),
        expires: 9_007_199_254_740_991, // Number.MAX_SAFE_INTEGER
        extra: Map::new(),
    };
    let refreshed = oauth.refresh(&credential, None).await.expect("refresh");
    assert_eq!(refreshed, credential);
}

/// `toAuth` (openrouter.ts:308-310) maps the permanent key to `api_key`.
#[tokio::test]
async fn openrouter_to_auth_maps_access_to_api_key_through_the_factory() {
    let provider = openrouter_provider();
    let oauth = provider.auth().oauth.as_ref().expect("oauth").clone();
    let credential = OAuthCredential {
        refresh: String::new(),
        access: "sk-or-permanent".to_owned(),
        expires: i64::MAX,
        extra: Map::new(),
    };
    let auth = oauth.to_auth(&credential).await.expect("to_auth");
    assert_eq!(auth.api_key.as_deref(), Some("sk-or-permanent"));
}
