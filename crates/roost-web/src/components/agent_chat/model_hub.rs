//! The models hub `/model` opens: views (recent models, roles, all models,
//! each provider) and the chosen view. A model view switches this chat's
//! model; Roles edits the all-folders role chains and presets; a provider not
//! signed in offers its sign-in. On a wide screen the views are a sidebar
//! beside the content in a dialog; on a phone the hub is a full-height bottom
//! sheet that opens on the chosen view, with Back to the list of views.
//! Mounted by `agent_chat::surface`.

pub mod hub_logic;
mod hub_nav;
mod hub_presets;

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{
    AgentSettingsView, ConversationSummary, ModelsCatalog, ProviderEntry,
};

use self::hub_logic::{HubSection, HubView, hub_options, hub_sections, initial_view, role_leaders};
use self::hub_nav::{HubNav, nav_row_id};
use self::hub_presets::HubPresets;
use super::model_controls::{Configure, configure};
use super::model_picker::ModelPicker;
use crate::components::file_viewer::dom::focus_by_id;
use crate::components::layout::window_size::use_is_compact;
use crate::components::md::{
    Button, ButtonSize, ButtonVariant, Dialog, EmptyState, IconButton, IconButtonSize, Skeleton,
};
use crate::components::settings::agent_roles::{AgentRoles, load_settings};
use crate::pump::Pump;

/// The phone detail heading's id, focused when a view is entered from the list.
const DETAIL_HEADING_ID: &str = "agent-model-hub-detail-heading";

#[component]
pub fn ModelHub(
    open: Signal<bool>,
    conversation: ConversationSummary,
    catalog: Option<ModelsCatalog>,
    pump: Pump,
    connect_provider: Signal<Option<ProviderEntry>>,
) -> Element {
    let mut open = open;
    let compact = use_is_compact();
    let mut view = use_signal(|| None::<HubView>);
    // Phone only: the list of views is showing instead of one view.
    let mut listing = use_signal(|| false);
    // Where focus goes once the next screen has rendered.
    let mut focus_target = use_signal(|| None::<String>);
    let settings = use_signal(AgentSettingsView::default);
    let mut error = use_signal(String::new);
    let load_pump = pump.clone();
    use_effect(move || {
        if open() {
            view.set(None);
            listing.set(false);
            error.set(String::new());
            load_settings(load_pump.clone(), settings, error);
        }
    });
    use_effect(move || {
        if let Some(id) = focus_target() {
            focus_by_id(&id);
            focus_target.set(None);
        }
    });
    if !open() {
        return rsx! {};
    }
    let close = move |()| open.set(false);
    let (class, description) = if compact {
        (
            "roost-sheet--bottom roost-sheet--kb model-hub model-hub--stacked",
            None,
        )
    } else {
        (
            "roost-dialog--wide model-hub model-hub--split",
            Some(rsx! { "Pick this chat's model, or set the roles every chat starts from." }),
        )
    };
    let Some(catalog) = catalog else {
        return rsx! {
            Dialog { open: true, on_close: close, headline: "Models".to_owned(), description,
                class: class.to_owned(), test_id: "agent-model-hub".to_owned(),
                div { class: "model-hub__loading", role: "status", "aria-label": "Loading models",
                    for index in 0..4 { Skeleton { key: "{index}", class: "model-hub__skeleton" } }
                }
            }
        };
    };
    let sections = hub_sections(&catalog);
    let current = view().unwrap_or_else(|| initial_view(&catalog));
    let section = sections
        .iter()
        .find(|section| section.view == current)
        .cloned();
    let content = rsx! {
        HubContent {
            view: current.clone(),
            section: section.clone(),
            catalog: catalog.clone(),
            conversation: conversation.clone(),
            settings,
            error,
            pump: pump.clone(),
            compact,
            open,
            connect_provider,
        }
    };
    let nav = rsx! {
        HubNav {
            sections,
            current: current.clone(),
            compact,
            on_select: move |target: HubView| {
                view.set(Some(target));
                if compact {
                    listing.set(false);
                    focus_target.set(Some(DETAIL_HEADING_ID.to_owned()));
                }
            },
        }
    };
    let title = section.map_or_else(String::new, |section| section.label);
    let left = current.clone();
    let body = if !compact {
        rsx! {
            div { class: "model-hub__layout",
                div { class: "model-hub__sidebar", {nav} }
                div { class: "model-hub__content", {content} }
            }
        }
    } else if listing() {
        nav
    } else {
        rsx! {
            div { class: "model-hub__detail",
                div { class: "model-hub__detail-head",
                    IconButton {
                        icon: "arrow_back",
                        label: "All model views",
                        title: "All model views",
                        variant: ButtonVariant::Ghost,
                        size: IconButtonSize::Icon,
                        "data-testid": "agent-model-hub-back",
                        onclick: move |_| {
                            listing.set(true);
                            focus_target.set(Some(nav_row_id(&left)));
                        },
                    }
                    h3 { id: DETAIL_HEADING_ID, class: "model-hub__detail-title", tabindex: "-1", "{title}" }
                }
                div { class: "model-hub__content", {content} }
            }
        }
    };
    rsx! {
        Dialog {
            open: true,
            on_close: close,
            headline: "Models".to_owned(),
            description,
            class: class.to_owned(),
            test_id: "agent-model-hub".to_owned(),
            {body}
            if !error().is_empty() {
                p { class: "model-hub__error", role: "alert", "{error}" }
            }
        }
    }
}

