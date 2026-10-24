//! The hub's view for a provider not signed in: its sign-in action, which the
//! Enter key also starts. Mounted by `model_hub` in place of the model rows.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{ModelsCatalog, ProviderEntry};

use super::hub_logic::HubView;
use crate::components::ui::{Button, ButtonSize, ButtonVariant, EmptyState, IconType};

/// The locked provider's view: its sign-in, which the Enter key also starts.
#[component]
pub(super) fn LockedProvider(
    catalog: ModelsCatalog,
    view: HubView,
    connect_provider: Signal<Option<ProviderEntry>>,
    open: Signal<bool>,
) -> Element {
    let HubView::Provider(provider_id) = view else {
        return rsx! {};
    };
    let Some(entry) = catalog
        .providers
        .iter()
        .find(|provider| provider.id == provider_id)
        .cloned()
    else {
        return rsx! {};
    };
    let label = entry.name.clone();
    let mut open = open;
    rsx! {
        EmptyState {
            icon: IconType::Lock,
            title: format!("{label} is not signed in"),
            supporting: Some("Sign in to use its models in this chat and in model roles.".to_owned()),
            action: Some(rsx! {
                Button {
                    variant: ButtonVariant::Default,
                    size: ButtonSize::Default,
                    "data-testid": "agent-model-hub-sign-in",
                    onclick: move |_| {
                        connect_provider.set(Some(entry.clone()));
                        open.set(false);
                    },
                    "Sign in to {label}"
                }
            }),
        }
    }
}
