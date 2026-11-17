//! Closed-default access screens and the shared gate alert frame.
//! Checking exposes safe connection recovery, not the protected shell.
//! PairSurface owns approval ceremonies above this gate.

use dioxus::prelude::*;

use crate::components::ui::{Alert, AlertVariant};

/// Full-height frame for access gates; TV padding follows the overscan tokens.
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
/// Recovery remains usable while protected workspace data stays closed.
#[component]
pub fn CheckingScreen() -> Element {
    rsx! {
        div { class: "h-dvh w-full overflow-y-auto", "data-testid": "access-checking",
            div { class: CHECKING_CLASS,
            div { class: "mx-auto w-full max-w-2xl",
                crate::components::access::AccessSurface {}
            }
            }
        }
    }
}
