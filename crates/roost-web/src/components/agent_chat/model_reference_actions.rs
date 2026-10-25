//! Catalog options and conversation-only configuration for the composer model
//! menu. It reuses role and search rules without altering persisted global roles.
use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{ModelEntry, ModelsCatalog};

use super::model_picker::{PickerOption, filter_options};
use crate::components::settings::agent_roles::role_options;
use crate::pump::Pump;

#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::agent_chat::ConfigureAgentChat;

pub(super) fn available_and_locked_models(catalog: &ModelsCatalog) -> Vec<ModelEntry> {
    let mut models: Vec<ModelEntry> = role_options(catalog, "default")
        .into_iter()
        .filter_map(|option| {
            let (provider, model_id) = option.selector.split_once('/')?;
            catalog
                .models
                .iter()
                .find(|model| {
                    model.provider == provider && model.model_id == model_id && model.available
                })
                .cloned()
        })
        .collect();
    models.extend(
        catalog
            .models
            .iter()
            .filter(|model| !model.available && !model.classifier)
            .cloned(),
    );
    models
}

pub(super) fn filter_model_rows(
    catalog: Option<&ModelsCatalog>,
    models: &[ModelEntry],
    query: &str,
) -> Vec<ModelEntry> {
    let options = model_search_options(catalog, models);
    filter_options(&options, query)
        .into_iter()
        .filter_map(|option| {
            models
                .iter()
                .find(|model| option.selector == format!("{}/{}", model.provider, model.model_id))
                .cloned()
        })
        .collect()
}

fn model_search_options(
    catalog: Option<&ModelsCatalog>,
    models: &[ModelEntry],
) -> Vec<PickerOption> {
    models
        .iter()
        .map(|model| {
            let provider = catalog
                .and_then(|catalog| {
                    catalog
                        .providers
                        .iter()
                        .find(|entry| entry.id == model.provider)
                })
                .map_or(model.provider.as_str(), |entry| entry.name.as_str());
            PickerOption {
                selector: format!("{}/{}", model.provider, model.model_id),
                name: model.name.clone(),
                group: provider.to_owned(),
                detail: None,
            }
        })
        .collect()
}

#[cfg_attr(not(target_arch = "wasm32"), allow(unused_variables, unused_mut))]
pub(super) fn configure_model(
    pump: Pump,
    conversation_id: String,
    selector: Option<String>,
    thinking_level: Option<String>,
    mut pending: Signal<bool>,
    mut error: Signal<String>,
) {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        let (model_provider, model_id) = match selector {
            Some(selector) => match selector.split_once('/') {
                Some((provider, model_id)) => {
                    (Some(provider.to_owned()), Some(model_id.to_owned()))
                }
                None => {
                    pending.set(false);
                    error.set("The selected model has an invalid identifier.".to_owned());
                    return;
                }
            },
            None => (
                thinking_level.is_none().then(String::new),
                thinking_level.is_none().then(String::new),
            ),
        };
        let call = ConfigureAgentChat {
            conversation_id,
            model_provider,
            model_id,
            thinking_level,
            ..Default::default()
        };
        match pump.rpc().call(&call).await {
            Ok(_) => error.set(String::new()),
            Err(failure) => {
                tracing::warn!(target: "agent_chat", %failure, "agent model configuration failed");
                error.set(failure.to_string());
            }
        }
        pending.set(false);
    });
}
