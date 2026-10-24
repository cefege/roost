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
use crate::pump::{Pump, use_store};
use libero::components::{Badge, Button, Flex, Paper, Table, Text, TextField, column};

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
    let semibold_text_sx = use_hook(|| libero::sx::sx().font_weight("600"));
    let medium_text_sx = use_hook(|| libero::sx::sx().font_weight("500"));
    let paper_sx = use_hook(|| libero::sx::sx().padding("md"));

    let state = view();
    let rows: Vec<AuditLogRow> = state
        .rows
        .iter()
        .filter(|row| row_matches(row, &state))
        .cloned()
        .collect();
    rsx! {
        Flex { direction: "column", gap: "md", "data-testid": "audit-pane",
            Paper { sx: paper_sx.clone(),
                Flex { direction: "column", gap: "sm",
                    Text { size: "lg", sx: semibold_text_sx.clone(), "Filter" }
                    TextField {
                        label: "Caller fingerprint", placeholder: "authorized key fingerprint",
                        value: Some(state.fingerprint.clone()), oninput: move |value| view.write().fingerprint = value,
                    }
                    TextField {
                        label: "Method", placeholder: "SessionsSpawn",
                        value: Some(state.method.clone()), oninput: move |value| view.write().method = value,
                    }
                    TextField {
                        label: "Path contains", placeholder: "/roost.v1",
                        value: Some(state.path.clone()), oninput: move |value| view.write().path = value,
                    }
                }
            }
            Paper { sx: paper_sx,
                Flex { direction: "column", gap: "sm",
                    Text { size: "lg", sx: semibold_text_sx.clone(), "Audit log" }
                    Text { size: "sm", color: "muted", "Newest first. Rows arrive from the coordinator's own table; the live feed keeps this tab's rows fresh." }
                    if let Some(message) = state.error.clone() {
                        Text { size: "sm", color: "error", role: "alert", "{message}" }
                    }
                    if !state.loaded {
                        Text { size: "sm", color: "muted", "Loading…" }
                    } else if rows.is_empty() {
                        Flex { direction: "column", align: "center", gap: "xs",
                            Text { size: "md", sx: semibold_text_sx.clone(), "No audit rows" }
                            Text { size: "sm", color: "muted", "Nothing has matched yet. Widen the filter, or drive some coordinator traffic." }
                        }
                    } else {
                        Table {
                            data: rows,
                            row_attrs: |_: &AuditLogRow| vec![Attribute::new("data-testid", "audit-row", None, false)],
                            columns: vec![
                                column("Timestamp").value(|row: &AuditLogRow| format_timestamp(row.ts)).render(|row: &AuditLogRow| rsx! { Text { size: "xs", color: "muted", "{format_timestamp(row.ts)}" } }),
                                column("Method").value(|row: &AuditLogRow| row.method.clone()).render({ let medium_text_sx = medium_text_sx.clone(); move |row: &AuditLogRow| rsx! { Text { size: "sm", sx: medium_text_sx.clone(), "{row.method}" } } }),
                                column("Path").value(|row: &AuditLogRow| row.path.clone()).render(|row: &AuditLogRow| rsx! { Text { size: "sm", color: "muted", "{row.path}" } }),
                                column("Status").value(|row: &AuditLogRow| row.status.to_string()).render(|row: &AuditLogRow| rsx! { Badge { variant: if row.status >= 400 { "tonal" } else { "outlined" }, "{row.status}" } }),
                                column("Caller").value(|row: &AuditLogRow| row.caller_label.clone().unwrap_or_else(|| short_identifier(row.caller_fp.as_deref().unwrap_or("no caller"), 12))).render(|row: &AuditLogRow| rsx! { Text { size: "xs", color: "muted", "{row.caller_label.clone().unwrap_or_else(|| short_identifier(row.caller_fp.as_deref().unwrap_or(\"no caller\"), 12))}" } }),
                            ],
                        }
                    }
                    if state.next_cursor.is_some() {
                        Button {
                            variant: "standard",
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
