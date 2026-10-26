//! Integration-level chain proof for T04 part 2: a stored Anthropic OAuth
//! credential flows through `resolve_provider_auth` (store hit stops the
//! chain) into `OAuthAuth::to_auth`, producing `ModelAuth.api_key` — the
//! access token the anthropic-messages adapter then disguises as a Bearer
//! Claude Code request (adapter-side coverage lives in
//! `api/anthropic_messages.rs` tests).

use std::sync::Arc;

use rpi_ai::auth::{
    AuthContext, Credential, CredentialStore, DefaultAuthContext, InMemoryCredentialStore,
    OAuthCredential, ProviderAuth, anthropic_oauth, resolve_provider_auth,
};
use serde_json::Map;

#[tokio::test]
async fn resolve_provider_auth_with_stored_oauth_credential_derives_api_key() {
    let store: Arc<dyn CredentialStore> = Arc::new(InMemoryCredentialStore::new());
    // Seed a valid (non-expired) OAuth credential via the only write path.
    store
        .modify(
            "anthropic",
            Arc::new(|_current| {
                Box::pin(async {
                    Ok(Some(Credential::OAuth(OAuthCredential {
                        refresh: "refresh-token".to_owned(),
                        access: "sk-ant-oat01-token".to_owned(),
                        expires: i64::MAX,
                        extra: Map::new(),
                    })))
                })
            }),
            None,
        )
        .await
        .expect("seed credential");

    let auth = ProviderAuth {
        api_key: None,
        oauth: Some(anthropic_oauth()),
    };
    let auth_context: Arc<dyn AuthContext> = Arc::new(DefaultAuthContext);

    let result = resolve_provider_auth("anthropic", &auth, &store, &auth_context, None)
        .await
        .expect("resolve")
        .expect("stored credential resolves");

    assert_eq!(result.auth.api_key.as_deref(), Some("sk-ant-oat01-token"));
    assert_eq!(result.source.as_deref(), Some("OAuth"));
}
