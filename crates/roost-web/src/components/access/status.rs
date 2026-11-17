//! Connection facts shown before authorization and inside Access settings.
//! Uses the boot-selected RPC endpoint, public identity probe and existing link liveness.
//! It does not promote public reachability into device authorization or offline authority.

use dioxus::prelude::*;
use roost_client_core::client::access::{
    AccessOverview, CoordinatorReachability, CoordinatorSelection, CoordinatorSource,
};
use roost_client_core::client::local::bootstrap::COORDINATOR_OVERRIDE_KEY;
use roost_client_core::client::rpc::calls::settings::machines::GetCoordinatorIdentity;
use roost_client_core::store::BrowserAccessState;
use roost_client_core::store::terminal_transport::has_liveness_qualified_direct_terminal;
use roost_client_core::{ClientEvent, KeyValueStore, TransportControl};

use crate::components::hooks::use_copy_clipboard::use_copy_clipboard;
use crate::components::terminal::dom::{page_visible, sleep_ms};
use crate::components::ui::{
    Button, ButtonVariant, Card, CardContent, CardDescription, CardHeader, CardTitle,
};
use crate::platform::storage::LocalStorageKeyValueStore;
use crate::pump::{Pump, use_store};

fn live_status(pump: &Pump) -> (CoordinatorReachability, bool) {
    let core = pump.core();
    let core = core.borrow();
    let rpc = pump.rpc();
    let overview = AccessOverview::read(
        core.store(),
        CoordinatorSelection {
            origin: rpc.base_url(),
            source: rpc.selection_source(),
        },
        "",
        crate::platform::network::browser_online(),
        core.clock().now_ms(),
        None,
    );
    (
        overview.reachability,
        has_liveness_qualified_direct_terminal(core.store()),
    )
}

#[component]
pub fn AccessStatusCard() -> Element {
    let pump = use_store();
    let origin = use_hook(crate::platform::door_probe::page_origin);
    let mut live = use_signal(|| live_status(&pump));
    let watched = pump.clone();
    use_future(move || {
        let watched = watched.clone();
        async move {
            loop {
                if page_visible() {
                    let next = live_status(&watched);
                    if *live.peek() != next {
                        live.set(next);
                    }
                }
                sleep_ms(2_000).await;
            }
        }
    });
    let probing = pump.clone();
    let mut probe = use_resource(move || {
        let rpc = probing.rpc();
        async move {
            rpc.call_public(&GetCoordinatorIdentity)
                .await
                .map_err(|error| error.to_string())
        }
    });
    let rpc = pump.rpc();
    let door = pump.local_worker_door();
    let core = pump.core();
    let core = core.borrow();
    let overview = AccessOverview::read(
        core.store(),
        CoordinatorSelection {
            origin: rpc.base_url(),
            source: rpc.selection_source(),
        },
        &origin,
        crate::platform::network::browser_online(),
        core.clock().now_ms(),
        door.as_ref().map(|door| door.origin.as_str()),
    );
    let authorization = match overview.authorization {
        BrowserAccessState::Checking => "Checking this browser's approval",
        BrowserAccessState::Unauthorized => "This browser needs approval",
        BrowserAccessState::Authorized => "This browser is approved",
    };
    let control_link = match live().0 {
        CoordinatorReachability::BrowserOffline => "Browser reports offline",
        CoordinatorReachability::Answering => "Control link is answering",
        CoordinatorReachability::Waiting => "Control link is not established",
    };
    let endpoint = overview.coordinator.origin.to_owned();
    let source = overview.coordinator.source.description();
    let reset_override = overview.coordinator.source == CoordinatorSource::StoredOverride;
    let revoked = core.store().sync.auth_revoked;
    let declared = overview
        .declared_public_url
        .filter(|url| !url.is_empty())
        .map(str::to_owned);
    let direct_live = live().1;
    drop(core);
    let (copy, copied) = use_copy_clipboard(None, None);
    let copy_endpoint = endpoint.clone();
    let retry = pump.clone();
    let answer = probe.read();
    rsx! {
        Card { "data-testid": "access-status",
            CardHeader {
                CardTitle { "Access to Roost" }
                CardDescription { "The server address, network connection and browser approval are separate facts." }
            }
            CardContent { class: "flex flex-col gap-3",
                dl { class: "grid grid-cols-1 gap-x-4 gap-y-1 text-sm sm:grid-cols-2",
                    dt { class: "text-muted-foreground", "Roost server" }
                    dd { class: "m-0 break-all font-mono", "data-testid": "access-effective-endpoint", "{endpoint}" }
                    dt { class: "text-muted-foreground", "Selected by" }
                    dd { class: "m-0", "{source}" }
                    if origin != endpoint {
                        dt { class: "text-muted-foreground", "Page opened from" }
                        dd { class: "m-0 break-all font-mono", "{origin}" }
                    }
                    dt { class: "text-muted-foreground", "Server reachability" }
                    dd { class: "m-0", "data-testid": "access-server-reachability",
                        match answer.as_ref() {
                            None => rsx! { "Checking the server…" },
                            Some(Ok(_)) => rsx! { "The server answered this page's identity check" },
                            Some(Err(message)) => rsx! {
                                span { "The server did not answer the identity check: {message}" }
                                p { class: "m-0 text-muted-foreground", "Check the address, network/TLS and any proxy sign-in. An edge sign-in failure does not mean Roost revoked this browser." }
                            },
                        }
                    }
                    dt { class: "text-muted-foreground", "Browser authorization" }
                    dd { class: "m-0", "data-testid": "access-browser-authorization", "{authorization}" }
                    dt { class: "text-muted-foreground", "Live control connection" }
                    dd { class: "m-0", "{control_link}" }
                    if let Some(declared) = declared {
                        dt { class: "text-muted-foreground", "Declared enrollment address" }
                        dd { class: "m-0 break-all font-mono", "{declared}" }
                    }
                    dt { class: "text-muted-foreground", "This computer's worker door" }
                    dd { class: "m-0",
                        if let Some(door) = door { code { class: "break-all", "{door.origin}" } }
                        else { "No reachable local worker door discovered" }
                    }
                }
                if revoked {
                    p { class: "m-0 text-sm text-muted-foreground", "Roost rejected this browser key. Reconnect cannot restore revoked access; request approval or explicitly recover the key." }
                }
                if direct_live {
                    p { class: "m-0 text-sm text-muted-foreground", "An established direct terminal still has a valid liveness proof. It can continue only while its grant remains valid; fleet controls and new grants require the Roost server." }
                }
                div { class: "flex flex-wrap gap-2",
                    Button { variant: ButtonVariant::Secondary,
                        onclick: move |_| { probe.restart(); retry.dispatch(ClientEvent::SyncTransportControl(TransportControl::Reconnect)); },
                        "Check again"
                    }
                    Button { variant: ButtonVariant::Outline,
                        onclick: move |_| copy(&copy_endpoint),
                        if copied() { "Copied" } else { "Copy server address" }
                    }
                    if reset_override {
                        Button { variant: ButtonVariant::Outline,
                            onclick: move |_| {
                                LocalStorageKeyValueStore::new().remove(COORDINATOR_OVERRIDE_KEY);
                                tracing::info!(target: "access", "explicit coordinator override cleared");
                                crate::platform::location::reload_document();
                            },
                            "Reload using this page's server"
                        }
                    }
                }
            }
        }
    }
}
