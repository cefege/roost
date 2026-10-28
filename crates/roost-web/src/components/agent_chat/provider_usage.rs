//! Provider quota and recorded chat usage beside the model selector.
//! Limits come from `AgentUsageGet`; chat totals come from the inspection projection.
//! Mounted by `AgentModelControls`, keyed to the conversation's pinned or effective model.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{
    AccountEntry, AccountKind, AccountUsageWindow, ConversationSummary, ModelsCatalog, UsageTotals,
};

#[cfg(target_arch = "wasm32")]
use roost_protocol::wire::agent_chat::AgentInspection;

use crate::components::settings::agent_provider_status::AccountHealth;
use crate::components::settings::format::time_until;
use crate::components::ui::{
    Badge, BadgeVariant, Card, CardContent, CardSize, Popover, PopoverContent, PopoverTitle,
    PopoverTrigger, UsageRing,
};
use crate::pump::Pump;
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::agent_chat::{GetAgentUsage, InspectAgentChat};

/// Shows the current provider's quota and this conversation's recorded usage.
#[component]
pub(super) fn ProviderUsage(
    conversation: ConversationSummary,
    catalog: Option<ModelsCatalog>,
    pump: Pump,
) -> Element {
    let model = conversation
        .effective_model
        .as_ref()
        .or(conversation.model.as_ref());
    let provider = model.map(|model| model.provider.clone());
    let provider_name = provider
        .as_deref()
        .and_then(|provider| {
            catalog
                .as_ref()?
                .providers
                .iter()
                .find(|item| item.id == provider)
        })
        .map(|provider| provider.name.clone())
        .or_else(|| provider.clone());
    let mut accounts = use_signal(|| None::<Vec<AccountEntry>>);
    let mut quota_error = use_signal(String::new);
    let mut chat_usage = use_signal(|| None::<UsageTotals>);
    let mut chat_error = use_signal(String::new);
    let mut request_generation = use_signal(|| 0_u64);
    let mut open = use_signal(|| false);

    #[cfg(target_arch = "wasm32")]
    {
        let load_pump = pump.clone();
        use_effect(use_reactive(
            (
                &conversation.id,
                &conversation.updated_ms,
                &provider,
                &open(),
            ),
            move |(conversation_id, _, provider, _)| {
                let pump = load_pump.clone();
                // A previous lookup must not overwrite a newer model's selection.
                let generation = request_generation.peek().wrapping_add(1);
                request_generation.set(generation);
                let request_generation = request_generation;
                accounts.set(None);
                quota_error.set(String::new());
                chat_usage.set(None);
                chat_error.set(String::new());
                spawn(async move {
                    let inspection = pump
                        .rpc()
                        .call(&InspectAgentChat {
                            conversation_id: conversation_id.clone(),
                        })
                        .await;
                    if request_generation() != generation {
                        return;
                    }
                    match inspection {
                        Ok(AgentInspection { usage, .. }) => chat_usage.set(Some(usage)),
                        Err(failure) => {
                            chat_error.set(format!("Chat usage unavailable: {failure}"));
                        }
                    }
                    let Some(provider) = provider else {
                        return;
                    };
                    let usage = pump
                        .rpc()
                        .call(&GetAgentUsage {
                            conversation_id,
                            provider,
                        })
                        .await;
                    if request_generation() != generation {
                        return;
                    }
                    match usage {
                        Ok(json) => match serde_json::from_str::<Vec<AccountEntry>>(&json) {
                            Ok(rows) => accounts.set(Some(rows)),
                            Err(failure) => {
                                quota_error.set(format!("Provider limits unavailable: {failure}"));
                            }
                        },
                        Err(failure) => {
                            quota_error.set(format!("Provider limits unavailable: {failure}"));
                        }
                    }
                });
            },
        ));
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (
        &pump,
        &conversation,
        &provider,
        &mut accounts,
        &mut quota_error,
        &mut chat_usage,
        &mut chat_error,
        &mut request_generation,
    );

    let account_view = accounts.read();
    let selected = account_view.as_ref().and_then(|rows| {
        rows.iter().find(|account| {
            account.provider == provider.as_deref().unwrap_or_default() && account.selected
        })
    });
    let window = selected.and_then(ring_window);
    let fraction = window.map(|window| window.used_fraction.clamp(0.0, 1.0));
    let ring_title = match (&selected, window, provider_name.as_deref()) {
        (Some(account), Some(window), Some(provider)) => format!(
            "{provider} · {} · {}: {:.0}% used",
            account.label,
            window_label(&window.name),
            window.used_fraction.clamp(0.0, 1.0) * 100.0
        ),
        (Some(account), None, Some(provider)) => {
            format!("{provider} · {} · limits unavailable", account.label)
        }
        (_, _, Some(provider)) => format!("{provider} usage limits"),
        _ => "Provider usage limits".to_owned(),
    };
    let ring_title = if selected
        .as_ref()
        .and_then(|account| account.usage.as_ref())
        .is_some_and(|usage| usage.note.is_some() && !usage.windows.is_empty())
    {
        format!("{ring_title} · last known")
    } else {
        ring_title
    };
    let chat_usage_value = chat_usage();
    let chat_error_value = chat_error();
    let quota_error_value = quota_error();
    let now_ms = now_ms();
    rsx! {
        Popover {
            open: open(),
            on_open_change: move |next| open.set(next),
            test_id: "agent-provider-usage",
            PopoverTrigger {
                ghost: true,
                class: "size-8 shrink-0 p-0",
                aria_label: Some("Provider usage and this chat's usage".to_owned()),
                title: ring_title.clone(),
                UsageRing {
                    fraction,
                    label: Some("Provider quota used".to_owned()),
                    title: Some(ring_title.clone()),
                    size: 18,
                }
            }
            PopoverContent {
                class: "w-80 max-h-96 overflow-y-auto",
                PopoverTitle { "Usage" }
                p { class: "mb-3 text-xs text-muted-foreground",
                    "Provider limits are account-wide, not per-chat. This chat's recorded tokens and estimated cost are separate."
                }
                section { class: "mb-4 flex flex-col gap-2",
                    h3 { class: "text-sm font-medium", "{provider_name.clone().unwrap_or_else(|| \"Provider\".to_owned())} limits" }
                    if !quota_error_value.is_empty() {
                        p { class: "text-xs text-muted-foreground", "{quota_error_value}" }
                    } else if provider.is_none() {
                        p { class: "text-xs text-muted-foreground", "Choose a model to see its provider limits." }
                    } else if let Some(rows) = accounts() {
                        ProviderAccounts { accounts: rows, provider: provider.clone().unwrap_or_default(), now_ms }
                    } else {
                        p { class: "text-xs text-muted-foreground", "Loading provider limits…" }
                    }
                }
                section { class: "border-t border-border pt-3 flex flex-col gap-2",
                    h3 { class: "text-sm font-medium", "This chat" }
                    if let Some(usage) = chat_usage_value {
                        ChatUsage { usage }
                    } else if !chat_error_value.is_empty() {
                        p { class: "text-xs text-muted-foreground", "{chat_error_value}" }
                    } else {
                        p { class: "text-xs text-muted-foreground", "Loading chat usage…" }
                    }
                }
            }
        }
    }
}

#[component]
fn ProviderAccounts(accounts: Vec<AccountEntry>, provider: String, now_ms: u64) -> Element {
    let rows: Vec<_> = accounts
        .into_iter()
        .filter(|account| account.provider == provider)
        .collect();
    if rows.is_empty() {
        return rsx! { p { class: "text-xs text-muted-foreground", "No account connected for this provider." } };
    }
    rsx! {
        for account in rows {
            AccountQuota { account, now_ms }
        }
    }
}

#[component]
fn AccountQuota(account: AccountEntry, now_ms: u64) -> Element {
    let selected = account.selected;
    let health = AccountHealth::of(&account, now_ms);
    let unavailable = if account.kind == AccountKind::ApiKey {
        "Limits are not reported for API-key accounts."
    } else {
        "No provider limits reported."
    };
    rsx! {
        Card { size: CardSize::Sm,
            CardContent { class: "flex flex-col gap-2",
                div { class: "flex items-center justify-between gap-2",
                    span { class: "truncate text-sm font-medium", "{account.label}" }
                    if selected {
                        Badge { variant: BadgeVariant::Secondary, "Selected" }
                    }
                }
                if let Some(warning) = health_description(&health) {
                    p { class: "text-xs text-muted-foreground", "{warning}" }
                }
                if let Some(usage) = account.usage {
                    if usage.windows.is_empty() {
                        if let Some(note) = usage.note {
                            p { class: "text-xs text-muted-foreground", "Limits unavailable: {note}" }
                        } else {
                            p { class: "text-xs text-muted-foreground", "{unavailable}" }
                        }
                    } else {
                        if let Some(note) = usage.note {
                            p { class: "text-xs text-muted-foreground", "Last known limits. Provider update: {note}" }
                        }
                        for window in usage.windows {
                            QuotaWindow { window }
                        }
                    }
                } else {
                    p { class: "text-xs text-muted-foreground", "Usage limits have not been reported." }
                }
            }
        }
    }
}

#[component]
fn QuotaWindow(window: AccountUsageWindow) -> Element {
    let value = if window.used_fraction.is_finite() {
        format!("{:.0}% used", window.used_fraction.clamp(0.0, 1.0) * 100.0)
    } else {
        "Usage unknown".to_owned()
    };
    let reset = window
        .resets_at_ms
        .and_then(|at| u64::try_from(at).ok())
        .map(|reset_at| format!(" · resets {}", time_until(now_ms(), reset_at)))
        .unwrap_or_default();
    rsx! {
        div { class: "flex items-center justify-between gap-2 text-xs",
            span { "{window_label(&window.name)}" }
            span { class: "text-muted-foreground", "{value}{reset}" }
        }
    }
}

#[component]
fn ChatUsage(usage: UsageTotals) -> Element {
    rsx! {
        p { class: "text-xs text-muted-foreground", "{usage.input_tokens} input · {usage.output_tokens} output tokens" }
        p { class: "text-xs text-muted-foreground", "Estimated model cost: ${usage.cost_usd:.6}" }
    }
}

fn ring_window(account: &AccountEntry) -> Option<&AccountUsageWindow> {
    let windows = &account.usage.as_ref()?.windows;
    windows
        .iter()
        .find(|window| window.name == "7d" && window.used_fraction.is_finite())
        .or_else(|| {
            windows
                .iter()
                .filter(|window| window.used_fraction.is_finite())
                .max_by(|left, right| left.used_fraction.total_cmp(&right.used_fraction))
        })
}

fn window_label(name: &str) -> &str {
    match name {
        "5h" => "Short window (5h)",
        "7d" => "Weekly (7d)",
        "primary" => "Primary window",
        "secondary" => "Secondary window",
        other => other,
    }
}

fn health_description(health: &AccountHealth) -> Option<String> {
    match health {
        AccountHealth::Disabled { cause } => Some(format!("Disabled: {cause}")),
        AccountHealth::RateLimited { until_ms } => Some(format!(
            "Rate limited until {}",
            time_until(now_ms(), *until_ms)
        )),
        AccountHealth::NearLimit { used_fraction } => Some(format!(
            "Near its limit ({:.0}% used)",
            used_fraction * 100.0
        )),
        AccountHealth::Ready => None,
    }
}

#[cfg(target_arch = "wasm32")]
fn now_ms() -> u64 {
    crate::platform::clock::WallClock.now_ms()
}

#[cfg(not(target_arch = "wasm32"))]
fn now_ms() -> u64 {
    0
}
