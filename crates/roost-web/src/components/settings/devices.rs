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
use crate::components::pairing::PhonePairingCard;
use crate::pump::{Pump, use_store};
use crate::router_state::use_navigate;
use crate::routes::Route;
use libero::components::{Button, Flex, Paper, Pictogram, Text, Title};

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
    let load_pump = pump.clone();
    use_effect(move || reload(load_pump.clone(), view));
    let state = view();
    rsx! {
        Flex { direction: "column", gap: "md", "data-testid": "settings-devices-pane",
            Paper {
                Flex { direction: "column", gap: "sm",
                    Title { size: "md", "Authorized devices" }
                    Text { size: "sm", color: "muted",
                        "Browsers authorized to access this coordinator. Revocation is permanent for that key."
                    }
                    if !state.loaded && state.error.is_none() {
                        Text { size: "sm", color: "muted", "aria-live": "polite", "Loading devices…" }
                    }
                    if let Some(message) = state.error.clone() {
                        Text { color: "error", role: "alert", "{message}" }
                    }
                    if state.loaded && state.error.is_none() && state.devices.is_empty() {
                        Flex { direction: "column", align: "center", gap: "xs",
                            Pictogram { icon: pictogram_icons_lucide::monitor_smartphone::outlined }
                            Text { size: "sm", "No browser devices" }
                            Text { size: "sm", color: "muted", "Pair a browser below, then refresh this list." }
                        }
                    }
                    if !state.devices.is_empty() {
                        Flex { direction: "column", gap: "sm",
                            for device in state.devices.iter() {
                                DeviceRow { device: device.clone(), view, pump: pump.clone() }
                            }
                        }
                    }
                }
            }
            PhonePairingCard {}
            Paper {
                Flex { direction: "column", gap: "sm",
                    Title { size: "md", "Approve a browser" }
                    Text { size: "sm", color: "muted",
                        "A browser that requested access waits on the pairing page until you approve it with the code it shows."
                    }
                    Button {
                        variant: "outlined",
                        icon: Some(rsx! { Pictogram { icon: pictogram_icons_lucide::monitor_smartphone::outlined } }),
                        "data-testid": "settings-pair-device",
                        onclick: move |_| navigate.call(Route::Pair.to_path()),
                        "Open pairing"
                    }
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
        Flex { direction: "row", align: "center", justify: "space-between", gap: "md",
            "data-testid": format!("authorized-device-{fingerprint}"),
            Flex { direction: "row", align: "center", gap: "sm",
                Pictogram {
                    icon: if device.is_self { pictogram_icons_lucide::monitor_smartphone::outlined } else { pictogram_icons_lucide::laptop::outlined },
                }
                Flex { direction: "column", gap: "xs",
                    Flex { direction: "row", align: "center", gap: "xs",
                        Text { size: "sm", "{label}" }
                        if device.is_self {
                            Text { size: "xs", color: "primary", "This device" }
                        }
                    }
                    Text { size: "xs", color: "muted",
                        "{fingerprint} · {format_timestamp(device.added_at_ms / 1_000)}"
                    }
                    Text { size: "xs", color: "muted", "{provenance}" }
                }
            }
            if device.is_self {
                Text { size: "xs", color: "muted", "You cannot revoke this browser" }
            } else {
                Button {
                    variant: "outlined",
                    color: "error",
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
