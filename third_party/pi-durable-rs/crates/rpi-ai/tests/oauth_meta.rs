//! V15-15 integration tests: the `meta` OAuth flow (`auth/oauth/meta.rs`,
//! port of `packages/ai/src/auth/oauth/meta.ts` @ pi 0.86.1 `b73412a37`,
//! #9096) wired into the provider factory surface.
//!
//! Covered here (cross-module, through the public API — the
//! `oauth_kimi_xai.rs` precedent):
//! - the factory exposes the real flow under the upstream display name;
//! - `to_auth` maps the minted key to the request api key (no bearer
//!   header);
//! - the load.ts counterpart (`auth/oauth/load.rs`) resolves the flow by
//!   provider id.
//!
//! Flow-level behavior (device authorization, identity token poll, key
//! mint, refresh re-mint, 401/403 → `/login meta`) is covered by the
//! file-internal unit tests in `auth/oauth/meta.rs`.

use rpi_ai::auth::OAuthCredential;
use rpi_ai::auth::oauth::load::load_oauth_flow;
use rpi_ai::providers::meta::meta_provider;
use serde_json::Map;

/// The meta factory exposes the real device-code flow under the upstream
/// display name (`metaOAuth.name`, meta.ts @ b73412a37).
#[test]
fn meta_factory_oauth_is_the_real_flow() {
    let provider = meta_provider();
    let oauth = provider.auth().oauth.as_ref().expect("oauth");
    assert_eq!(oauth.name(), "Meta (Muse subscription)");
    assert!(oauth.is_subscription());
}

/// `toAuth: { apiKey: credential.access }` through the factory-built
/// object (upstream meta-oauth.test.ts test 4).
#[tokio::test]
async fn meta_to_auth_maps_the_minted_key_to_an_api_key() {
    let provider = meta_provider();
    let oauth = provider.auth().oauth.as_ref().expect("oauth");
    let auth = oauth
        .to_auth(&OAuthCredential {
            refresh: "identity-token".to_owned(),
            access: "LLM|key".to_owned(),
            expires: 1,
            extra: Map::new(),
        })
        .await
        .expect("to_auth");
    assert_eq!(auth.api_key.as_deref(), Some("LLM|key"));
    assert!(auth.headers.is_none());
}

/// The load registry resolves the meta flow by provider id
/// (`loadMetaOAuth`, load.ts @ b73412a37).
#[test]
fn load_registry_resolves_meta() {
    let flow = load_oauth_flow("meta").expect("meta loader");
    assert_eq!(flow.name(), "Meta (Muse subscription)");
    assert!(flow.is_subscription());
}
