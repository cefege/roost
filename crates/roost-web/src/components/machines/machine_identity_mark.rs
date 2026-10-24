//! A folder row's leading machine mark: the Linux distribution logo when one is
//! recognized, otherwise the platform's Lucide glyph, plus the Apple chip
//! badge. Ports `apps/web/src/components/machines/MachineIdentityMark.tsx`;
//! `FolderList` renders it from the worker record.

use dioxus::prelude::*;
use libero::components::{Badge, Box, Pictogram};
use libero::sx::sx;
use roost_protocol::wire::Worker;

use super::linux_distribution_mark::LinuxDistributionMark;
use super::machine_identity::machine_identity_presentation;

/// The mark for `worker`; `context_title` is appended to the hover text.
#[component]
pub fn MachineIdentityMark(worker: Option<Worker>, context_title: Option<String>) -> Element {
    let presentation = machine_identity_presentation(worker.as_ref());
    let title = match context_title {
        Some(context) if !context.is_empty() => format!("{} · {context}", presentation.title),
        _ => presentation.title.clone(),
    };
    let mark_sx = use_hook(|| {
        sx().position("relative")
            .display("inline-flex")
            .align_items("center")
            .justify_content("center")
    });
    let badge_sx = use_hook(|| {
        sx().position("absolute")
            .with("bottom", "-0.25rem")
            .left("50%")
            .with("transform", "translateX(-50%)")
            .white_space("nowrap")
            .font_family("monospace")
    });
    rsx! {
        Box {
            component: "span",
            sx: mark_sx,
            "data-linux-brand": presentation.linux_brand.map(|brand| brand.as_str()),
            role: "img",
            "aria-label": presentation.title.clone(),
            title,
            if let Some(brand) = presentation.linux_brand {
                LinuxDistributionMark { brand }
            } else {
                Pictogram { icon: presentation.icon, width: "1rem", height: "1rem" }
            }
            if let Some(badge) = presentation.apple_chip_badge {
                Badge {
                    variant: "outlined",
                    color: "muted",
                    size: "xs",
                    sx: badge_sx,
                    {badge}
                }
            }
        }
    }
}
