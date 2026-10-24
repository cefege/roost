//! One signed-in provider on the Models pane: its state at a glance, every
//! account with its usage and limits, removal, and the models it unlocks.
//! Rendered by `agent_models`; the pane owns loading, this file only draws
//! and sends `AgentAccountRemove`.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{AccountEntry, AccountKind, AccountUsageWindow, ModelEntry};

use super::agent_provider_status::{AccountHealth, ProviderSummary};
use super::format::time_until;
use crate::pump::{Pump, use_store};
use libero::components::{Badge, Button, Chip, Flex, Indicator, Paper, Text, Title};
use libero::sx::sx;
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::agent_chat::RemoveAgentAccount;

/// How many of a provider's models show before "Show all".
const MODEL_PREVIEW: usize = 12;

/// A signed-in provider: header with state and counts, then its accounts.
#[component]
pub fn ConnectedProviderCard(
    summary: ProviderSummary,
    models: Vec<ModelEntry>,
    now_ms: u64,
    on_add: EventHandler<()>,
    on_changed: EventHandler<()>,
) -> Element {
    let health = summary.health(now_ms);
    let account_count = summary.accounts.len();
    let counts = [
        plural(account_count, "account", "accounts"),
        plural(summary.model_count, "model", "models"),
    ]
    .join(" · ");
    let card_sx = use_hook(|| sx().padding("md"));
    let state = provider_state(&health, account_count, now_ms);
    rsx! {
        Paper { sx: card_sx, "data-testid": "agent-provider-{summary.provider.id}",
            Flex { direction: "column", gap: "md",
                Flex { direction: "row", align: "center", justify: "space-between", gap: "sm",
                    Flex { direction: "column",
                        Title { size: "md", "{summary.provider.name}" }
                        Text { size: "sm", color: "muted", "{counts} · {state}" }
                    }
                    Flex { direction: "row", align: "center", gap: "sm",
                        Indicator { color: if matches!(&health, AccountHealth::Ready) { "success" } else { "warning" }, title: health_title(&health, now_ms) }
                        Button { variant: "outlined", size: "sm",
                            icon: Some(rsx! { libero::components::Pictogram { icon: pictogram_icons_lucide::plus::outlined } }),
                            onclick: move |_| on_add.call(()), "Add account"
                        }
                    }
                }
                if !summary.accounts.is_empty() {
                    Flex { direction: "column", gap: "sm",
                        for account in summary.accounts.clone() {
                            AccountRow { key: "{account.credential_id}", account, now_ms, on_removed: on_changed }
                        }
                    }
                }
                if !models.is_empty() { ModelPreview { models } }
            }
        }
    }
}

