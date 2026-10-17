//! A folder row's leading machine mark: the Linux distribution logo when one is
//! recognized, otherwise the platform's Lucide glyph, plus the Apple chip
//! badge. Ports `apps/web/src/components/machines/MachineIdentityMark.tsx`;
//! `FolderList` renders it from the worker record.

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
            class: "relative inline-flex items-center justify-center",
            "data-linux-brand": presentation.linux_brand.map(|brand| brand.as_str()),
            role: "img",
            "aria-label": presentation.title.clone(),
            title,
            if let Some(brand) = presentation.linux_brand {
                LinuxDistributionMark { brand }
            } else {
                Icon { icon: presentation.icon, class: "size-4" }
            }
            if let Some(badge) = presentation.apple_chip_badge {
                span { class: "absolute -bottom-1 left-1/2 -translate-x-1/2 whitespace-nowrap rounded-full border border-current bg-[var(--surface-1)] px-1 font-mono text-xs leading-none text-[var(--text-hi)]", {badge} }
            }
        }
    }
}
