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

use libero::components::{Box, Button, Code, Flex, Paper, Text, Title};
use libero::sx::sx;

/// The card's headline, which is also the answer this state gives.
pub const LOCAL_ACCESS_HEADLINE: &str = "Connect another machine";

/// The button the reader returns with.
pub const RECHECK_LABEL: &str = "Check again";

const SUPPORTING_LINE: &str = "The new machine needs a reachable HTTPS address for this Roost.";

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
const STEP_THREE_COMMAND: &str = "tailscale serve --bg --https=443 http://127.0.0.1:4113";
const STEP_THREE_TAIL: &str = "An operator-changed loopback port must replace 4113. Both machines \
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
    let paper_sx = use_hook(|| sx().padding("md").gap("md"));
    let steps_sx = use_hook(|| {
        sx().margin("0")
            .padding_left("md")
            .display("grid")
            .gap("md")
            .with("list-style-type", "decimal")
            .color("muted")
            .selector("& li", sx().padding_left("xs"))
    });
    let inline_code_sx = use_hook(|| {
        sx().with("overflow-wrap", "anywhere")
            .font_family("monospace")
    });
    rsx! {
        Paper { sx: paper_sx.clone(), "data-testid": "machine-deploy-local-only",
            Flex { direction: "column", gap: "sm",
                Title { order: 3, size: "md", {LOCAL_ACCESS_HEADLINE} }
                Text { size: "sm", color: "muted", {SUPPORTING_LINE} }
            }
            Flex { direction: "column", gap: "md",
                if checking {
                    Text { size: "sm", color: "muted", "data-testid": "machine-deploy-pending",
                        {CHECKING_LABEL}
                    }
                } else {
                    Box { component: "ol", sx: steps_sx,
                        li { Text { size: "sm", color: "muted", {STEP_ONE} } }
                        li {
                            Text { size: "sm", color: "muted",
                                {STEP_TWO_LEAD}
                                Code { source: STEP_TWO_COMMAND, sx: inline_code_sx.clone() }
                                {" "}
                                {STEP_TWO_TAIL}
                            }
                        }
                        li {
                            Flex { direction: "column", gap: "sm",
                                Text { size: "sm", color: "muted", {STEP_THREE_LEAD} }
                                Code { source: STEP_THREE_COMMAND, sx: inline_code_sx.clone() }
                                Text { size: "sm", color: "muted", {STEP_THREE_TAIL} }
                            }
                        }
                        li {
                            Text { size: "sm", color: "muted",
                                {STEP_FOUR_LEAD}
                                strong { {RECHECK_LABEL} }
                                {STEP_FOUR_TAIL}
                            }
                        }
                    }
                    Button {
                        variant: "outlined",
                        "data-testid": "machine-deploy-recheck",
                        onclick: move |_| on_recheck.call(()),
                        {RECHECK_LABEL}
                    }
                }
            }
        }
    }
}
