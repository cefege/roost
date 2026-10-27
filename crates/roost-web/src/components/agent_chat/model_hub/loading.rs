//! The model hub's loading and settings-error surface.
//! Mounted while the catalog or shared settings are unavailable.
//! Uses the same dialog bounds and close behavior as the loaded hub.

use dioxus::prelude::*;

use super::HUB_DIALOG_CLASS;
use crate::components::ui::{
    Dialog, DialogBody, DialogContent, DialogHeader, DialogTitle, FieldError, Skeleton,
};

#[component]
pub(super) fn ModelHubLoading(error: String, on_open_change: EventHandler<bool>) -> Element {
    rsx! {
        Dialog { open: true, on_open_change,
            DialogContent { class: HUB_DIALOG_CLASS, test_id: "agent-model-hub",
                DialogHeader { DialogTitle { "Models" } }
                DialogBody { class: "min-h-0 flex-1 gap-0 p-5",
                    if !error.is_empty() {
                        FieldError { "{error}" }
                    } else {
                        div { class: "flex flex-col gap-2", role: "status", "aria-label": "Loading models",
                            for index in 0..4 { Skeleton { key: "{index}", class: "h-8" } }
                        }
                    }
                }
            }
        }
    }
}
