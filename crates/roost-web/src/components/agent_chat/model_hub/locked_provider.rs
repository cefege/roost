//! The hub's view for a provider not signed in: its sign-in action, which the
//! Enter key also starts. Mounted by `model_hub` in place of the model rows.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{ModelsCatalog, ProviderEntry};

use super::hub_logic::HubView;
use libero::components::{Button, Flex, Pictogram, Text, Title};
use libero::sx::sx;

/// The locked provider's view: its sign-in, which the Enter key also starts.
#[component]
pub(super) fn LockedProvider(
    catalog: ModelsCatalog,
    view: HubView,
    connect_provider: Signal<Option<ProviderEntry>>,
    open: Signal<bool>,
) -> Element {
    let container_sx = use_hook(|| {
        sx().flex_direction("column")
            .align_items("center")
            .justify_content("center")
            .gap("md")
            .min_height("12rem")
            .padding("lg")
            .text_align("center")
    });
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
    let title = format!("{label} is not signed in");
    rsx! {
        Flex { direction: "column", sx: container_sx,
            Pictogram { icon: pictogram_icons_lucide::lock::outlined, width: "1.5rem", height: "1.5rem" }
            Title { component: "h3", "{title}" }
            Text { size: "sm", color: "text-dimmed", "Sign in to use its models in this chat and in model roles." }
            Button {
                variant: "filled",
                "data-testid": "agent-model-hub-sign-in",
                onclick: move |_| {
                    connect_provider.set(Some(entry.clone()));
                    open.set(false);
                },
                "Sign in to {label}"
            }
        }
    }
}
