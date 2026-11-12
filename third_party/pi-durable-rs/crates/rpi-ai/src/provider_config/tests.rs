//! Behavioral coverage of configured provider validation and authentication.
//! Exercises concrete built-in auth and Models resolution without live providers.
//! Adapted from model_runtime.rs at revpidev/rpi 7ee970aad1d825b30b9413ce251c98e610214e2b.

use std::collections::HashMap;
use std::sync::Arc;

use crate::auth::{ApiKeyCredential, AuthContext, Credential, CredentialStore,
    InMemoryCredentialStore, ModelsErrorCode, OAuthCredential};
use crate::models::{CreateModelsOptions, Models, Provider};
use crate::models_json::ModelsJsonProvider;
use crate::providers::anthropic::anthropic_provider;
use crate::types::ModelType;
use serde_json::{Value, json};

use super::compose_configured_provider;

#[derive(Default)]
pub(super) struct MapAuthContext(pub HashMap<String, String>);

#[async_trait::async_trait]
impl AuthContext for MapAuthContext {
    async fn env(&self, name: &str) -> Option<String> {
        self.0.get(name).cloned()
    }
    async fn file_exists(&self, _path: &str) -> bool {
        false
    }
}

pub(super) fn config(value: Value) -> ModelsJsonProvider {
    serde_json::from_value(value).expect("provider config")
}

pub(super) fn custom_config() -> Value {
    json!({"api":"openai-completions", "apiKey":"configured-key",
        "baseUrl":"https://example.invalid/v1", "models":[{"id":"custom-coder"}]})
}

pub(super) fn collection(provider: Arc<dyn Provider>, store: Arc<InMemoryCredentialStore>) -> Models {
    let models = Models::new(Some(CreateModelsOptions {
        credentials: Some(store), auth_context: Some(Arc::new(MapAuthContext::default())),
        models_store: None,
    }));
    models.set_provider(provider);
    models
}

pub(super) async fn seed(store: &InMemoryCredentialStore, provider: &str, credential: Credential) {
    store.modify(provider, Arc::new(move |_| {
        let credential = credential.clone();
        Box::pin(async move { Ok(Some(credential)) })
    }), None).await.expect("seed credential");
}

#[tokio::test]
async fn explicit_custom_model_is_available_and_literal_key_resolves() {
    let provider = compose_configured_provider("local", None, &config(custom_config())).expect("compose");
    let models = collection(provider, Arc::new(InMemoryCredentialStore::new()));
    let available = models.get_available_of_type(ModelType::Chat, None).await.expect("available");
    assert_eq!(available.len(), 1);
    let model = available[0].as_chat().expect("chat");
    assert_eq!(model.id, "custom-coder");
    assert_eq!(model.context_window, 128000);
    assert_eq!(model.max_tokens, 16384);
    let auth = models.get_auth(model, None).await.expect("resolve").expect("configured");
    assert_eq!(auth.auth.api_key.as_deref(), Some("configured-key"));
}

#[test]
fn missing_and_unsupported_apis_are_explicit_errors() {
    for (api, fragment, code) in [
        (None, "no \"api\" specified", ModelsErrorCode::ModelValidation),
        (Some("not-an-adapter"), "No API provider registered", ModelsErrorCode::Provider),
    ] {
        let mut value = custom_config();
        match api {
            Some(api) => value["api"] = json!(api),
            None => { value.as_object_mut().expect("object").remove("api"); }
        }
        let error = compose_configured_provider("local", None, &config(value)).err().expect("reject");
        assert_eq!(error.code, code);
        assert!(error.message.contains(fragment), "{}", error.message);
    }
    let value = json!({"apiKey":"key", "models":[{"id":"model", "api":"openai-completions", "baseUrl":"https://example.invalid"}]});
    let error = compose_configured_provider("local", None, &config(value)).err().expect("provider api required");
    assert!(error.message.contains("no api configured"));
}

