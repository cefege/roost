//! Renders pending ACP forms and permission choices from OMP as explicit user decisions.
//! The transcript owns pending lifetime; this card owns only draft values and request errors.
//! Raw ACP schema stays intact in the wire request and is interpreted here without native-plan rules.

use crate::components::agent_chat::agent_alert::AgentAlert;
use crate::components::ui::{Button, ButtonSize, ButtonVariant, Card, CardSize};
use crate::pump::Pump;
use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::OmpInteraction;
use serde_json::{Map, Value, json};

#[component]
pub(super) fn OmpInteractionCard(
    conversation_id: String,
    interaction: OmpInteraction,
    pump: Pump,
) -> Element {
    let content = use_signal(|| initial_values(&interaction.request));
    let mut pending = use_signal(|| false);
    let mut error = use_signal(|| None::<String>);
    let mut submission = use_signal(|| None::<(String, Value, String)>);
    let request = &interaction.request;
    let is_permission = request.get("kind").and_then(Value::as_str) == Some("permission");
    let message = request
        .get("message")
        .or_else(|| request.pointer("/toolCall/title"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let properties = request
        .pointer("/requestedSchema/properties")
        .and_then(Value::as_object);
    let required = request
        .pointer("/requestedSchema/required")
        .and_then(Value::as_array);
    let choices = request
        .get("options")
        .or_else(|| request.pointer("/permission/options"))
        .and_then(Value::as_array);
    let mut fields = properties
        .into_iter()
        .flatten()
        .filter(|(name, _)| !name.ends_with("__other"));
    let message_is_field_title = !is_permission
        && fields
            .next()
            .and_then(|(_, schema)| schema.get("title"))
            .and_then(Value::as_str)
            .is_some_and(|title| title.trim() == message.trim())
        && fields.next().is_none();
    let on_answer = EventHandler::new({
        let pump = pump.clone();
        let conversation_id = conversation_id.clone();
        let item_id = interaction.id.clone();
        move |(action, value): (String, Value)| {
            if pending() {
                return;
            }
            pending.set(true);
            error.set(None);
            let request_id = submission
                .peek()
                .as_ref()
                .filter(|(previous_action, previous_value, _)| {
                    previous_action == &action && previous_value == &value
                })
                .map(|(_, _, request_id)| request_id.clone())
                .unwrap_or_else(super::request_ids::new_request_id);
            submission.set(Some((action.clone(), value.clone(), request_id.clone())));
            let pump = pump.clone();
            let conversation_id = conversation_id.clone();
            let item_id = item_id.clone();
            spawn(async move {
                #[cfg(target_arch = "wasm32")]
                let result = pump
                    .rpc()
                    .call(
                        &roost_client_core::client::rpc::calls::agent_chat::RespondOmpInteraction {
                            conversation_id,
                            item_id,
                            action,
                            content_json: value.to_string(),
                            request_id,
                        },
                    )
                    .await
                    .map_err(|failure| failure.to_string());
                #[cfg(not(target_arch = "wasm32"))]
                let result: Result<(), String> = {
                    let _ = (pump, conversation_id, item_id, action, value, request_id);
                    Err("OMP questions are available in the browser.".into())
                };
                if let Err(failure) = result {
                    pending.set(false);
                    error.set(Some(failure));
                }
            });
        }
    });
    let valid_content = use_memo(use_reactive((&interaction,), move |(interaction,)| {
        serde_json::to_string(&*content.read()).is_ok_and(|json| {
            roost_protocol::wire::agent_chat::validate_omp_interaction_response(
                &interaction,
                &interaction.id,
                "accept",
                &json,
            )
            .is_ok()
        })
    }));
    rsx! {
        Card { size: CardSize::Sm, class: Some("w-full gap-3 p-4".to_owned()),
            if !message.is_empty() && !message_is_field_title {
                crate::components::ui::Markdown {
                    html: super::markdown::markdown_to_safe_html(message),
                    size: crate::components::ui::MarkdownSize::Sm,
                }
            }
            if is_permission {
                div { class: "flex flex-wrap gap-2",
                    for (index, choice) in choices.into_iter().flatten().enumerate() {
                        { let label = choice.get("name").or_else(|| choice.get("title")).or_else(|| choice.get("optionId")).and_then(Value::as_str).unwrap_or("Allow").to_owned();
                          let option = choice.get("optionId").or_else(|| choice.get("id")).and_then(Value::as_str).unwrap_or_default().to_owned();
                          rsx! { Button { key: "{index}", class: "h-auto max-w-full whitespace-normal text-left", size: ButtonSize::Sm, disabled: pending(), onclick: move |_| on_answer.call(("permission".to_owned(), json!({"optionId": option}))), "{label}" } } }
                    }
                }
            } else {
                for (name, schema) in properties.into_iter().flatten() {
                    super::omp_form_field::OmpFormField {
                        key: "{name}",
                        name: name.clone(),
                        schema: schema.clone(),
                        required: required.is_some_and(|required| required.iter().any(|entry| entry.as_str() == Some(name))),
                        content,
                        disabled: pending(),
                    }
                }
            }
            if let Some(message) = error() { AgentAlert { test_id: "omp-interaction-error", "{message}" } }
            div { class: "flex flex-wrap justify-end gap-2",
                Button { variant: ButtonVariant::Outline, size: ButtonSize::Sm, disabled: pending(), onclick: move |_| on_answer.call(("cancel".to_owned(), Value::Null)), "Cancel" }
                if !is_permission {
                    Button { variant: ButtonVariant::Destructive, size: ButtonSize::Sm, disabled: pending(), onclick: move |_| on_answer.call(("decline".to_owned(), Value::Null)), "Decline" }
                    Button { size: ButtonSize::Sm, disabled: pending() || !valid_content(), onclick: move |_| on_answer.call(("accept".to_owned(), Value::Object(content()))), "Submit" }
                }
            }
        }
    }
}

fn initial_values(request: &Value) -> Map<String, Value> {
    let mut result = Map::new();
    if let Some(properties) = request
        .pointer("/requestedSchema/properties")
        .and_then(Value::as_object)
    {
        for (name, schema) in properties {
            if let Some(value) = schema.get("default") {
                result.insert(name.clone(), value.clone());
            } else if let Some(value) = schema
                .get("enum")
                .and_then(Value::as_array)
                .or_else(|| schema.get("oneOf").and_then(Value::as_array))
                .and_then(|values| values.first())
            {
                result.insert(
                    name.clone(),
                    value.get("const").cloned().unwrap_or_else(|| value.clone()),
                );
            } else if schema.get("type").and_then(Value::as_str) == Some("boolean") {
                result.insert(name.clone(), Value::Bool(false));
            } else if schema.get("type").and_then(Value::as_str) == Some("array") {
                result.insert(name.clone(), json!([]));
            }
        }
    }
    result
}
