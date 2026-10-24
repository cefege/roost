//! The per-session viewer avatar stack: one tonal monogram per browser tab
//! viewing the session, overlapped, with the size-binding viewer haloed. Ports
//! `apps/web/src/components/sidebar/ViewersChip.tsx`; the session rows render
//! it. The avatars are `roost_client_core::store::sidebar::viewers`.

use dioxus::prelude::*;
use roost_client_core::store::sidebar::viewers::session_viewer_avatars;

use crate::pump::use_store;
use libero::components::{Box, Flex};
use libero::sx::sx;

/// The stack for `session_id`; nothing while nobody is viewing.
#[component]
pub fn ViewersChip(session_id: String) -> Element {
    let pump = use_store();
    let avatars = session_viewer_avatars(pump.core().borrow().store(), &session_id);
    let count = avatars.len();
    let stack_sx = use_hook(|| {
        sx().display("flex")
            .align_items("center")
            .gap("xs")
            .margin_right("sm")
    });
    let avatar_sx = use_hook(|| {
        sx().position("relative")
            .display("flex")
            .flex_shrink("0")
            .align_items("center")
            .justify_content("center")
            .width("1.375rem")
            .height("1.375rem")
            .border_radius("9999rem")
            .font_size("xs")
            .font_weight("600")
            .with("box-shadow", "0 0 0 2px var(--lsx-surface)")
            .selector(
                "&[data-controlling=true]",
                sx().with("z-index", "1").with(
                    "box-shadow",
                    "0 0 0 2px var(--lsx-surface), 0 0 0 3px var(--lsx-primary-6)",
                ),
            )
    });
    if avatars.is_empty() {
        return rsx! {};
    }
    rsx! {
        Flex { direction: "row", sx: stack_sx,
            "data-testid": "session-viewers-{session_id}",
            "data-viewer-count": "{count}",
            for (index, avatar) in avatars.into_iter().enumerate() {
                Box { component: "span",
                    sx: avatar_sx.clone(),
                    "data-viewer-fp": avatar.fp.clone(),
                    "data-viewer-label": avatar.label.clone(),
                    "data-viewer-name": avatar.name.clone(),
                    "data-controlling": if avatar.controlling { "true" } else { "false" },
                    title: avatar.title.clone(),
                    style: "margin-left: {overlap(index)}; background-color: {avatar.color.bg}; color: {avatar.color.fg};",
                    {avatar.monogram.clone()}
                }
            }
        }
    }
}

fn overlap(index: usize) -> &'static str {
    if index == 0 { "0" } else { "-7px" }
}
