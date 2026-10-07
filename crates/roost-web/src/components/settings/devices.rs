//! Settings → Network → Devices: the browsers authorized on this coordinator.
//!
//! Ports `apps/web/src/components/Settings/DevicesPane.tsx`'s authorized-device
//! card. Depends on `roost-client-core`'s `DevicesList`/`DevicesRevoke` calls.
//!
//! A REFUSAL IS A STATE, NOT AN ABSENCE. The coordinator's denied, expired and
//! revoked answers are shown as themselves: a read that comes back refused says
//! so in a `role="alert"` line rather than rendering "No browser devices", which
//! is what an empty list means and would read as a fact about the fleet.

use dioxus::prelude::*;
use roost_client_core::client::rpc::calls::settings::devices::{
    AuthorizedDevice, pairing_provenance,
};
// The round trips are browser-only: a native build has no coordinator to ask,
// so the call types and the toast their refusals raise are gated with them.
#[cfg(target_arch = "wasm32")]
use roost_client_core::ClientEvent;
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::settings::devices::{ListDevices, RevokeDevice};
#[cfg(target_arch = "wasm32")]
use roost_client_core::store::shell_intent::ShellIntent;

use super::format::format_timestamp;
use crate::components::md::{Button, ButtonVariant, Card, EmptyState, List, ListRow};
use crate::components::pairing::PhonePairingCard;
use crate::pump::{Pump, use_store};
use crate::router_state::use_navigate;
use crate::routes::Route;

/// The device list, and whatever the coordinator said about the last read.
#[derive(Debug, Clone, PartialEq, Default)]
struct DevicesView {
    devices: Vec<AuthorizedDevice>,
    loaded: bool,
    error: Option<String>,
    busy: Option<String>,
}

/// The pane.
#[component]
pub fn DevicesPane() -> Element {
    let pump = use_store();
    let navigate = use_navigate();
    let view = use_signal(DevicesView::default);
    // The effect outlives this render, so it takes its OWN clone: a `move`
    // closure over `pump` itself would consume the binding the revoke handler
    // below still reads.
    let load_pump = pump.clone();
    use_effect(move || reload(load_pump.clone(), view));
    let state = view();
    rsx! {
        div {
            class: "settings-pane",
            style: "display: flex; flex-direction: column; gap: var(--md-space-5);",
            "data-testid": "settings-devices-pane",
            Card {
                title: "Authorized devices",
                supporting: "Browsers authorized to access this coordinator. Revocation is permanent for that key.",
                if !state.loaded && state.error.is_none() {
                    p { class: "md-body-m", "aria-live": "polite", style: "color: var(--md-sys-color-on-surface-variant); margin: 0;",
                        "Loading devices…"
                    }
                }
                if let Some(message) = state.error.clone() {
                    p { role: "alert", class: "md-body-m", style: "color: var(--md-sys-color-error); margin: 0;",
                        {message}
                    }
                }
                if state.loaded && state.error.is_none() && state.devices.is_empty() {
                    EmptyState {
                        icon: "devices",
                        title: "No browser devices",
                        supporting: "Pair a browser below, then refresh this list.",
                    }
                }
                if !state.devices.is_empty() {
                    List {
                        for device in state.devices.iter() {
                            DeviceRow { device: device.clone(), view, pump: pump.clone() }
                        }
                    }
                }
            }
            PhonePairingCard {}
            Card {
                title: "Approve a browser",
                supporting: "A browser that requested access waits on the pairing page until you approve it with the code it shows.",
                Button {
                    variant: ButtonVariant::Secondary,
                    icon: "devices",
                    "data-testid": "settings-pair-device",
                    onclick: move |_| navigate.call(Route::Pair.to_path()),
                    "Open pairing"
                }
            }
        }
    }
}

/// One authorized browser, and the revoke that ends its access.
#[component]
fn DeviceRow(device: AuthorizedDevice, view: Signal<DevicesView>, pump: Pump) -> Element {
    let label = if device.label.trim().is_empty() {
        "Unnamed browser".to_owned()
    } else {
        device.label.clone()
    };
    let fingerprint = device.fingerprint.clone();
    let busy = view().busy.as_deref() == Some(fingerprint.as_str());
    let provenance = pairing_provenance(&device);
    rsx! {
        ListRow {
            test_id: Some(format!("authorized-device-{fingerprint}")),
            leading_icon: Some(if device.is_self { "devices".to_owned() } else { "laptop_chromebook".to_owned() }),
            headline: rsx! {
                span { style: "display: inline-flex; align-items: baseline; gap: var(--md-space-2); max-width: 100%;",
                    span { style: "overflow: hidden; text-overflow: ellipsis;", {label.clone()} }
                    if device.is_self {
                        span { class: "md-label-s", style: "color: var(--md-sys-color-primary); flex-shrink: 0;", "This device" }
                    }
                }
            },
            support: rsx! {
                span { style: "display: block; overflow-wrap: anywhere;",
                    span { style: "font-family: var(--term-font-family);", {fingerprint.clone()} }
                    span { " · " }
                    time { datetime: format_timestamp(device.added_at_ms / 1_000) }
                }
                span { style: "display: block; overflow-wrap: anywhere;", {provenance.clone()} }
            },
            trailing: rsx! {
                if device.is_self {
                    span { class: "md-label-s", style: "color: var(--md-sys-color-on-surface-variant);", "You cannot revoke this browser" }
                } else {
                    Button {
                        variant: ButtonVariant::Secondary,
                        "aria-label": format!("Revoke {label}"),
                        "data-testid": format!("revoke-device-{fingerprint}"),
                        disabled: busy,
                        onclick: {
                            let fingerprint = fingerprint.clone();
                            move |_| revoke(pump.clone(), fingerprint.clone(), view)
                        },
                        if busy { "Revoking…" } else { "Revoke" }
                    }
                }
            },
        }
    }
}

/// Re-read the authorized devices.
fn reload(pump: Pump, view: Signal<DevicesView>) {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        let mut view = view;
        match pump.rpc().call(&ListDevices).await {
            Ok(devices) => {
                let mut view = view.write();
                view.devices = devices;
                view.loaded = true;
                view.error = None;
            }
            Err(error) => {
                tracing::warn!(target: "settings", %error, "device list refused");
                let mut view = view.write();
                view.loaded = true;
                view.error = Some(error.to_string());
            }
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, view);
}

/// Revoke one browser's key, and say so when the coordinator will not.
fn revoke(pump: Pump, fingerprint: String, mut view: Signal<DevicesView>) {
    view.write().busy = Some(fingerprint.clone());
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        let request = RevokeDevice {
            fingerprint: fingerprint.clone(),
        };
        let outcome = pump.rpc().call(&request).await;
        let refused = match outcome {
            Ok(true) => None,
            Ok(false) => Some("the coordinator has no such device".to_owned()),
            Err(error) => Some(error.to_string()),
        };
        let mut state = view.write();
        state.busy = None;
        state
            .devices
            .retain(|device| device.fingerprint != fingerprint);
        state.loaded = true;
        let refusal = refused.clone();
        drop(state);
        if let Some(reason) = refusal {
            view.write().error = Some(format!("Revoke failed: {reason}"));
            pump.dispatch(ClientEvent::Shell(ShellIntent::ActionFailed {
                message: format!("Revoke failed: {reason}"),
            }));
            return;
        }
        reload(pump.clone(), view);
        pump.dispatch(ClientEvent::Shell(ShellIntent::ActionSucceeded {
            message: "Device revoked".to_owned(),
        }));
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, fingerprint, view);
}
