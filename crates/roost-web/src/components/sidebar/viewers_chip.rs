//! Per-session viewer avatars, rendered by folder session rows.
//! Client-core derives monograms, identity colors and size-binding control.
//! The component composes stock Avatar and AvatarGroup primitives.

use dioxus::prelude::*;
use roost_client_core::store::sidebar::viewers::session_viewer_avatars;

use crate::components::ui::{Avatar, AvatarFallback, AvatarGroup, AvatarSize};
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
            "data-testid": "session-viewers-{session_id}",
            "data-viewer-count": "{count}",
            AvatarGroup { class: "mr-1.5 items-center",
                for avatar in avatars {
                    span {
                        "data-slot": "avatar",
                        key: "{avatar.fp}",
                        "data-viewer-fp": avatar.fp.clone(),
                        "data-viewer-label": avatar.label.clone(),
                        "data-viewer-name": avatar.name.clone(),
                        "data-controlling": if avatar.controlling { "true" } else { "false" },
                        title: avatar.title.clone(),
                        Avatar {
                            size: AvatarSize::Sm,
                            class: if avatar.controlling { "z-10 ring-2 ring-primary" } else { "" },
                            AvatarFallback { class: "bg-transparent p-0",
                                span {
                                    class: "inline-flex size-full items-center justify-center rounded-full text-xs font-semibold",
                                    style: "background-color: {avatar.color.bg}; color: {avatar.color.fg};",
                                    {avatar.monogram.clone()}
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
