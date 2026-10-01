//! Settings → System → Audit: the coordinator's audit log, newest first.
//!
//! Ports `apps/web/src/components/Settings/AuditLogPane.tsx`. Depends on
//! `roost-client-core`'s `AuditList` call; the store's own `audit_rows` is the
//! live Sync feed, and this pane adds the paging and filtering a reader wants
//! over a longer window than the feed retains.
//!
//! A refusal is a row-shaped error, not an empty table: "you may not read the
//! audit log" and "nothing has happened" are different facts.

use dioxus::prelude::*;
use roost_client_core::client::rpc::calls::settings::audit::AuditLogRow;
// The round trips are browser-only: a native build has no coordinator to ask,
// so the call types are gated with them.
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::settings::audit::{AuditLogPage, ListAuditRows};

use super::format::{format_timestamp, short_identifier};
use crate::components::md::{Button, ButtonVariant, Card, EmptyState, TextField};
use crate::pump::{Pump, use_store};

/// How many rows a page asks for.
#[cfg(target_arch = "wasm32")]
const PAGE_LIMIT: u32 = 100;

/// The rows read so far, the cursor for the next page, and the filter bar.
#[derive(Debug, Clone, PartialEq, Default)]
struct AuditView {
    rows: Vec<AuditLogRow>,
    next_cursor: Option<String>,
    loaded: bool,
    error: Option<String>,
    fingerprint: String,
    method: String,
    path: String,
    loading_more: bool,
}

/// The pane.
#[component]
pub fn AuditLogPane() -> Element {
    let pump = use_store();
    let mut view = use_signal(AuditView::default);
    let load_pump = pump.clone();
    use_effect(move || load_page(load_pump.clone(), None, view));

    let state = view();
    let rows: Vec<AuditLogRow> = state
        .rows
        .iter()
        .filter(|row| row_matches(row, &state))
        .cloned()
        .collect();
    rsx! {
        div {
            class: "settings-pane",
            style: "display: flex; flex-direction: column; gap: var(--md-space-5);",
            "data-testid": "audit-pane",
            Card { title: "Filter",
                div { style: "display: flex; flex-direction: column; gap: var(--md-space-3);",
                    TextField {
                        label: "Caller fingerprint".to_owned(),
                        placeholder: "authorized key fingerprint".to_owned(),
                        value: state.fingerprint.clone(),
                        on_input: move |value| view.write().fingerprint = value,
                    }
                    TextField {
                        label: "Method".to_owned(),
                        placeholder: "SessionsSpawn".to_owned(),
                        value: state.method.clone(),
                        on_input: move |value| view.write().method = value,
                    }
                    TextField {
                        label: "Path contains".to_owned(),
                        placeholder: "/roost.v1".to_owned(),
                        value: state.path.clone(),
                        on_input: move |value| view.write().path = value,
                    }
                }
            }
            Card {
                title: "Audit log",
                supporting: "Newest first. Rows arrive from the coordinator's own table; the live feed keeps this tab's rows fresh.",
                if let Some(message) = state.error.clone() {
                    p { role: "alert", class: "md-body-m", style: "color: var(--md-sys-color-error); margin: 0 0 var(--md-space-3);", {message} }
                }
                if !state.loaded {
                    span { class: "md-body-m", style: "color: var(--md-sys-color-on-surface-variant);", "Loading…" }
                } else if rows.is_empty() {
                    EmptyState {
                        icon: "history",
                        title: "No audit rows",
                        supporting: "Nothing has matched yet. Widen the filter, or drive some coordinator traffic.",
                    }
                } else {
                    div { style: "display: flex; flex-direction: column;",
                        for row in rows {
                            AuditTableRow { row }
                        }
                    }
                }
                if state.next_cursor.is_some() {
                    Button {
                        variant: ButtonVariant::Ghost,
                        "data-testid": "audit-load-more",
                        disabled: state.loading_more,
                        onclick: {
                            let cursor = state.next_cursor.clone();
                            move |_| load_more(pump.clone(), cursor.clone(), view)
                        },
                        if state.loading_more { "Loading…" } else { "Load more" }
                    }
                }
            }
        }
    }
}

/// One row, in the table anatomy the pane's own card supplies.
#[component]
fn AuditTableRow(row: AuditLogRow) -> Element {
    let tone = if row.status < 300 {
        "color: var(--md-sys-color-on-secondary-container); background: var(--md-sys-color-secondary-container);"
    } else if row.status < 400 {
        "color: var(--md-sys-color-on-surface); background: var(--md-sys-color-surface-container-high);"
    } else if row.status < 500 {
        "color: var(--md-sys-color-on-tertiary-container); background: var(--md-sys-color-tertiary-container);"
    } else {
        "color: var(--md-on-primary); background: var(--md-sys-color-error);"
    };
    let status_style = format!("padding: 2px 8px; border-radius: var(--md-shape-full); {tone}");
    rsx! {
        div { class: "md-list-row", "data-testid": "audit-row", "data-status": row.status.to_string(),
            span { style: "font-family: var(--font-mono); min-width: 10ch; color: var(--md-sys-color-on-surface-variant);",
                {format_timestamp(row.ts)}
            }
            span { style: "font-family: var(--font-mono); flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis;",
                title: row.path.clone(),
                {row.method.clone()}
            }
            span { style: "font-family: var(--font-mono); color: var(--md-sys-color-on-surface-variant);",
                {row.path.clone()}
            }
            span { class: "md-label-s", style: status_style,
                {row.status.to_string()}
            }
            span { class: "md-label-s", style: "color: var(--md-sys-color-on-surface-variant);",
                {row.caller_label.clone().unwrap_or_else(|| {
                    short_identifier(row.caller_fp.as_deref().unwrap_or("no caller"), 12)
                })}
            }
        }
    }
}

/// Whether one row survives the filter bar.
fn row_matches(row: &AuditLogRow, view: &AuditView) -> bool {
    let fingerprint = view.fingerprint.trim();
    if !fingerprint.is_empty()
        && !row
            .caller_fp
            .as_deref()
            .unwrap_or_default()
            .contains(fingerprint)
    {
        return false;
    }
    let method = view.method.trim();
    if !method.is_empty() && !row.method.contains(method) {
        return false;
    }
    let path = view.path.trim();
    if !path.is_empty() && !row.path.contains(path) {
        return false;
    }
    true
}

/// Read the newest page, or the first when `cursor` is `None`.
fn load_page(pump: Pump, cursor: Option<String>, view: Signal<AuditView>) {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        let mut view = view;
        let request = ListAuditRows {
            cursor,
            limit: Some(PAGE_LIMIT),
            caller_fp: None,
            method: None,
        };
        let outcome: Result<AuditLogPage, _> = pump.rpc().call(&request).await;
        let mut view = view.write();
        match outcome {
            Ok(page) => {
                if view.rows.is_empty() {
                    view.rows = page.rows;
                } else {
                    view.rows.extend(page.rows);
                }
                view.next_cursor = page.next_cursor;
                view.loaded = true;
                view.loading_more = false;
                view.error = None;
            }
            Err(error) => {
                tracing::warn!(target: "settings", %error, "audit read refused");
                view.loaded = true;
                view.loading_more = false;
                view.error = Some(error.to_string());
            }
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, cursor, view);
}

/// Append the next page.
fn load_more(pump: Pump, cursor: Option<String>, mut view: Signal<AuditView>) {
    if cursor.is_none() {
        return;
    }
    view.write().loading_more = true;
    load_page(pump, cursor, view);
}
