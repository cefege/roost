//! The operator-managed expansion path: what to set up when Roost answers only
//! on this machine's loopback, and how to come back and check. Ports
//! `apps/web/src/components/machines/MachineLocalAccessGuide.tsx`.
//!
//! It owns no RPC and no transition state — the deploy dialog supplies both —
//! so the one thing it must never do is invite a grant. A reader on a local-only
//! install is told how to make Roost reachable and then told to come back;
//! minting against a door nobody else can reach produces a command that cannot
//! work, pasted on a train.

use dioxus::prelude::*;

use crate::components::md::{Button, ButtonVariant, Card};

/// The card's headline, which is also the answer this state gives.
pub const LOCAL_ACCESS_HEADLINE: &str = "Connect another machine";

/// The button the reader returns with.
pub const RECHECK_LABEL: &str = "Check again";

const SUPPORTING_LINE: &str = "The new machine needs a reachable HTTPS address for this Roost.";

const STACK_STYLE: &str = "display: grid; gap: var(--md-space-4);";
const LIST_STYLE: &str =
    "display: grid; gap: var(--md-space-4); margin: 0; padding: 0 0 0 var(--md-space-5);";
const LIST_ITEM_STYLE: &str = "padding: 0 0 0 var(--md-space-1);";
const SUPPORT_STYLE: &str = "margin: 0; color: var(--md-sys-color-on-surface-variant);";
const CODE_STYLE: &str = "overflow-wrap: anywhere;";

/// The four steps, in the order they have to happen.
///
/// THE ORDER IS THE CONTENT. A front door pointed at a listener that has not
/// been switched to the proxy trust profile answers as if it were still local,
/// and a Generate pressed before the coordinator advertises an external origin
/// mints against a door the target machine cannot dial.
const STEP_ONE: &str = "Choose an HTTPS address supplied by the operator's proxy/tunnel/private-access \
setup. No purchased domain is required; Tailscale Serve can supply a *.ts.net address. Network \
membership alone does not expose Roost.";
const STEP_TWO_LEAD: &str = "Before exposing the running local listener, run ";
const STEP_TWO_COMMAND: &str = "roost quickstart --coordinator-url https://<your-Roost-address>";
const STEP_TWO_TAIL: &str = "on the coordinator machine. This first switches Roost to the proxy \
trust profile.";
const STEP_THREE_LEAD: &str = "Configure the front door to forward to the installed loopback bind \
and overwrite XFF. Tailscale Serve is an optional default-port example:";
const STEP_THREE_COMMAND: &str = "tailscale serve --bg --https=443 http://127.0.0.1:4103";
const STEP_THREE_TAIL: &str = "An operator-changed loopback port must replace 4103. Both machines \
must have the required tailnet route/ACL access; WireGuard alone does not supply HTTPS.";
const STEP_FOUR_LEAD: &str = "Return to this dialog and choose ";
const STEP_FOUR_TAIL: &str = ". Generate the join command only after the coordinator advertises a \
valid external origin. The target must reach that address, and the main machine must stay awake \
and available for remote control.";
const CHECKING_LABEL: &str = "Checking the coordinator's enrollment address…";

/// The expansion path. `checking` is the one answer before the first read lands,
/// and it is still this guide: the reader learns what the dialog is waiting for
/// instead of an empty modal.
#[component]
pub fn MachineLocalAccessGuide(checking: bool, on_recheck: EventHandler<()>) -> Element {
    rsx! {
        Card {
            title: LOCAL_ACCESS_HEADLINE,
            supporting: SUPPORTING_LINE,
            test_id: Some("machine-deploy-local-only".to_owned()),
            div { style: STACK_STYLE,
                if checking {
                    p { class: "md-body-m", style: SUPPORT_STYLE,
                        "data-testid": "machine-deploy-pending",
                        {CHECKING_LABEL}
                    }
                } else {
                    ol { style: LIST_STYLE,
                        li { style: LIST_ITEM_STYLE,
                            p { class: "md-body-m", style: SUPPORT_STYLE, {STEP_ONE} }
                        }
                        li { style: LIST_ITEM_STYLE,
                            p { class: "md-body-m", style: SUPPORT_STYLE,
                                {STEP_TWO_LEAD}
                                code { style: CODE_STYLE, {STEP_TWO_COMMAND} }
                                {" "}
                                {STEP_TWO_TAIL}
                            }
                        }
                        li { style: LIST_ITEM_STYLE,
                            div { style: STACK_STYLE,
                                p { class: "md-body-m", style: SUPPORT_STYLE, {STEP_THREE_LEAD} }
                                code { class: "md-body-s", style: CODE_STYLE, {STEP_THREE_COMMAND} }
                                p { class: "md-body-m", style: SUPPORT_STYLE, {STEP_THREE_TAIL} }
                            }
                        }
                        li { style: LIST_ITEM_STYLE,
                            p { class: "md-body-m", style: SUPPORT_STYLE,
                                {STEP_FOUR_LEAD}
                                strong { {RECHECK_LABEL} }
                                {STEP_FOUR_TAIL}
                            }
                        }
                    }
                    div {
                        Button {
                            variant: ButtonVariant::Secondary,
                            "data-testid": "machine-deploy-recheck",
                            onclick: move |_| on_recheck.call(()),
                            {RECHECK_LABEL}
                        }
                    }
                }
            }
        }
    }
}