/// One view's content: the role editor and presets, a provider's sign-in, or
/// a model picker that switches this chat.
#[component]
fn HubContent(
    view: HubView,
    section: Option<HubSection>,
    catalog: ModelsCatalog,
    conversation: ConversationSummary,
    settings: Signal<AgentSettingsView>,
    error: Signal<String>,
    pump: Pump,
    compact: bool,
    open: Signal<bool>,
    connect_provider: Signal<Option<ProviderEntry>>,
) -> Element {
    let mut open = open;
    let mut connect_provider = connect_provider;
    let control_size = if compact {
        ButtonSize::Default
    } else {
        ButtonSize::Sm
    };
    if view == HubView::Roles {
        return rsx! {
            AgentRoles { catalog, settings, error }
            HubPresets { settings, error, pump, compact }
        };
    }
    if let (HubView::Provider(id), Some(section)) =
        (&view, section.filter(|section| section.locked))
    {
        let provider = catalog
            .providers
            .iter()
            .find(|provider| provider.id == *id)
            .cloned();
        let count = section.count.unwrap_or_default();
        return rsx! {
            EmptyState {
                icon: "lock",
                title: format!("{} is not signed in", section.label),
                supporting: format!("Sign in to use its {count} models in this chat and in model roles."),
                action: rsx! {
                    Button {
                        variant: ButtonVariant::Default,
                        size: control_size,
                        icon: "add_link",
                        "data-testid": "agent-model-hub-sign-in",
                        onclick: move |_| {
                            connect_provider.set(provider.clone());
                            open.set(false);
                        },
                        "Sign in to {section.label}"
                    }
                },
            }
        };
    }
    let options = hub_options(&catalog, &view, &settings());
    if options.is_empty() {
        let (icon, title, supporting) = if view == HubView::Recent {
            (
                "history",
                "No recent models yet",
                "Models you switch a chat to show up here.",
            )
        } else {
            (
                "link_off",
                "No models to choose from",
                "Sign in to a provider in the list of views to use its models.",
            )
        };
        return rsx! {
            EmptyState {
                icon: icon.to_owned(),
                title: title.to_owned(),
                supporting: supporting.to_owned(),
            }
        };
    }
    let selected = conversation
        .effective_model
        .as_ref()
        .or(conversation.model.as_ref())
        .map(|model| format!("{}/{}", model.provider, model.model_id));
    let conversation_id = conversation.id.clone();
    // Recent lists what chats used, every version of it; elsewhere a model a
    // role leads with stays listed even once a newer version exists.
    let keep = if view == HubView::Recent {
        options
            .iter()
            .map(|option| option.selector.clone())
            .collect()
    } else {
        role_leaders(&settings(), &catalog.thinking_levels)
    };
    rsx! {
        ModelPicker {
            key: "{view.id()}",
            options,
            selected,
            keep,
            placeholder: "Search models",
            test_id: "agent-model-hub-picker",
            autofocus: !compact,
            on_choose: move |selector: String| {
                configure(pump.clone(), conversation_id.clone(), Configure::Model(Some(selector)));
                open.set(false);
            },
        }
    }
}
