//! A folder row's leading machine mark: the Linux distribution logo when one is
//! recognized, otherwise the platform's Lucide glyph. The mark is 16px, too
//! small for an overlay, so the Apple chip appears in the hover title only.
//! `FolderList` and the Machines settings row render it from the worker record.

use dioxus::prelude::*;
use roost_protocol::wire::Worker;

use super::linux_distribution_mark::LinuxDistributionMark;
use super::machine_identity::machine_identity_presentation;
use crate::components::ui::Icon;

/// The mark for `worker`; `context_title` is appended to the hover text.
#[component]
pub fn MachineIdentityMark(worker: Option<Worker>, context_title: Option<String>) -> Element {
    let presentation = machine_identity_presentation(worker.as_ref());
    let title = match context_title {
        Some(context) if !context.is_empty() => format!("{} · {context}", presentation.title),
        _ => presentation.title.clone(),
    };
    rsx! {
        span {
            class: "inline-flex items-center justify-center",
            "data-linux-brand": presentation.linux_brand.map(|brand| brand.as_str()),
            role: "img",
            "aria-label": presentation.title.clone(),
            title,
            if let Some(brand) = presentation.linux_brand {
                LinuxDistributionMark { brand }
            } else {
                Icon { icon: presentation.icon, class: "size-4" }
            }
        }
    }
}
