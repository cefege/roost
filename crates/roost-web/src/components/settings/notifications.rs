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
use libero::components::{Flex, Paper, Pictogram, Switch, Text, Title};
use roost_client_core::ClientEvent;
// The read is browser-only: a native build has no coordinator to ask, so the
// call type is gated with it.
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::settings::push::GetPushConfig;
use roost_client_core::store::prefs::notify::NotifyPref;
use roost_client_core::store::shell_intent::ShellIntent;

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
    let command_finished_pump = pump.clone();
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
    let medium_text_sx = use_hook(|| libero::sx::sx().font_weight("500"));

    rsx! {
        Flex { direction: "column", gap: "lg",
            "data-testid": "settings-notifications-pane",
            Paper { "data-testid": "notifications-status",
                Flex { align: "center", gap: "md",
                    Pictogram { icon: pictogram_icons_lucide::bell::outlined }
                    Flex { direction: "column", gap: "xs",
                        Text { size: "md", sx: medium_text_sx.clone(), "Agent notifications" }
                        Text { size: "sm", color: "muted", "Know when a background coding agent needs input or finishes. Saved per browser." }
                    }
                }
            }
            Paper {
                Title { size: "sm", "Surfaces" }
                Flex { direction: "column", gap: "md",
                    Flex { align: "center", justify: "space-between",
                        Flex { direction: "column", gap: "xs",
                            Text { size: "sm", sx: medium_text_sx.clone(), "In-app toasts" }
                            Text { size: "sm", color: "muted", "Show a delayed toast with a View action when a background agent needs input or finishes." }
                        }
                        Switch { label: "In-app toasts", checked: prefs.in_app, "data-testid": "notify-in-app-toggle",
                            onchange: move |value| set_pref(&in_app_pump, NotifyPref::InApp, value) }
                    }
                    Flex { align: "center", justify: "space-between",
                        Flex { direction: "column", gap: "xs",
                            Text { size: "sm", sx: medium_text_sx.clone(), "Desktop notifications" }
                            Text { size: "sm", color: "muted", "{row.support}" }
                        }
                        Switch { label: "Desktop notifications", checked: row.checked, disabled: row.disabled,
                            "data-testid": "notify-desktop-toggle",
                            onchange: move |value| toggle_desktop(&desktop_pump, desktop, value) }
                    }
                    if let Some(message) = error {
                        Text { color: "error", role: "alert", "data-testid": "notify-desktop-error", "{message}" }
                    }
                    Text { size: "sm", color: "muted", "data-testid": "notify-ios-home-screen",
                        "On iPhone and iPad, Safari delivers notifications only to an installed app: tap Share → Add to Home Screen, open Roost from the Home Screen, then turn this on there."
                    }
                    Flex { align: "center", justify: "space-between",
                        Flex { direction: "column", gap: "xs",
                            Text { size: "sm", sx: medium_text_sx.clone(), "Notify when a long command finishes" }
                            Text { size: "sm", color: "muted", "When a shell command that ran for 10 seconds or more finishes in a terminal you are not looking at, show a toast and play the finished sound." }
                        }
                        Switch { label: "Notify when a long command finishes", checked: prefs.command_finished,
                            "data-testid": "notify-command-finished-toggle",
                            onchange: move |value| set_pref(&command_finished_pump, NotifyPref::CommandFinished, value) }
                    }
                    Flex { align: "center", justify: "space-between",
                        Flex { direction: "column", gap: "xs",
                            Text { size: "sm", sx: medium_text_sx.clone(), "Tab title badge" }
                            Text { size: "sm", color: "muted", "Prefix the Roost tab title with the number of unseen needs-input and finished states." }
                        }
                        Switch { label: "Tab title badge", checked: prefs.title_badge,
                            "data-testid": "notify-title-badge-toggle",
                            onchange: move |value| set_pref(&title_pump, NotifyPref::TitleBadge, value) }
                    }
                }
            }
            Paper {
                Title { size: "sm", "Sound" }
                Flex { direction: "column", gap: "md",
                    Flex { align: "center", justify: "space-between",
                        Flex { direction: "column", gap: "xs",
                            Text { size: "sm", sx: medium_text_sx.clone(), "Sound when input is needed" }
                            Text { size: "sm", color: "muted", "Play two short ascending tones after a background agent starts waiting for you." }
                        }
                        Switch { label: "Sound when input is needed", checked: prefs.blocked_sound,
                            "data-testid": "notify-blocked-sound-toggle",
                            onchange: move |value| set_sound_pref(&blocked_pump, &blocked_tones, NotifyPref::BlockedSound, value) }
                    }
                    Flex { align: "center", justify: "space-between",
                        Flex { direction: "column", gap: "xs",
                            Text { size: "sm", sx: medium_text_sx.clone(), "Sound when finished" }
                            Text { size: "sm", color: "muted", "Play a short tone after a background agent or terminal command completes." }
                        }
                        Switch { label: "Sound when finished", checked: prefs.done_sound,
                            "data-testid": "notify-done-sound-toggle",
                            onchange: move |value| set_sound_pref(&done_pump, &tones, NotifyPref::DoneSound, value) }
                    }
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
