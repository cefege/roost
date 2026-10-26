//! Registers Roost's typed model catalog with Pi's provider collection.
//! Chat and classifier APIs share native credential availability, while calls
//! remain on the existing account-aware transport adapters.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use rpi_ai::auth::ProviderAuth;
use rpi_ai::models::{
    CreateProviderOptions, Models, ProviderApi, ProviderStreams, create_provider,
};
use rpi_ai::types::{AnyModel, Model, ProviderClassifier};

use crate::error::AgentError;
use crate::runtime::AgentRuntime;

use super::super::state::ProjectionState;
use super::RoostProvider;
use super::model_auth::NativeModelAuth;
use super::model_catalog::{pi_classifier, pi_model};
use super::model_classifier::NativeClassifier;
/// Build the real Pi model collection from Roost's supported catalog.
pub(crate) fn models(
    runtime: AgentRuntime,
    conversation_id: String,
    projection: Arc<ProjectionState>,
) -> Result<Models, AgentError> {
    let collection = Models::new(None);
    let catalog = runtime.inner.llm.catalog();
    let mut provider_models: HashMap<String, Vec<Model>> = HashMap::new();
    let mut provider_catalog: HashMap<String, Vec<AnyModel>> = HashMap::new();
    let mut classifier_apis: HashMap<String, HashSet<String>> = HashMap::new();
    for info in catalog.models() {
        let all_models = provider_catalog.entry(info.provider.clone()).or_default();
        match info.kind {
            roost_llm::ModelKind::Chat => {
                let model = pi_model(info);
                provider_models
                    .entry(info.provider.clone())
                    .or_default()
                    .push(model.clone());
                all_models.push(AnyModel::Chat(model));
            }
            roost_llm::ModelKind::Classifier => {
                all_models.push(AnyModel::Classifier(pi_classifier(info)));
                let api = match info.api {
                    roost_llm::WireApi::AnthropicMessages => "anthropic-messages",
                    roost_llm::WireApi::OpenAiCodexResponses => "openai-codex-responses",
                    roost_llm::WireApi::OpenAiCompletions => "openai-completions",
                    roost_llm::WireApi::TypesafeSystemOne => "typesafe-system-one",
                };
                classifier_apis
                    .entry(info.provider.clone())
                    .or_default()
                    .insert(api.to_owned());
            }
        }
    }
    for (provider_id, all_models) in provider_catalog {
        let provider_models = provider_models.remove(&provider_id).unwrap_or_default();
        let classifiers = classifier_apis
            .remove(&provider_id)
            .unwrap_or_default()
            .into_iter()
            .map(|api| {
                (
                    api,
                    Arc::new(NativeClassifier::new(runtime.clone())) as Arc<dyn ProviderClassifier>,
                )
            })
            .collect();
        let streams: Arc<dyn ProviderStreams> = Arc::new(RoostProvider {
            runtime: runtime.clone(),
            conversation_id: conversation_id.clone(),
            projection: Arc::clone(&projection),
        });
        let provider = create_provider(CreateProviderOptions {
            id: provider_id.clone(),
            name: Some(provider_id.clone()),
            auth: ProviderAuth {
                api_key: Some(NativeModelAuth::new(runtime.clone(), provider_id)),
                oauth: None,
            },
            models: provider_models,
            all_models,
            classifiers,
            api: ProviderApi::Single(streams),
            ..Default::default()
        });
        collection.set_provider(provider);
    }
    Ok(collection)
}
