//! The Models pane's "Not signed in" list: every provider still to connect,
//! searchable, with how it signs in and how many models it would unlock.
//! Rendered by `agent_models`, which opens `AgentConnectDialog` on a choice.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::ProviderEntry;

use super::agent_accounts::plural;
use super::agent_provider_status::ProviderSummary;
use crate::components::ui::{
    Button, ButtonSize, ButtonVariant, EmptyState, IconType, List, ListRow, StatusDot, TextField,
};

#[component]
pub fn AvailableProviders(
    providers: Vec<ProviderSummary>,
    on_connect: EventHandler<ProviderEntry>,
) -> Element {
    let mut filter = use_signal(String::new);
    let needle = filter().trim().to_lowercase();
    let total = providers.len();
    let shown: Vec<ProviderSummary> = providers
        .into_iter()
        .filter(|summary| {
            needle.is_empty()
                || summary.provider.name.to_lowercase().contains(&needle)
                || summary.provider.id.to_lowercase().contains(&needle)
        })
        .collect();
    rsx! {
        if total > 6 {
            TextField {
                value: filter(),
                on_input: move |value| filter.set(value),
                placeholder: Some("Search providers".to_owned()),
                aria_label: Some("Search providers".to_owned()),
                class: Some("w-full max-w-sm".to_owned()),
                input_type: Some("search".to_owned()),
            }
        }
        if total == 0 {
            EmptyState { icon: IconType::Check, title: "Every provider is signed in.".to_owned() }
        } else if shown.is_empty() {
            EmptyState { icon: IconType::SearchX, title: format!("No provider matches “{}”.", filter()) }
        } else {
            List { contained: true,
                for summary in shown {
                    ListRow {
                        key: "{summary.provider.id}",
                        test_id: Some(format!("agent-provider-available-{}", summary.provider.id)),
                        leading: rsx! { StatusDot { status: "idle".to_owned(), hollow: true, title: Some("Not signed in".to_owned()) } },
                        headline: rsx! { "{summary.provider.name}" },
                        support: rsx! { "{provider_support(&summary)}" },
                        trailing: rsx! {
                            Button {
                                variant: if summary.provider.supports_oauth { ButtonVariant::Default } else { ButtonVariant::Outline },
                                size: ButtonSize::Sm,
                                icon: Some(if summary.provider.supports_oauth { IconType::LogIn } else { IconType::Key }),
                                onclick: {
                                    let provider = summary.provider.clone();
                                    move |_| on_connect.call(provider.clone())
                                },
                                if summary.provider.supports_oauth { "Sign in" } else { "Add API key" }
                            }
                        },
                    }
                }
            }
        }
    }
}

fn provider_support(summary: &ProviderSummary) -> String {
    let method = if summary.provider.supports_oauth {
        "Sign in with your subscription or an API key"
    } else {
        "API key"
    };
    if summary.model_count == 0 {
        return method.to_owned();
    }
    format!(
        "{} · {method}",
        plural(summary.model_count, "model", "models")
    )
}