#[test]
fn structural_errors_and_missing_auth_fail_at_composition() {
    let base = anthropic_provider();
    for value in [json!({"oauth":"radius"}), json!({"name":"Name only"})] {
        let error = compose_configured_provider("anthropic", Some(&base), &config(value)).err().expect("reject");
        assert_eq!(error.code, ModelsErrorCode::ModelValidation);
    }
    let mut value = custom_config();
    value.as_object_mut().expect("object").remove("baseUrl");
    assert!(compose_configured_provider("local", None, &config(value)).err().expect("base URL required").message.contains("baseUrl"));
    let mut value = custom_config();
    value.as_object_mut().expect("object").remove("apiKey");
    assert_eq!(compose_configured_provider("local", None, &config(value)).err().expect("auth required").code, ModelsErrorCode::Auth);
    for field in ["contextWindow", "maxTokens"] {
        let mut value = custom_config();
        value["models"][0][field] = json!(0);
        assert_eq!(compose_configured_provider("local", None, &config(value)).err().expect("positive bounds").code, ModelsErrorCode::ModelValidation);
    }
}

#[tokio::test]
async fn stored_key_wins_over_missing_config_env_with_and_without_native_auth() {
    let store = Arc::new(InMemoryCredentialStore::new());
    for base in [None, Some(anthropic_provider())] {
        let provider_id = if base.is_some() { "anthropic" } else { "local" };
        let mut value = custom_config();
        value["apiKey"] = json!("${RPI_COMPOSER_MISSING_KEY}");
        let provider = compose_configured_provider(provider_id, base.as_ref(), &config(value)).expect("compose");
        seed(&store, provider_id, Credential::ApiKey(ApiKeyCredential {
            key: Some("stored-key".to_owned()), env: None,
        })).await;
        let models = collection(provider, store.clone());
        let model = models.get_model(provider_id, "custom-coder").expect("model");
        let auth = models.get_auth(&model, None).await.expect("resolve").expect("stored");
        assert_eq!(auth.auth.api_key.as_deref(), Some("stored-key"));
    }
}

#[tokio::test]
async fn configured_key_preserves_native_oauth_resolution() {
    let base = anthropic_provider();
    let provider = compose_configured_provider("anthropic", Some(&base), &config(json!({"apiKey":"configured-key"}))).expect("compose");
    let store = Arc::new(InMemoryCredentialStore::new());
    seed(&store, "anthropic", Credential::OAuth(OAuthCredential {
        refresh: "refresh-token".to_owned(), access: "oauth-access".to_owned(),
        expires: i64::MAX, extra: Default::default(),
    })).await;
    let models = collection(provider, store);
    let model = models.get_models(Some("anthropic")).remove(0);
    let auth = models.get_auth(&model, None).await.expect("resolve").expect("OAuth");
    assert_eq!(auth.auth.api_key.as_deref(), Some("oauth-access"));
}

#[tokio::test]
async fn configured_headers_preserve_missing_env_check_boundary() {
    for auth_header in [false, true] {
        let mut value = custom_config();
        value["apiKey"] = json!("${RPI_COMPOSER_MISSING_KEY}");
        value["headers"] = json!({"X-Configured":"value"});
        value["authHeader"] = json!(auth_header);
        let provider = compose_configured_provider("local", None, &config(value)).expect("compose");
        let auth = provider.auth().api_key.as_ref().expect("API key auth");
        assert!(auth.has_check());
        assert!(auth.check(&MapAuthContext::default(), None).await.expect("check must not resolve").is_none());
        assert!(auth.resolve(&MapAuthContext::default(), None).await.is_err());
        let present = MapAuthContext(HashMap::from([("RPI_COMPOSER_MISSING_KEY".to_owned(), "env-key".to_owned())]));
        assert!(auth.check(&present, None).await.expect("check").is_some());
        let resolved = auth.resolve(&present, None).await.expect("resolve").expect("configured");
        assert_eq!(resolved.auth.api_key.as_deref(), Some("env-key"));
        let headers = resolved.auth.headers.expect("configured headers");
        assert_eq!(headers.get("X-Configured").and_then(Option::as_deref), Some("value"));
        assert_eq!(headers.get("Authorization").and_then(Option::as_deref), auth_header.then_some("Bearer env-key"));
    }
}
