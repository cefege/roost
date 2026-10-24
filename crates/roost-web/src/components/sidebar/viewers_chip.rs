//! The per-session viewer avatar stack: one tonal monogram per browser tab
//! viewing the session, overlapped, with the size-binding viewer haloed. Ports
//! `apps/web/src/components/sidebar/ViewersChip.tsx`; the session rows render
//! it. The avatars are `roost_client_core::store::sidebar::viewers`.

use dioxus::prelude::*;
use roost_client_core::store::sidebar::viewers::session_viewer_avatars;

use crate::pump::use_store;

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
            for (index, avatar) in avatars.into_iter().enumerate() {
                span {
                    key: "{avatar.fp}-{index}",
                    class: avatar_class(avatar.controlling),
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

/// The avatar's Tailwind classes. The controller's halo is a second ring
/// outside the surface ring, and it raises the avatar above its neighbours.
fn avatar_class(controlling: bool) -> &'static str {
    if controlling {
        "relative z-1 inline-flex size-5.5 shrink-0 cursor-default select-none items-center justify-center rounded-full text-xs font-semibold leading-none ring-2 ring-background shadow-[0_0_0_3px_var(--primary)]"
    } else {
        "relative z-0 inline-flex size-5.5 shrink-0 cursor-default select-none items-center justify-center rounded-full text-xs font-semibold leading-none ring-2 ring-background"
    }
}

fn overlap(index: usize) -> &'static str {
    if index == 0 { "0" } else { "-7px" }
}
