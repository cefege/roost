//! The terminal bell's browser half. Mounted once by `NotificationDock`: drains
//! `Store::terminal_bells` rings and presents each per Settings → Terminal
//! "Bell" — a brief flash of every pane showing the session, a short tone, both,
//! or nothing — then clears the unseen mark of every ringing session that is on
//! screen in a visible tab. Never a toast and never an OS push: a bell is a
//! nudge inside the terminal, not news.

use std::rc::Rc;

use dioxus::prelude::*;
use roost_client_core::ClientEvent;
use roost_client_core::store::prefs::TerminalBell;
use roost_client_core::store::shell_intent::ShellIntent;

use super::agent_notifications::notification_tone::{ToneNote, TonePlayer};
use crate::pump::use_store;

/// One short, high, quiet note: a bell is a nudge, quieter than the agent cues.
pub const BELL_CUE: [ToneNote; 1] = [ToneNote {
    frequency_hz: 988.0,
    offset_s: 0.0,
    duration_s: 0.09,
    peak_gain: 0.06,
}];

/// How long a flash attribute stays on a pane, matching the stylesheet's
/// `terminal-bell-flash` animation. Removed afterwards so a pane that is
/// re-shown later does not replay an old flash.
#[cfg(target_arch = "wasm32")]
const FLASH_MS: i32 = 400;

/// What a ring does under the device's bell choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BellSurfaces {
    /// Flash the panes showing the session.
    pub flash: bool,
    /// Play [`BELL_CUE`].
    pub sound: bool,
}

impl BellSurfaces {
    /// The surfaces `choice` turns on.
    #[must_use]
    pub const fn for_choice(choice: TerminalBell) -> Self {
        match choice {
            TerminalBell::Visual => Self {
                flash: true,
                sound: false,
            },
            TerminalBell::Sound => Self {
                flash: false,
                sound: true,
            },
            TerminalBell::VisualAndSound => Self {
                flash: true,
                sound: true,
            },
            TerminalBell::Off => Self {
                flash: false,
                sound: false,
            },
        }
    }
}

/// Presents rings and clears the marks of sessions on screen.
#[component]
pub fn TerminalBellPresenter() -> Element {
    let pump = use_store();
    let tones = use_hook(|| Rc::new(TonePlayer::default()));
    use_effect(move || {
        let _ = pump.revision().read();
        let (rings, choice, unseen) = {
            let core = pump.core();
            let mut core = core.borrow_mut();
            let store = core.store_mut();
            let rings = store.terminal_bells.take_rings();
            let unseen: Vec<String> = store.terminal_bells.unseen().map(str::to_owned).collect();
            (rings, store.prefs.terminal_bell, unseen)
        };
        let surfaces = BellSurfaces::for_choice(choice);
        if surfaces.flash {
            for session_id in &rings {
                flash_session_panes(session_id);
            }
        }
        // Several rings in one revision are one tone: a burst is one event to
        // the ear, and stacked oscillators would only be louder.
        if surfaces.sound && !rings.is_empty() {
            tones.play_notes(&BELL_CUE);
        }
        for session_id in unseen.into_iter().filter(|id| session_on_screen(id)) {
            pump.dispatch(ClientEvent::Shell(ShellIntent::ClearTerminalBell {
                session_id,
            }));
        }
    });
    rsx! {}
}

/// Every mounted pane element showing `session_id`.
#[cfg(target_arch = "wasm32")]
fn session_panes(session_id: &str) -> Vec<web_sys::Element> {
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return Vec::new();
    };
    let escaped = session_id.replace('\\', "\\\\").replace('"', "\\\"");
    let selector = format!("[data-testid=\"cell-terminal-pane\"][data-session-id=\"{escaped}\"]");
    let Ok(nodes) = document.query_selector_all(&selector) else {
        return Vec::new();
    };
    (0..nodes.length())
        .filter_map(|index| nodes.item(index))
        .filter_map(|node| wasm_bindgen::JsCast::dyn_into::<web_sys::Element>(node).ok())
        .collect()
}

/// Restart the flash on every pane showing `session_id`. Removing the
/// attribute and reading layout in between is what restarts a CSS animation
/// that is already running from a ring a moment ago.
#[cfg(target_arch = "wasm32")]
fn flash_session_panes(session_id: &str) {
    use wasm_bindgen::JsCast as _;

    for pane in session_panes(session_id) {
        let _ = pane.remove_attribute("data-bell");
        if let Some(html) = pane.dyn_ref::<web_sys::HtmlElement>() {
            let _ = html.offset_width();
        }
        let _ = pane.set_attribute("data-bell", "ring");
        let clear = wasm_bindgen::closure::Closure::once_into_js(move || {
            let _ = pane.remove_attribute("data-bell");
        });
        if let Some(window) = web_sys::window() {
            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                clear.unchecked_ref(),
                FLASH_MS,
            );
        }
    }
}

/// Whether `session_id` is on screen: a pane showing it is laid out and this
/// tab is the one the operator is looking at.
#[cfg(target_arch = "wasm32")]
fn session_on_screen(session_id: &str) -> bool {
    let visible = web_sys::window()
        .and_then(|window| window.document())
        .is_some_and(|document| document.visibility_state() == web_sys::VisibilityState::Visible);
    visible
        && session_panes(session_id)
            .iter()
            .any(|pane| pane.get_client_rects().length() > 0)
}

#[cfg(not(target_arch = "wasm32"))]
fn flash_session_panes(_session_id: &str) {}

#[cfg(not(target_arch = "wasm32"))]
fn session_on_screen(_session_id: &str) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::{BellSurfaces, TerminalBell};

    #[test]
    fn each_choice_turns_on_exactly_its_surfaces() {
        assert_eq!(
            BellSurfaces::for_choice(TerminalBell::Visual),
            BellSurfaces {
                flash: true,
                sound: false
            }
        );
        assert_eq!(
            BellSurfaces::for_choice(TerminalBell::Sound),
            BellSurfaces {
                flash: false,
                sound: true
            }
        );
        assert_eq!(
            BellSurfaces::for_choice(TerminalBell::VisualAndSound),
            BellSurfaces {
                flash: true,
                sound: true
            }
        );
        assert_eq!(
            BellSurfaces::for_choice(TerminalBell::Off),
            BellSurfaces {
                flash: false,
                sound: false
            }
        );
    }
}
