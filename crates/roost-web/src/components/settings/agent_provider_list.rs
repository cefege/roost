//! The Models pane's "Not signed in" list: every provider still to connect,
//! searchable, with how it signs in and how many models it would unlock.
//! Rendered by `agent_models`, which opens `AgentConnectDialog` on a choice.

use dioxus::prelude::*;
use libero::components::{Button, Flex, Indicator, Pictogram, Text, TextField};
use roost_protocol::wire::agent_chat::ProviderEntry;

use super::agent_accounts::plural;
use super::agent_provider_status::ProviderSummary;
use libero::sx::sx;

#[component]
pub fn AvailableProviders(
    providers: Vec<ProviderSummary>,
    on_connect: EventHandler<ProviderEntry>,
) -> Element {
    let mut filter = use_signal(String::new);
    let search_sx = use_hook(|| sx().width("100%").max_width("sm"));
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
                value: Some(filter()),
                oninput: move |value| filter.set(value),
                placeholder: "Search providers",
                aria_label: "Search providers",
                sx: search_sx,
                r#type: "search",
            }
        }
        if total == 0 {
            Flex { direction: "row", align: "center", gap: "sm",
                Pictogram { icon: pictogram_icons_lucide::check::outlined }
                Text { size: "sm", "Every provider is signed in." }
            }
        } else if shown.is_empty() {
            Flex { direction: "row", align: "center", gap: "sm",
                Pictogram { icon: pictogram_icons_lucide::search_x::outlined }
                Text { size: "sm", "No provider matches “{filter()}”." }
            }
        } else {
            Flex { direction: "column", gap: "xs",
                for summary in shown {
                    Flex { direction: "row",
                        key: "{summary.provider.id}",
                        align: "center",
                        justify: "space-between",
                        gap: "sm",
                        "data-testid": "agent-provider-available-{summary.provider.id}",
                        Flex { direction: "row", align: "center", gap: "sm",
                            Indicator { color: "muted", with_border: true }
                            Flex { direction: "column",
                                Text { size: "sm", "{summary.provider.name}" }
                                Text { size: "xs", color: "muted", "{provider_support(&summary)}" }
                            }
                        }
                        Button {
                            variant: if summary.provider.supports_oauth { "filled" } else { "outlined" },
                            size: "sm",
                            icon: Some(rsx! { Pictogram { icon: if summary.provider.supports_oauth { pictogram_icons_lucide::log_in::outlined } else { pictogram_icons_lucide::key::outlined } } }),
                            onclick: {
                                let provider = summary.provider.clone();
                                move |_| on_connect.call(provider.clone())
                            },
                            if summary.provider.supports_oauth { "Sign in" } else { "Add API key" }
                        }
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