#[component]
fn AccountRow(account: AccountEntry, now_ms: u64, on_removed: EventHandler<()>) -> Element {
    let pump = use_store();
    let mut confirming = use_signal(|| false);
    let error = use_signal(String::new);
    let health = AccountHealth::of(&account, now_ms);
    let credential_id = account.credential_id;
    let windows = account
        .usage
        .as_ref()
        .map(|usage| usage.windows.clone())
        .unwrap_or_default();
    let note = account.usage.as_ref().and_then(|usage| usage.note.clone());
    let row_sx = use_hook(|| sx().padding("sm"));
    rsx! {
        Paper { sx: row_sx,
            Flex { direction: "row", align: "start", justify: "space-between", gap: "sm",
                Flex { direction: "column", gap: "sm",
                    Flex { direction: "row", align: "center", gap: "sm",
                        Indicator { color: if matches!(&health, AccountHealth::Ready) { "success" } else { "warning" }, title: health_title(&health, now_ms) }
                        Text { size: "sm", "{account.label}" }
                    }
                    Text { size: "xs", color: "muted", "{kind_label(account.kind)} · {health_title(&health, now_ms)}" }
                    if !windows.is_empty() {
                        Flex { direction: "row", gap: "sm", wrap: true,
                            for window in windows {
                                UsageWindow { key: "{window.name}", window, now_ms }
                            }
                        }
                    }
                    if let Some(note) = note { Text { size: "sm", color: "muted", "{note}" } }
                    if !error().is_empty() { Text { size: "sm", color: "error", role: "alert", "{error()}" } }
                }
                Flex { direction: "row", gap: "xs",
                    if confirming() {
                        Button { variant: "standard", size: "sm", onclick: move |_| confirming.set(false), "Cancel" }
                        Button { variant: "filled", color: "error", size: "sm",
                            onclick: move |_| remove_account(pump.clone(), credential_id, error, on_removed),
                            "Remove"
                        }
                    } else {
                        Button { variant: "standard", color: "muted", size: "sm",
                            icon: Some(rsx! { libero::components::Pictogram { icon: pictogram_icons_lucide::trash_2::outlined } }),
                            onclick: move |_| confirming.set(true), "Remove"
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn UsageWindow(window: AccountUsageWindow, now_ms: u64) -> Element {
    let percent = (window.used_fraction * 100.0).clamp(0.0, 100.0);
    let reset = window
        .resets_at_ms
        .and_then(|at| u64::try_from(at).ok())
        .map(|at| format!(" · resets {}", time_until(now_ms, at)))
        .unwrap_or_default();
    let window_sx = use_hook(|| sx().padding("sm"));
    rsx! {
        Paper { sx: window_sx,
            Flex { direction: "column", gap: "xs",
                Text { size: "xs", color: "muted", "{window.name}" }
                Badge { variant: "outlined", "{percent:.0}%" }
                Text { size: "xs", color: "muted", "used{reset}" }
                libero::components::ProgressBar {
                    aria_label: "{window.name} used",
                    value: Some(percent),
                    max: 100.0,
                }
            }
        }
    }
}

/// The models a provider unlocks, newest first as the catalog orders them;
/// a long list (OpenRouter offers hundreds) folds behind "Show all".
#[component]
fn ModelPreview(models: Vec<ModelEntry>) -> Element {
    let mut expanded = use_signal(|| false);
    let total = models.len();
    let shown = if expanded() {
        total
    } else {
        total.min(MODEL_PREVIEW)
    };
    rsx! {
        Flex { direction: "column", gap: "sm",
            Title { size: "sm", "Models" }
            Flex { direction: "row", gap: "xs", wrap: true,
                for model in models.into_iter().take(shown) {
                    Chip {
                        key: "{model.model_id}",
                        icon: model.reasoning.then(|| rsx! { libero::components::Pictogram { icon: pictogram_icons_lucide::brain::outlined } }),
                        "{model.name}"
                    }
                }
            }
            if total > MODEL_PREVIEW {
                Button { variant: "standard", size: "sm",
                    onclick: move |_| expanded.set(!expanded()),
                    if expanded() { "Show fewer" } else { "Show all {total}" }
                }
            }
        }
    }
}

pub fn kind_label(kind: AccountKind) -> &'static str {
    match kind {
        AccountKind::Oauth => "Subscription sign-in",
        AccountKind::ApiKey => "API key",
    }
}

pub fn plural(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

fn health_title(health: &AccountHealth, now_ms: u64) -> String {
    match health {
        AccountHealth::Ready => "Ready".to_owned(),
        AccountHealth::NearLimit { used_fraction } => {
            format!("Near its limit ({:.0}% used)", used_fraction * 100.0)
        }
        AccountHealth::RateLimited { until_ms } => {
            format!("Rate-limited, back {}", time_until(now_ms, *until_ms))
        }
        AccountHealth::Disabled { cause } => format!("Disabled: {cause}"),
    }
}

/// The provider line's last part: whether a turn can run on it right now.
fn provider_state(health: &AccountHealth, account_count: usize, now_ms: u64) -> String {
    match health {
        AccountHealth::Ready => "Ready".to_owned(),
        _ if account_count > 1 => format!("No account ready: {}", health_title(health, now_ms)),
        _ => health_title(health, now_ms),
    }
}

fn remove_account(
    pump: Pump,
    credential_id: i64,
    error: Signal<String>,
    on_removed: EventHandler<()>,
) {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        let mut error = error;
        match pump.rpc().call(&RemoveAgentAccount { credential_id }).await {
            Ok(()) => {
                tracing::info!(target: "agent_settings", credential_id, "agent account removed");
                on_removed.call(());
            }
            Err(failure) => error.set(format!("Could not remove this account: {failure}")),
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, credential_id, error, on_removed);
}
