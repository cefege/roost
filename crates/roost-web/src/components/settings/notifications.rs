//! Settings → Interface → Notifications: how loudly an agent interrupts.
//!
//! Ports `apps/web/src/components/Settings/NotificationsPane.tsx`. The five
//! switches are per-browser preferences in `roost_client_core::store::prefs`.
//! The Desktop switch additionally asks for the browser's permission inside
//! the click and subscribes this browser through `crate::web_push`; its
//! preference turns on only once both have succeeded. A sound switch turned on
//! previews its cue through `notification_tone`.

use std::rc::Rc;

use dioxus::prelude::*;
use roost_client_core::ClientEvent;
// The read is browser-only: a native build has no coordinator to ask, so the
// call type is gated with it.
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::settings::push::GetPushConfig;
use roost_client_core::store::prefs::notify::NotifyPref;
use roost_client_core::store::shell_intent::ShellIntent;

use crate::components::md::{Card, Icon, IconSize, SwitchRow};
use crate::components::notifications::agent_notifications::notification_tone::{
    TonePlayer, previewed_kind,
};
use crate::pump::{Pump, use_store};
use crate::web_push::desktop_push_plan::{DesktopPushFacts, PushPermission, desktop_push_row};

/// The Desktop row's live facts that are not the stored preference.
#[derive(Clone, Copy)]
struct DesktopPushSignals {
    coordinator_available: Signal<bool>,
    permission: Signal<PushPermission>,
    busy: Signal<bool>,
    error: Signal<Option<String>>,
}

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
    let tones = use_hook(|| Rc::new(TonePlayer::default()));
    let blocked_tones = Rc::clone(&tones);
    let desktop = DesktopPushSignals {
        coordinator_available: use_signal(|| false),
        permission: use_signal(current_permission),
        busy: use_signal(|| false),
        error: use_signal(|| None),
    };
    let available = desktop.coordinator_available;
    use_effect(move || read_push_availability(load_pump.clone(), available));

    let row = desktop_push_row(DesktopPushFacts {
        browser_supported: browser_supports_push(),
        coordinator_available: (desktop.coordinator_available)(),
        permission: (desktop.permission)(),
        enabled: prefs.desktop,
        busy: (desktop.busy)(),
    });
    let error = (desktop.error)();

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
                    support: row.support,
                    checked: row.checked,
                    disabled: row.disabled,
                    on_change: move |value| toggle_desktop(&desktop_pump, desktop, value),
                }
                if let Some(message) = error {
                    p { role: "alert", class: "md-body-s", "data-testid": "notify-desktop-error",
                        style: "color: var(--md-sys-color-error); margin: 0;",
                        {message}
                    }
                }
                p { class: "md-body-s", "data-testid": "notify-ios-home-screen",
                    style: "color: var(--md-sys-color-on-surface-variant); margin: 0;",
                    "On iPhone and iPad, Safari delivers notifications only to an installed app: tap Share → Add to Home Screen, open Roost from the Home Screen, then turn this on there."
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
                    on_change: move |value| set_sound_pref(&blocked_pump, &blocked_tones, NotifyPref::BlockedSound, value),
                }
                SwitchRow {
                    test_id: "notify-done-sound-toggle",
                    headline: "Sound when finished",
                    support: "Play a short tone after a background agent completes its work.",
                    checked: prefs.done_sound,
                    on_change: move |value| set_sound_pref(&done_pump, &tones, NotifyPref::DoneSound, value),
                }
            }
        }
    }
}

/// Write one switch through the store, so it persists and repaints together.
fn set_pref(pump: &Pump, pref: NotifyPref, value: bool) {
    pump.dispatch(ClientEvent::Shell(ShellIntent::SetNotifyPref {
        pref,
        value,
    }));
}

/// Write a sound switch, and play its cue when it turns on: the click is the
/// gesture the browser's autoplay policy wants, and the reader hears what they
/// just chose.
fn set_sound_pref(pump: &Pump, tones: &TonePlayer, pref: NotifyPref, value: bool) {
    set_pref(pump, pref, value);
    if let Some(kind) = previewed_kind(pref).filter(|_| value) {
        tones.play(kind);
    }
}

/// Turn Desktop notifications on or off from the switch's click.
///
/// The permission prompt is STARTED here, before any `await`: Safari and
/// Firefox show it only to a user gesture, and the gesture does not survive a
/// round trip to the coordinator.
fn toggle_desktop(pump: &Pump, signals: DesktopPushSignals, requested: bool) {
    #[cfg(target_arch = "wasm32")]
    {
        use crate::web_push::desktop_push_plan::preference_after_toggle;
        use crate::web_push::{browser_push, push_lifecycle};

        let DesktopPushSignals {
            mut permission,
            mut busy,
            mut error,
            ..
        } = signals;
        if *busy.peek() {
            return;
        }
        let permission_request = requested
            .then(browser_push::request_permission_in_gesture)
            .flatten();
        busy.set(true);
        error.set(None);
        let pump = pump.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let rpc = pump.rpc();
            let outcome = if requested {
                push_lifecycle::enable_desktop_push(&rpc, permission_request).await
            } else {
                push_lifecycle::disable_desktop_push(&rpc).await
            };
            set_pref(
                &pump,
                NotifyPref::Desktop,
                preference_after_toggle(requested, &outcome),
            );
            if let Err(failure) = &outcome {
                tracing::warn!(target: "settings", requested, error = %failure, "desktop notifications toggle failed");
                error.set(Some(failure.to_string()));
            }
            permission.set(browser_push::current_permission());
            busy.set(false);
        });
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, signals, requested);
}

/// Whether this browser can hold a push subscription. A native build cannot.
fn browser_supports_push() -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        crate::web_push::browser_push::browser_supports_push()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        false
    }
}

/// The browser's notification permission. A native build has none to grant.
fn current_permission() -> PushPermission {
    #[cfg(target_arch = "wasm32")]
    {
        crate::web_push::browser_push::current_permission()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        PushPermission::Default
    }
}

/// Whether this coordinator will accept a push subscription at all.
///
/// The row is enabled from this answer as well as the browser's own support,
/// because a browser that supports Web Push on a coordinator that has no VAPID
/// key still cannot deliver anything.
fn read_push_availability(pump: Pump, available: Signal<bool>) {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        let mut available = available;
        match pump.rpc().call(&GetPushConfig).await {
            Ok(config) => available.set(config.application_server_key().is_ok()),
            Err(error) => {
                tracing::warn!(target: "settings", %error, "push config read refused");
                available.set(false);
            }
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, available);
}
