//! The per-session viewer avatar stack: one tonal monogram per browser tab
//! viewing the session, overlapped, with the size-binding viewer haloed. Ports
//! `apps/web/src/components/sidebar/ViewersChip.tsx`; the session rows render
//! it. The avatars are `roost_client_core::store::sidebar::viewers`.

use dioxus::prelude::*;
use roost_client_core::store::sidebar::viewers::session_viewer_avatars;

use crate::pump::use_store;

/// The ring every avatar draws against the surface, and the halo a
/// controller adds. The surface and primary colours are per-avatar-independent
/// design tokens, so they stay in the inline box-shadow string.
const RING: &str = "0 0 0 2px var(--md-sys-color-surface)";
const CONTROLLER_HALO: &str = "0 0 0 2px var(--md-sys-color-surface), 0 0 0 3px var(--md-sys-color-primary), \
     0 0 var(--md-space-2) 1px color-mix(in srgb, var(--md-sys-color-primary) 60%, transparent)";

/// The stack for `session_id`; nothing while nobody is viewing.
#[component]
pub fn ViewersChip(session_id: String) -> Element {
    let pump = use_store();
    let avatars = session_viewer_avatars(pump.core().borrow().store(), &session_id);
    if avatars.is_empty() {
        return rsx! {};
    }
    let count = avatars.len();
    rsx! {
        span {
            class: "mr-1.5 inline-flex items-center gap-1",
            "data-testid": "session-viewers-{session_id}",
            "data-viewer-count": "{count}",
            style: "font-family: var(--md-font);",
            for (index, avatar) in avatars.into_iter().enumerate() {
                span {
                    key: "{avatar.fp}-{index}",
                    class: "relative inline-flex size-5.5 shrink-0 cursor-default select-none items-center justify-center rounded-full font-semibold leading-none",
                    "data-viewer-fp": avatar.fp.clone(),
                    "data-viewer-label": avatar.label.clone(),
                    "data-viewer-name": avatar.name.clone(),
                    "data-controlling": if avatar.controlling { "true" } else { "false" },
                    title: avatar.title.clone(),
                    style: "margin-left: {overlap(index)}; background: {avatar.color.bg}; color: {avatar.color.fg}; \
                            box-shadow: {shadow(avatar.controlling)}; z-index: {z(avatar.controlling)}; \
                            font-size: var(--md-label-s-size); letter-spacing: 0.2px;",
                    {avatar.monogram.clone()}
                }
            }
        }
    }
}

fn overlap(index: usize) -> &'static str {
    if index == 0 { "0" } else { "-7px" }
}

fn shadow(controlling: bool) -> &'static str {
    if controlling { CONTROLLER_HALO } else { RING }
}

fn z(controlling: bool) -> &'static str {
    if controlling { "1" } else { "0" }
}
