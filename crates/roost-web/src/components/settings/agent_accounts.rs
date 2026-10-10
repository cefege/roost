//! Provider account rows for agent credentials, usage and removal.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{AccountEntry, AccountKind, AccountUsage};

use crate::components::md::{Button, ButtonVariant, Chip, List, ListRow, ProgressBar};
use crate::pump::{Pump, use_store};
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::agent_chat::{
    GetAgentUsage, ListAgentAccounts, RemoveAgentAccount,
};

/// Loads and renders the connected accounts for a single provider.
#[component]
pub fn AgentAccounts(provider: String, refresh: u64, on_add: EventHandler<()>) -> Element {
    let pump = use_store();
    let accounts = use_signal(Vec::<AccountEntry>::new);
    let revision = use_signal(|| refresh);
    let error = use_signal(String::new);
    let load_pump = pump.clone();
    use_effect(move || {
        // Re-run on a parent refresh (a sign-in finished) and after a removal.
        let _parent = refresh;
        let _local = revision();
        load_accounts(load_pump.clone(), accounts, error);
    });
    let rows: Vec<_> = accounts()
        .into_iter()
        .filter(|account| account.provider == provider)
        .collect();
    rsx! {
        div { class: "agent-settings__accounts",
            if !rows.is_empty() {
                List { contained: true,
                    for account in rows {
                        {account_row(account, pump.clone(), error, revision)}
                    }
                }
            }
            if !error().is_empty() { p { class: "md-body-s", role: "alert", {error()} } }
            Button { variant: ButtonVariant::Outline, icon: "add", onclick: move |_| on_add.call(()), "Add account" }
        }
    }
}

fn account_row(
    account: AccountEntry,
    pump: Pump,
    error: Signal<String>,
    revision: Signal<u64>,
) -> Element {
    let credential_id = account.credential_id;
    rsx! {
        ListRow {
            key: "{credential_id}",
            headline: rsx! { "{account.label}" },
            support: rsx! {
                if let Some(usage) = account.usage.clone() { {usage_view(usage)} }
                if let Some(cause) = account.disabled_cause.clone() { p { class: "md-body-s", role: "status", "Disabled: {cause}" } }
                if let Some(until) = account.blocked_until_ms { p { class: "md-body-s", role: "status", "Blocked until {format_timestamp(until)}" } }
            },
            trailing: rsx! {
                Chip { label: kind_label(account.kind).to_owned(), icon: None, selected: None }
                Button { variant: ButtonVariant::Outline,
                    onclick: move |_| remove_account(pump.clone(), credential_id, error, revision),
                    "Remove"
                }
            },
        }
    }
}

fn usage_view(usage: AccountUsage) -> Element {
    rsx! { div { class: "agent-settings__usage",
        for window in usage.windows {
            div { class: "agent-settings__usage-window", key: "{window.name}",
                ProgressBar { value: Some(window.used_fraction), label: format!("{} · {:.0}%", window.name, window.used_fraction * 100.0) }
                if let Some(reset) = window.resets_at_ms { span { class: "md-body-s", "Resets {format_timestamp(reset)}" } }
            }
        }
        if let Some(note) = usage.note { p { class: "md-body-s", "{note}" } }
    } }
}

fn kind_label(kind: AccountKind) -> &'static str {
    match kind {
        AccountKind::Oauth => "OAuth",
        AccountKind::ApiKey => "API key",
    }
}

/// UTC `YYYY-MM-DD HH:MM` for a millisecond timestamp.
fn format_timestamp(timestamp_ms: i64) -> String {
    let seconds = timestamp_ms.div_euclid(1000);
    let days = seconds.div_euclid(86_400);
    let minute_of_day = seconds.rem_euclid(86_400) / 60;
    // Civil-from-days (Howard Hinnant), proleptic Gregorian calendar.
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        minute_of_day / 60,
        minute_of_day % 60
    )
}
fn load_accounts(pump: Pump, accounts: Signal<Vec<AccountEntry>>, error: Signal<String>) {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        let mut accounts = accounts;
        let result = pump.rpc().call(&ListAgentAccounts {}).await;
        let usage = pump.rpc().call(&GetAgentUsage {}).await;
        let mut error = error;
        match (result, usage) {
            (Ok(json), usage_result) => match serde_json::from_str::<Vec<AccountEntry>>(&json) {
                Ok(mut rows) => {
                    if let Ok(usage_json) = usage_result {
                        if let Ok(usage_rows) =
                            serde_json::from_str::<Vec<AccountEntry>>(&usage_json)
                        {
                            for row in &mut rows {
                                if let Some(fresh) = usage_rows
                                    .iter()
                                    .find(|item| item.credential_id == row.credential_id)
                                {
                                    row.usage = fresh.usage.clone();
                                    row.blocked_until_ms = fresh.blocked_until_ms;
                                }
                            }
                        }
                    }
                    accounts.set(rows);
                    error.set(String::new());
                }
                Err(failure) => error.set(format!("Could not decode agent accounts: {failure}")),
            },
            (Err(failure), _) => error.set(format!("Could not load agent accounts: {failure}")),
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, accounts, error);
}

fn remove_account(pump: Pump, credential_id: i64, error: Signal<String>, revision: Signal<u64>) {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        let mut error = error;
        let mut revision = revision;
        match pump.rpc().call(&RemoveAgentAccount { credential_id }).await {
            Ok(()) => {
                error.set(String::new());
                revision.set(revision().wrapping_add(1));
            }
            Err(failure) => error.set(format!("Could not remove account: {failure}")),
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, credential_id, error, revision);
}
