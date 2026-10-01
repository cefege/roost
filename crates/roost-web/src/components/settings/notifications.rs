//! Settings → Interface → Notifications: how loudly an agent interrupts.
//!
//! Ports `apps/web/src/components/Settings/NotificationsPane.tsx`. The five
//! switches are per-browser preferences in `roost_client_core::store::prefs`;
//! the desktop toggle additionally needs a browser permission and a
//! coordinator subscription, in that order, so a switch that claims desktop
//! delivery before either has happened is a promise the browser cannot keep.
//!
//! Desktop delivery's browser half — `Notification.requestPermission` and the
//! PushManager subscription — belongs to the notifications slice's carrier
//! work. This pane owns the four local switches and the status wording, and
//! leaves the desktop row visible and honest about what has not been granted.

use dioxus::prelude::*;
use roost_client_core::ClientEvent;
// The read is browser-only: a native build has no coordinator to ask, so the
// call type is gated with it.
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::settings::push::GetPushConfig;
use roost_client_core::store::prefs::notify::NotifyPref;
use roost_client_core::store::shell_intent::ShellIntent;

use crate::components::md::{Card, Icon, IconSize, SwitchRow};
use crate::pump::{Pump, use_store};

/// The pane.
#[component]
pub fn NotificationsPane() -> Element {
    let pump = use_store();
    let core = pump.core();
    let prefs = core.borrow().store().prefs.notify;

    let load_pump = pump.clone();
    let in_app_pump = pump.clone();
    let desktop_pump = pump.clone();
    let title_pump = pump.clone();
    let blocked_pump = pump.clone();
    let done_pump = pump.clone();
    let push_available = use_signal(|| false);
    use_effect(move || read_push_availability(load_pump.clone(), push_available));

    rsx! {
        div {
            class: "settings-pane",
            style: "display: flex; flex-direction: column; gap: var(--md-space-5);",
            "data-testid": "settings-notifications-pane",
            Card { class: "settings-hero", test_id: "notifications-status",
                div { style: "display: flex; align-items: center; gap: var(--md-space-4);",
                    Icon { name: "notifications", filled: true, size: IconSize::Lg }
                    div { style: "flex: 1; min-width: 0;",
                        div { class: "md-title-m", "Agent notifications" }
                        div { class: "md-body-s", style: "color: var(--md-sys-color-on-surface-variant);",
                            "Know when a background coding agent needs input or finishes. Saved per browser."
                        }
                    }
                }
            }
            Card { title: "Surfaces",
                SwitchRow {
                    test_id: "notify-in-app-toggle",
                    headline: "In-app toasts",
                    support: "Show a delayed toast with a View action when a background agent needs input or finishes.",
                    checked: prefs.in_app,
                    on_change: move |value| set_pref(&in_app_pump, NotifyPref::InApp, value),
                }
                SwitchRow {
                    test_id: "notify-desktop-toggle",
                    headline: "Desktop notifications",
                    support: desktop_status(push_available()),
                    checked: prefs.desktop,
                    disabled: !push_available(),
                    on_change: move |value| set_pref(&desktop_pump, NotifyPref::Desktop, value),
                }
                SwitchRow {
                    test_id: "notify-title-badge-toggle",
                    headline: "Tab title badge",
                    support: "Prefix the Roost tab title with the number of unseen needs-input and finished states.",
                    checked: prefs.title_badge,
                    on_change: move |value| set_pref(&title_pump, NotifyPref::TitleBadge, value),
                }
            }
            Card { title: "Sound",
                SwitchRow {
                    test_id: "notify-blocked-sound-toggle",
                    headline: "Sound when input is needed",
                    support: "Play two short ascending tones after a background agent starts waiting for you.",
                    checked: prefs.blocked_sound,
                    on_change: move |value| set_pref(&blocked_pump, NotifyPref::BlockedSound, value),
                }
                SwitchRow {
                    test_id: "notify-done-sound-toggle",
                    headline: "Sound when finished",
                    support: "Play a short tone after a background agent completes its work.",
                    checked: prefs.done_sound,
                    on_change: move |value| set_pref(&done_pump, NotifyPref::DoneSound, value),
                }
            }
        }
    }
}

/// The desktop row's support line, in v2's wording for each state.
fn desktop_status(available: bool) -> String {
    if !available {
        return "Desktop notifications are unavailable in this browser. On Safari and iOS, install Roost to the Home Screen first.".to_owned();
    }
    "Get an OS notification even when Roost is closed. Enabling asks for browser permission and subscribes this browser to the coordinator.".to_owned()
}

/// Write one switch through the store, so it persists and repaints together.
fn set_pref(pump: &Pump, pref: NotifyPref, value: bool) {
    pump.dispatch(ClientEvent::Shell(ShellIntent::SetNotifyPref {
        pref,
        value,
    }));
}

/// Whether this coordinator will accept a push subscription at all.
///
/// The row is enabled from this answer rather than from the browser's own
/// support, because a browser that supports Web Push on a coordinator that has
/// no VAPID key still cannot deliver anything.
fn read_push_availability(pump: Pump, available: Signal<bool>) {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        let mut available = available;
        match pump.rpc().call(&GetPushConfig).await {
            Ok(config) => {
                available.set(config.available && !config.vapid_public_key_b64.is_empty())
            }
            Err(error) => {
                tracing::warn!(target: "settings", %error, "push config read refused");
                available.set(false);
            }
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, available);
}
