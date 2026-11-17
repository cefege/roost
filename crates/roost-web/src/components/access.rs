//! The shared Access surface for first load, pairing, settings and recovery.
//! Composes existing browser pairing, machine enrollment and access diagnostics.
//! Bookmarks are navigation targets, never evidence of connectivity or authorization.

mod configuration;
mod entry;
mod saved;
mod status;

use dioxus::prelude::*;
use roost_client_core::store::BrowserAccessState;

use crate::components::hooks::use_copy_clipboard::use_copy_clipboard;
use crate::components::machines::deploy_dialog::MachineDeployDialog;
use crate::components::pairing::{PairingRequester, PhonePairingCard};
use crate::components::ui::{
    Button, ButtonVariant, Card, CardContent, CardDescription, CardHeader, CardTitle, IconType,
    ListRow,
};
use crate::platform::storage::LocalStorageKeyValueStore;
use crate::pump::use_store;
use entry::ConnectEntry;
use saved::{SavedCoordinator, persist, restore_saved};
use status::AccessStatusCard;

#[component]
pub fn AccessSurface(#[props(default)] requester: Option<PairingRequester>) -> Element {
    let pump = use_store();
    let navigate = crate::router_state::use_navigate();
    let authorized =
        pump.core().borrow().store().browser_access_state == BrowserAccessState::Authorized;
    let mut saved = use_signal(|| restore_saved(&LocalStorageKeyValueStore::new()));
    let mut adding_machine = use_signal(|| false);
    let save_address = use_callback(move |origin: String| {
        if saved.peek().iter().any(|entry| entry.url == origin) {
            return;
        }
        saved.write().push(SavedCoordinator {
            name: origin.clone(),
            url: origin,
        });
        persist(&LocalStorageKeyValueStore::new(), &saved.read());
        tracing::info!(target: "access", "access address saved");
    });
    let remove_address = use_callback(move |origin: String| {
        saved.write().retain(|entry| entry.url != origin);
        persist(&LocalStorageKeyValueStore::new(), &saved.read());
        tracing::info!(target: "access", "access address removed");
    });
    rsx! {
        div { class: "flex min-w-0 flex-col gap-5", "data-testid": "access-surface",
            AccessStatusCard {}
            ConnectEntry { requester, on_save: move |origin| save_address.call(origin) }
            if !saved.read().is_empty() {
                Card {
                    CardHeader {
                        CardTitle { "Saved addresses" }
                        CardDescription { "Open the address in the browser. A saved address is not a verified route or an approved device." }
                    }
                    CardContent {
                        for coordinator in saved.read().iter() {
                            BookmarkRow { key: "{coordinator.url}", coordinator: coordinator.clone(), on_remove: move |origin| remove_address.call(origin) }
                        }
                    }
                }
            }
            if authorized {
                configuration::AccessConfiguration {}
                Card {
                    CardHeader {
                        CardTitle { "Machines and browsers" }
                        CardDescription { "A worker runs terminals on a computer. Pairing approves a browser or phone; these use separate grants." }
                    }
                    CardContent { class: "flex flex-wrap gap-2",
                        Button { variant: ButtonVariant::Default, onclick: move |_| adding_machine.set(true), "Add machine" }
                        Button { variant: ButtonVariant::Secondary, onclick: move |_| navigate.call("/pair".to_owned()), "Approve a browser" }
                        Button { variant: ButtonVariant::Outline, onclick: move |_| navigate.call("/settings/devices".to_owned()), "Review paired devices" }
                        Button { variant: ButtonVariant::Outline, onclick: move |_| navigate.call("/settings/machines".to_owned()), "Review machines" }
                    }
                }
                PhonePairingCard {}
                if adding_machine() { MachineDeployDialog { on_close: move |_| adding_machine.set(false) } }
            }
            Card {
                CardHeader {
                    CardTitle { "Set up access on the owning machine" }
                    CardDescription { "Local, Tailscale and stable public HTTPS routes belong to the installation, not this browser's bookmarks." }
                }
                CardContent { class: "flex flex-col gap-3",
                    p { class: "m-0 text-sm text-muted-foreground", "Run the access wizard on the computer that owns this installation:" }
                    code { class: "font-mono", "roost access" }
                    p { class: "m-0 text-sm text-muted-foreground", "For a Kubernetes/Flux installation, review changes in its deployment source. Do not start a competing coordinator or run host quickstart to repair it." }
                }
            }
        }
    }
}

#[component]
fn BookmarkRow(coordinator: SavedCoordinator, on_remove: EventHandler<String>) -> Element {
    let (copy, copied) = use_copy_clipboard(None, None);
    let open_url = coordinator.url.clone();
    let copy_url = coordinator.url.clone();
    let remove_url = coordinator.url.clone();
    rsx! {
        ListRow {
            leading_icon: Some(IconType::Server),
            headline: rsx! { "{coordinator.name}" },
            support: Some(rsx! { code { class: "break-all", "{coordinator.url}" } }),
            trailing: Some(rsx! {
                Button { variant: ButtonVariant::Secondary, onclick: move |_| { crate::platform::location::open_roost(&open_url, None); }, "Open" }
                Button { variant: ButtonVariant::Ghost, onclick: move |_| copy(&copy_url), if copied() { "Copied" } else { "Copy" } }
                Button { variant: ButtonVariant::Ghost, icon: Some(IconType::Trash2), "aria-label": "Remove saved address", onclick: move |_| on_remove.call(remove_url.clone()) }
            }),
        }
    }
}
