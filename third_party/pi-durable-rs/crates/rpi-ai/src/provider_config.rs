//! Compose machine-owned models.json entries over optional built-in providers.
//! Callers load configuration and register the resulting provider in Models.
//! Extracted from revpidev/rpi 7ee970aad1d825b30b9413ce251c98e610214e2b;
//! application paths, extension registration, and remote catalogs are excluded.

use std::sync::Arc;

use crate::api::anthropic_messages::AnthropicMessages;
use crate::api::lazy::create_setup_error_message_for_model;
use crate::api::openai_completions::OpenAiCompletions;
use crate::api::openai_responses::OpenAiResponses;
use crate::auth::{ModelsError, ModelsErrorCode, ProviderAuth};
use crate::models::{CreateProviderOptions, Provider, ProviderApi, ProviderStreams, create_provider};
use crate::models_json::ModelsJsonProvider;
use crate::types::{ApiKind, ErrorReason, Model, ProviderHeaders, StreamEvent};
use crate::utils::event_stream::AssistantMessageEventStream;

mod auth;
mod catalog;
mod delegation;
mod model_definition;
mod model_overlay;

use auth::{ConfigApiKeyAuth, wrap_api_key_auth, wrap_provider_auth};
use catalog::ComposedModels;
use delegation::{ModelOverridingProvider, RefreshDelegatingProvider};

/// Compose a checked models.json entry without resolving credentials or making requests.
/// Structural errors are ModelValidation; absent auth is Auth; unknown APIs are Provider.
/// A configured API key preserves the base OAuth method and stored-key precedence.
pub fn compose_configured_provider(
    provider_id: &str,
    base: Option<&Arc<dyn Provider>>,
    config: &ModelsJsonProvider,
) -> Result<Arc<dyn Provider>, ModelsError> {
    let name = config.name.clone()
        .or_else(|| base.map(|provider| provider.name().to_owned()))
        .unwrap_or_else(|| provider_id.to_owned());
    let base_url = config.base_url.clone()
        .or_else(|| base.and_then(|provider| provider.base_url().map(str::to_owned)));
    let configured_headers: Option<ProviderHeaders> = config.headers.as_ref().map(|headers| {
        headers.iter().map(|(name, value)| (name.clone(), Some(value.clone()))).collect()
    });
    let auth_header = config.auth_header.unwrap_or(false);
    let auth = match &config.api_key {
        Some(raw_key) => ProviderAuth {
            api_key: Some(wrap_api_key_auth(
                Arc::new(ConfigApiKeyAuth::new(
                    provider_id, raw_key.clone(),
                    base.and_then(|provider| provider.auth().api_key.clone()),
                )),
                &configured_headers, auth_header,
            )),
            oauth: base.and_then(|provider| provider.auth().oauth.clone()),
        },
        None => {
            let inherited = base.map(|provider| provider.auth().clone()).ok_or_else(|| {
                ModelsError::new(ModelsErrorCode::Auth,
                    format!("Provider {provider_id}: no authentication method configured."))
            })?;
            wrap_provider_auth(inherited, &configured_headers, auth_header)
        }
    };
    let composed = ComposedModels::new(provider_id, base.cloned(), Some(config.clone()));
    let models = composed.compose_eager()
        .map_err(|message| ModelsError::new(ModelsErrorCode::ModelValidation, message))?;
    let streams = match (&config.api, base) {
        (Some(api), _) => api_streams(api).ok_or_else(|| {
            ModelsError::new(ModelsErrorCode::Provider,
                format!("No API provider registered for api: {api}"))
        })?,
        (None, Some(base)) => {
            let auth = (config.api_key.is_some() || configured_headers.is_some() || auth_header)
                .then_some(auth);
            return Ok(Arc::new(ModelOverridingProvider {
                base: base.clone(), composed, name, base_url, auth,
            }));
        }
        (None, None) => return Err(ModelsError::new(ModelsErrorCode::Provider,
            format!("Provider {provider_id}: no api configured."))),
    };
    let inner = create_provider(CreateProviderOptions {
        id: provider_id.to_owned(), name: Some(name), base_url,
        headers: base.and_then(|provider| provider.headers().cloned()),
        auth, models, api: ProviderApi::Single(streams), ..Default::default()
    });
    Ok(match base {
        Some(base) => Arc::new(RefreshDelegatingProvider { base: base.clone(), inner, composed }),
        None => inner,
    })
}

fn api_streams(api: &str) -> Option<Arc<dyn ProviderStreams>> {
    match api {
        ApiKind::ANTHROPIC_MESSAGES => Some(Arc::new(AnthropicMessages)),
        ApiKind::OPENAI_COMPLETIONS => Some(Arc::new(OpenAiCompletions)),
        ApiKind::OPENAI_RESPONSES | ApiKind::AZURE_OPENAI_RESPONSES => Some(Arc::new(OpenAiResponses)),
        _ => None,
    }
}

fn unsupported_api_stream(model: &Model) -> AssistantMessageEventStream {
    let stream = AssistantMessageEventStream::new();
    let message = create_setup_error_message_for_model(model,
        &format!("No API provider registered for api: {}", model.api.as_str()));
    stream.push(StreamEvent::Error { reason: ErrorReason::Error, error: message.clone() });
    stream.end(Some(message));
    stream
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod catalog_tests;
#[cfg(test)]
mod transport_tests;
