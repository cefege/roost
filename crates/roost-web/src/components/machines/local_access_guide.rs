//! Connects machine enrollment's local-only state to the unified Access screen.
//! The deploy dialog owns checking and closes before navigation.
//! No enrollment grant is offered until a reachable HTTPS route is verified.

use dioxus::prelude::*;

use crate::components::ui::{
    Button, ButtonVariant, Card, CardContent, CardDescription, CardHeader, CardTitle,
};

pub const RECHECK_LABEL: &str = "Check again";

#[component]
pub fn MachineLocalAccessGuide(
    checking: bool,
    on_recheck: EventHandler<()>,
    on_open_access: EventHandler<()>,
) -> Element {
    rsx! {
        Card { "data-testid": "machine-deploy-local-only",
            CardHeader {
                CardTitle { "Connect another machine" }
                CardDescription { "The new machine needs a reachable HTTPS address for this Roost." }
            }
            CardContent { class: "flex flex-col gap-3",
                if checking {
                    p { class: "m-0 text-muted-foreground", "data-testid": "machine-deploy-pending",
                        "Checking the coordinator's enrollment address…"
                    }
                }
                p { class: "m-0 text-sm text-muted-foreground",
                    "Use Access to review installation ownership and set up Tailscale or a stable HTTPS route. Return here after the new route is verified."
                }
                div { class: "flex flex-wrap gap-2",
                    Button { variant: ButtonVariant::Default, onclick: move |_| on_open_access.call(()), "Open Access" }
                    Button {
                        variant: ButtonVariant::Secondary,
                        disabled: checking,
                        "data-testid": "machine-deploy-recheck",
                        onclick: move |_| on_recheck.call(()),
                        {RECHECK_LABEL}
                    }
                }
            }
        }
    }
}
