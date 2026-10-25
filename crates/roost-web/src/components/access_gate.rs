//! The two screens that exist before a protected surface does: the checking
//! screen, and the refusal. Ported from
//! `apps/web/src/components/pairing/AccessCheckingScreen.tsx`'s slot in App.tsx's
//! `RootShell` switch, and from the checking-screen rules the old workbench
//! stylesheet carried, now Tailwind classes.
//!
//! THE REFUSAL IS NOT THE PAIRING PAGE. `components::pairing::PairSurface` is:
//! the root mounts it above the gate, so an unpaired reader at `/`, at
//! `/settings/devices` or at `/search` finds the same working requester panel
//! v2 shows there (`Onboarding.tsx:109-111`). What this file still owns is the
//! checking screen and the one-line diagnosis, which the panel's own notices
//! do not replace — they say what went wrong with a request, not why this
//! device key was refused in the first place, and a reader whose key was
//! REVOKED needs to hear that rather than infer it from a failed pairing.
//!
//! The checking screen is a spinner and a word, deliberately. A reader who
//! arrives with a stale credential sees "Checking…" for as long as the round trip
//! takes, and a screen that pretended to know more than that would be lying
//! during exactly the window where they are deciding whether to trust the page.

use dioxus::prelude::*;

use crate::components::ui::{Alert, AlertVariant, Icon, IconType};

/// The full-height centring frame both refusal screens share. The TV overscan
/// variant keeps the padding the TV mode gives the checking screen.
pub const CHECKING_CLASS: &str = "flex min-h-dvh w-full items-center justify-center p-6 tv:!py-[var(--tv-overscan-block)] tv:!px-[var(--tv-overscan-inline)]";
/// The alert that holds a gate's message. Its live region contains the whole
/// announcement.
#[component]
pub fn GateCard(
    #[props(into)] role: String,
    #[props(into, optional)] aria_live: Option<String>,
    children: Element,
) -> Element {
    rsx! {
        Alert {
            variant: AlertVariant::Default,
            role,
            aria_live,
            class: "flex items-center gap-2",
            {children}
        }
    }
}

/// The screen shown while the coordinator has not answered.
///
/// Full height and centred, because it replaces the whole application rather
/// than sitting inside the shell: drawing the shell around a spinner would
/// present a frame the reader cannot use.
#[component]
pub fn CheckingScreen() -> Element {
    rsx! {
        div { class: CHECKING_CLASS, "data-testid": "access-checking",
            GateCard { role: "status", aria_live: "polite",
                Icon { icon: IconType::Hourglass, class: "size-4" }
                span { "Checking access…" }
            }
        }
    }
}

/// What the refusal says, and where it sends the reader.
///
/// The only honest action from a protected page is the pairing route, and the
/// copy says the KEY was refused rather than that something went wrong. A
/// device key this coordinator does not trust cannot be repaired from a page —
/// the page is exactly what the key is not trusted to read.
#[component]
pub fn UnauthorizedScreen(on_navigate: EventHandler<String>) -> Element {
    let pair_path = crate::routes::Route::Pair.to_path();
    rsx! {
        div { class: CHECKING_CLASS, "data-testid": "access-unauthorized",
            GateCard { role: "alert",
                Icon { icon: IconType::Lock, class: "size-4" }
                span { "This browser is not authorized on this coordinator." }
                a { href: pair_path.clone(), onclick: move |_| on_navigate.call(pair_path.clone()),
                    "data-testid": "access-unauthorized-pair", class: "underline",
                    "Pair this browser"
                }
            }
        }
    }
}
