//! The machine chooser a bare `/browse` shows when more than one machine is
//! online: a new terminal never lands on a machine the reader did not pick.
//! Mounted by `browse::BrowseRedirect`; each row navigates to
//! `/browse/:workerFp`, where the folder picker takes over.

use dioxus::prelude::*;
use roost_protocol::wire::Worker;

use crate::components::browse::short_worker_label;
use crate::components::ui::{
    Button, ButtonVariant, Card, CardContent, CardDescription, CardHeader, CardTitle, Icon,
    IconType, StatusDot,
};
use crate::router_state::use_navigate;
use crate::routes::browse_href;

/// One button per online machine, in the order given.
#[component]
pub fn BrowseMachineChoice(machines: Vec<Worker>) -> Element {
    let navigate = use_navigate();
    rsx! {
        div { class: "flex h-full w-full items-start justify-center overflow-y-auto p-4",
            Card { class: "w-full max-w-md", "data-testid": "browse-machine-choice",
                CardHeader {
                    CardTitle { "New terminal" }
                    CardDescription { "Which machine should it open on?" }
                }
                CardContent { class: "flex flex-col gap-2",
                    for worker in machines {
                        Button {
                            key: "{worker.fp}",
                            variant: ButtonVariant::Outline,
                            class: "w-full justify-start gap-2",
                            "data-testid": "browse-machine-option",
                            onclick: {
                                let fp = worker.fp.clone();
                                move |_| {
                                    tracing::info!(target: "browse", worker_fp = %fp, "machine chosen for new terminal");
                                    navigate.call(browse_href(fp.as_str()));
                                }
                            },
                            StatusDot { status: "ok".to_owned() }
                            span { class: "truncate", {short_worker_label(worker.fp.as_str(), &worker.label)} }
                            Icon { icon: IconType::ChevronRight, class: "ml-auto" }
                        }
                    }
                }
            }
        }
    }
}
