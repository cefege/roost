//! Port of `packages/ai/src/providers/github-copilot.ts` @ pi 0.82.1
//! (2efa728) — GitHub Copilot: mixed 3-API dispatch on `model.api`
//! (`anthropic-messages` / `openai-completions` / `openai-responses`) plus
//! the credential-aware `filterModels` policy.
//!
//! W5 scope notes:
//! - OAuth (`loadGitHubCopilotOAuth`: device code, enterprise domains,
//!   per-account baseUrl, policy-enable) landed in T13 W5 as
//!   [`crate::auth::oauth::github_copilot`]; the login/refresh flows write
//!   `availableModelIds` onto the credential extras, which
//!   [`GithubCopilotProvider::filter_models`] consumes.
//! - `filterModels` hangs off the [`Provider`] trait (models.ts:111); its
//!   consumer is the model runtime's availability refresh
//!   (`crates/rpi/src/core/model_runtime.rs` `refresh_availability_inner`).
//!   The provider is a thin decorator over [`create_provider`] so the filter
//!   rides along without widening `CreateProviderOptions` mid-wave.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde_json::Value;

use crate::api::anthropic_messages::AnthropicMessages;
use crate::api::openai_completions::OpenAiCompletions;
use crate::api::openai_responses::OpenAiResponses;
use crate::auth::oauth::github_copilot_oauth;
use crate::auth::{Credential, ProviderAuth, env_api_key_auth};
use crate::generated::get_builtin_models;
use crate::models::{
    CreateProviderOptions, Provider, ProviderApi, ProviderStreams, create_provider,
};
use crate::types::{
    ApiKind, Model, ProviderHeaders, SimpleStreamOptions, StreamOptions, TranscriptContext,
};
use crate::utils::event_stream::AssistantMessageEventStream;

/// `githubCopilotProvider()`.
pub fn github_copilot_provider() -> Arc<dyn Provider> {
    Arc::new(GithubCopilotProvider {
        inner: create_provider(CreateProviderOptions {
            id: "github-copilot".to_owned(),
            name: Some("GitHub Copilot".to_owned()),
            base_url: Some("https://api.individual.githubcopilot.com".to_owned()),
            headers: None,
            auth: ProviderAuth {
                api_key: Some(Arc::new(env_api_key_auth(
                    "GitHub Copilot token",
                    &["COPILOT_GITHUB_TOKEN"],
                ))),
                oauth: Some(github_copilot_oauth()),
            },
            models: get_builtin_models("github-copilot").to_vec(),
            api: ProviderApi::Map(api_map()),
            ..Default::default()
        }),
    })
}

/// Mixed-API dispatch table (github-copilot.ts:28-33).
fn api_map() -> HashMap<String, Arc<dyn ProviderStreams>> {
    HashMap::from([
        (
            ApiKind::ANTHROPIC_MESSAGES.to_owned(),
            Arc::new(AnthropicMessages) as Arc<dyn ProviderStreams>,
        ),
        (
            ApiKind::OPENAI_COMPLETIONS.to_owned(),
            Arc::new(OpenAiCompletions) as Arc<dyn ProviderStreams>,
        ),
        (
            ApiKind::OPENAI_RESPONSES.to_owned(),
            Arc::new(OpenAiResponses) as Arc<dyn ProviderStreams>,
        ),
    ])
}

/// `filterModels` (github-copilot.ts:19-27): only OAuth credentials carry an
/// `availableModelIds` list; anything else (or a malformed list) leaves the
/// catalog untouched.
fn filter_models(models: Vec<Model>, credential: Option<&Credential>) -> Vec<Model> {
    let Some(Credential::OAuth(credential)) = credential else {
        return models;
    };
    let available = credential.extra.get("availableModelIds");
    let Some(ids) = available.and_then(|ids| ids.as_array()) else {
        return models;
    };
    if !ids.iter().all(Value::is_string) {
        return models;
    }
    let available: HashSet<&str> = ids.iter().filter_map(|id| id.as_str()).collect();
    models
        .into_iter()
        .filter(|model| available.contains(model.id.as_str()))
        .collect()
}

/// Decorator adding `filter_models` to the [`create_provider`] output.
struct GithubCopilotProvider {
    inner: Arc<dyn Provider>,
}

impl Provider for GithubCopilotProvider {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn name(&self) -> &str {
        self.inner.name()
    }

    fn base_url(&self) -> Option<&str> {
        self.inner.base_url()
    }

    fn headers(&self) -> Option<&ProviderHeaders> {
        self.inner.headers()
    }

    fn auth(&self) -> &ProviderAuth {
        self.inner.auth()
    }

    fn get_models(&self) -> Vec<Model> {
        self.inner.get_models()
    }

    fn filter_models(&self, models: Vec<Model>, credential: Option<&Credential>) -> Vec<Model> {
        filter_models(models, credential)
    }

    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<StreamOptions>,
    ) -> AssistantMessageEventStream {
        self.inner.stream(model, context, options)
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<SimpleStreamOptions>,
    ) -> Result<AssistantMessageEventStream, String> {
        self.inner.stream_simple(model, context, options)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Map;

    use super::*;
    use crate::auth::OAuthCredential;

    fn oauth_credential_with_available_ids(ids: Value) -> Credential {
        let mut extra = Map::new();
        extra.insert("availableModelIds".to_owned(), ids);
        Credential::OAuth(OAuthCredential {
            refresh: "r".to_owned(),
            access: "a".to_owned(),
            expires: i64::MAX,
            extra,
        })
    }

    #[test]
    fn api_map_covers_the_three_catalog_apis() {
        let map = api_map();
        for key in [
            ApiKind::ANTHROPIC_MESSAGES,
            ApiKind::OPENAI_COMPLETIONS,
            ApiKind::OPENAI_RESPONSES,
        ] {
            assert!(map.contains_key(key), "missing dispatch key {key}");
        }
        assert_eq!(map.len(), 3);
        // Every catalog model dispatches to a map entry.
        for model in get_builtin_models("github-copilot") {
            assert!(
                map.contains_key(model.api.as_str()),
                "catalog model {} has undispatched api {}",
                model.id,
                model.api
            );
        }
    }

    #[test]
    fn filter_models_only_narrows_for_oauth_with_valid_ids() {
        let provider = github_copilot_provider();
        let models = provider.get_models();
        assert!(!models.is_empty());

        // No credential / api-key credential / missing or malformed
        // availableModelIds: catalog untouched.
        assert_eq!(provider.filter_models(models.clone(), None), models);
        let api_key = Credential::ApiKey(crate::auth::ApiKeyCredential {
            key: Some("k".to_owned()),
            env: None,
        });
        assert_eq!(
            provider.filter_models(models.clone(), Some(&api_key)),
            models
        );
        let malformed = oauth_credential_with_available_ids(serde_json::json!("not-an-array"));
        assert_eq!(
            provider.filter_models(models.clone(), Some(&malformed)),
            models
        );
        let mixed_types = oauth_credential_with_available_ids(serde_json::json!(["gpt-4o", 42]));
        assert_eq!(
            provider.filter_models(models.clone(), Some(&mixed_types)),
            models
        );

        // Valid list narrows to the intersection, preserving catalog order.
        let first = models[0].id.clone();
        let valid = oauth_credential_with_available_ids(serde_json::json!([first, "ghost"]));
        let filtered = provider.filter_models(models.clone(), Some(&valid));
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].id, models[0].id);
    }
}
