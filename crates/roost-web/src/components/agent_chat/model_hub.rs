//! The models hub `/model` opens: a sidebar of views (recent models, roles,
//! all models, each provider) beside the chosen view. A model view switches
//! this chat's model; Roles edits the all-folders role chains and presets; a
//! provider not signed in offers its sign-in. Mounted by `agent_chat::surface`.

pub mod hub_logic;
mod hub_presets;

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{
    AgentSettingsView, ConversationSummary, ModelsCatalog, ProviderEntry,
};

use self::hub_logic::{HubView, hub_options, hub_sections, initial_view};
use self::hub_presets::HubPresets;
use super::model_controls::{Configure, configure};
use super::model_picker::ModelPicker;
use crate::components::md::{Button, ButtonSize, ButtonVariant, Dialog, List, ListRow};
use crate::components::settings::agent_roles::{AgentRoles, load_settings};
use crate::pump::Pump;

#[component]
pub fn ModelHub(
    open: Signal<bool>,
    conversation: ConversationSummary,
    catalog: Option<ModelsCatalog>,
    pump: Pump,
    connect_provider: Signal<Option<ProviderEntry>>,
) -> Element {
    let mut open = open;
    let mut connect_provider = connect_provider;
    let mut view = use_signal(|| None::<HubView>);
    let settings = use_signal(AgentSettingsView::default);
    let mut error = use_signal(String::new);
    let load_pump = pump.clone();
    use_effect(move || {
        if open() {
            view.set(None);
            error.set(String::new());
            load_settings(load_pump.clone(), settings, error);
        }
    });
    if !open() {
        return rsx! {};
    }
    let close = move |()| open.set(false);
    let Some(catalog) = catalog else {
        return rsx! {
            Dialog { open: true, on_close: close, headline: "Models".to_owned(),
                class: "roost-dialog--wide model-hub".to_owned(), test_id: "agent-model-hub".to_owned(),
                p { class: "model-hub__empty", "Loading models…" }
            }
        };
    };
    let sections = hub_sections(&catalog);
    let current = view().unwrap_or_else(|| initial_view(&catalog));
    let locked = sections
        .iter()
        .find(|section| section.view == current && section.locked)
        .cloned();
    let content = match (&current, locked) {
        (HubView::Roles, _) => rsx! {
            AgentRoles { catalog: catalog.clone(), settings, error }
            HubPresets { settings, error, pump: pump.clone() }
        },
        (HubView::Provider(id), Some(section)) => {
            let provider = catalog
                .providers
                .iter()
                .find(|provider| provider.id == *id)
                .cloned();
            let count = section.count.unwrap_or_default();
            rsx! {
                p { class: "model-hub__empty",
                    "{section.label} is not signed in. Sign in to use its {count} models."
                }
                Button {
                    variant: ButtonVariant::Default,
                    size: ButtonSize::Sm,
                    icon: "add_link",
                    "data-testid": "agent-model-hub-sign-in",
                    onclick: move |_| {
                        connect_provider.set(provider.clone());
                        open.set(false);
                    },
                    "Sign in"
                }
            }
        }
        (view, _) => {
            let options = hub_options(&catalog, view, &settings());
            let selected = conversation
                .effective_model
                .as_ref()
                .or(conversation.model.as_ref())
                .map(|model| format!("{}/{}", model.provider, model.model_id));
            let conversation_id = conversation.id.clone();
            let choose_pump = pump.clone();
            if options.is_empty() && *view == HubView::Recent {
                rsx! {
                    p { class: "model-hub__empty",
                        "No recent models yet. Models you switch a chat to show up here."
                    }
                }
            } else {
                rsx! {
                    ModelPicker {
                        key: "{view.id()}",
                        options,
                        selected,
                        placeholder: "Search models",
                        test_id: "agent-model-hub-picker",
                        on_choose: move |selector: String| {
                            configure(choose_pump.clone(), conversation_id.clone(), Configure::Model(Some(selector)));
                            open.set(false);
                        },
                    }
                }
            }
        }
    };
    rsx! {
        Dialog {
            open: true,
            on_close: close,
            headline: "Models".to_owned(),
            description: rsx! { "Pick this chat's model, or set the roles every chat starts from." },
            class: "roost-dialog--wide model-hub".to_owned(),
            test_id: "agent-model-hub".to_owned(),
            div { class: "model-hub__layout",
                nav { class: "model-hub__sidebar", "aria-label": "Model views",
                    List {
                        for section in sections {
                            {
                                let target = section.view.clone();
                                rsx! {
                                    ListRow {
                                        key: "{section.view.id()}",
                                        headline: rsx! { "{section.label}" },
                                        trailing: section.count.map(|count| rsx! { span { class: "model-hub__count", "{count}" } }),
                                        selected: section.view == current,
                                        class: section.locked.then(|| "model-hub__locked".to_owned()),
                                        test_id: format!("agent-model-hub-view-{}", section.view.id()),
                                        onclick: move |_| view.set(Some(target.clone())),
                                    }
                                }
                            }
                        }
                    }
                }
                div { class: "model-hub__content", {content} }
            }
            if !error().is_empty() {
                p { class: "model-hub__error", role: "alert", "{error}" }
            }
        }
    }
}
