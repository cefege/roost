//! The two screens that exist before a protected surface does: the checking
//! screen, and the refusal. Ported from
//! `apps/web/src/components/pairing/AccessCheckingScreen.tsx`'s slot in App.tsx's
//! `RootShell` switch, and from the `.access-checking` rules in
//! `assets/styles/workbench-shell.css`.
//!
//! THE GATE IS NOT THE PAIRING FLOW. Pairing is a surface with its own route
//! and its own ceremony; this module is what a reader sees while the client has
//! not yet been told whether this device key is trusted, and what they see when
//! the answer is no. Both are states, not steps.
//!
//! The checking screen is a spinner and a word, deliberately. A reader who
//! arrives with a stale credential sees "Checking…" for as long as the round trip
//! takes, and a screen that pretended to know more than that would be lying
//! during exactly the window where they are deciding whether to trust the page.

use dioxus::prelude::*;

use crate::components::design_icon::Icon;

/// The screen shown while the coordinator has not answered.
///
/// Full height and centred, because it replaces the whole application rather
/// than sitting inside the shell: drawing the shell around a spinner would
/// present a frame the reader cannot use.
#[component]
pub fn CheckingScreen() -> Element {
    rsx! {
        div { class: "access-checking", "data-testid": "access-checking",
            div { class: "access-checking__card", role: "status", "aria-live": "polite",
                Icon { name: "hourglass_top", filled: false }
                span { "Checking access…" }
            }
        }
    }
}

/// What the refusal says, and where it sends the reader.
///
/// A device key this coordinator does not trust cannot be repaired from a
/// protected page — the page is exactly what the key is not trusted to read. So
/// the only honest action is the pairing route, and the copy says the key was
/// refused rather than that something went wrong.
#[component]
pub fn UnauthorizedScreen(on_navigate: EventHandler<String>) -> Element {
    let pair_path = crate::routes::Route::Pair.to_path();
    rsx! {
        div { class: "access-checking", "data-testid": "access-unauthorized",
            div { class: "access-checking__card", role: "alert",
                Icon { name: "lock", filled: false }
                span { "This browser is not authorized on this coordinator." }
                a { href: pair_path.clone(), onclick: move |_| on_navigate.call(pair_path.clone()),
                    "data-testid": "access-unauthorized-pair",
                    "Pair this browser"
                }
            }
        }
    }
}
