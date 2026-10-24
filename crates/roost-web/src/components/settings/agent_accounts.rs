//! One signed-in provider on the Models pane: its state at a glance, every
//! account with its usage and limits, removal, and the models it unlocks.
//! Rendered by `agent_models`; the pane owns loading, this file only draws
//! and sends `AgentAccountRemove`.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{AccountEntry, AccountKind, AccountUsageWindow, ModelEntry};

use super::agent_provider_status::{AccountHealth, ProviderSummary};
use super::format::time_until;
use crate::components::ui::{
    Button, ButtonSize, ButtonVariant, Card, CardAction, CardContent, CardDescription, CardHeader,
    CardTitle, Chip, FieldError, IconType, List, ListRow, MetricTile, SectionTitle, StatusDot,
};
use crate::pump::{Pump, use_store};
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
    let state = provider_state(&health, account_count, now_ms);
    rsx! {
        Card {
            "data-testid": format!("agent-provider-{}", summary.provider.id),
            CardHeader {
                CardTitle { "{summary.provider.name}" }
                CardDescription { "{counts} · {state}" }
                CardAction {
                    div { class: "flex items-center gap-2",
                        StatusDot { status: health.dot().to_owned(), size: 10, title: Some(health_title(&health, now_ms)) }
                        Button { variant: ButtonVariant::Outline, size: ButtonSize::Sm, icon: Some(IconType::Plus), onclick: move |_| on_add.call(()), "Add account" }
                    }
                }
            }
            CardContent {
                if !summary.accounts.is_empty() {
                    List { contained: true,
                        for account in summary.accounts.clone() {
                            AccountRow { key: "{account.credential_id}", account, now_ms, on_removed: on_changed }
                        }
                    }
                }
                if !models.is_empty() {
                    ModelPreview { models }
                }
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
    rsx! {
        ListRow {
            leading: rsx! { StatusDot { status: health.dot().to_owned(), title: Some(health_title(&health, now_ms)) } },
            headline: rsx! { "{account.label}" },
            support: rsx! {
                div { class: "flex flex-col gap-2",
                    div { "{kind_label(account.kind)} · {health_title(&health, now_ms)}" }
                    if !windows.is_empty() {
                        div { class: "grid grid-cols-1 gap-2 sm:grid-cols-2",
                            for window in windows {
                                UsageWindow { key: "{window.name}", window, now_ms }
                            }
                        }
                    }
                    if let Some(note) = note { div { class: "text-sm text-muted-foreground", "{note}" } }
                    if !error().is_empty() {
                        FieldError { {error()} }
                    }
                }
            },
            trailing: rsx! {
                if confirming() {
                    Button { variant: ButtonVariant::Ghost, size: ButtonSize::Sm, onclick: move |_| confirming.set(false), "Cancel" }
                    Button { variant: ButtonVariant::Destructive, size: ButtonSize::Sm,
                        onclick: move |_| remove_account(pump.clone(), credential_id, error, on_removed),
                        "Remove"
                    }
                } else {
                    Button { variant: ButtonVariant::Ghost, size: ButtonSize::Sm, icon: Some(IconType::Trash2), onclick: move |_| confirming.set(true), "Remove" }
                }
            },
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
    rsx! {
        MetricTile {
            label: window.name.clone(),
            icon: None,
            value: format!("{percent:.0}%"),
            support: Some(format!("used{reset}")),
            ratio: Some(window.used_fraction),
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
        div { class: "flex flex-col gap-2",
            SectionTitle { "Models" }
            div { class: "flex flex-wrap items-center gap-2",
                for model in models.into_iter().take(shown) {
                    Chip {
                        key: "{model.model_id}",
                        label: model.name.clone(),
                        icon: model.reasoning.then_some(IconType::Brain),
                        selected: None,
                    }
                }
            }
            if total > MODEL_PREVIEW {
                Button { variant: ButtonVariant::Link, size: ButtonSize::Sm,
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
