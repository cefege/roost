//! `DesignOverlayStates`: the modal reference on `/design` — the shared dialog
//! with an action band, a right sheet, and a centred sheet, each opened from a
//! button and closed through the primitive's own dismissal. Ported from
//! `apps/web/src/components/design/DesignOverlayStates.tsx`; `gallery.rs`
//! mounts it. Focus, dismissal and scrim behaviour are `Dialog`'s, not this
//! file's.

use dioxus::prelude::*;

use crate::components::md::{Button, ButtonVariant, Dialog, Sheet, SheetSide};

/// Body copy in the body-medium ramp.
const BODY_M_STYLE: &str = "margin: 0; color: var(--text-mid); font-size: var(--md-body-m-size); \
     line-height: var(--md-body-m-line);";

/// A sheet body wide enough to read as a sheet on a desktop.
const SHEET_BODY_STYLE: &str = "display: grid; gap: var(--md-space-4); \
     min-inline-size: min(calc(var(--md-space-9) * 5), 80vw);";

/// The overlay reference.
#[component]
pub fn DesignOverlayStates() -> Element {
    let mut dialog_open = use_signal(|| false);
    let mut sheet_open = use_signal(|| false);
    let mut centered_sheet_open = use_signal(|| false);

    rsx! {
        div { style: "display: flex; flex-wrap: wrap; gap: var(--md-space-3); align-items: center;",
            Button {
                variant: ButtonVariant::Default,
                icon: "open_in_new",
                onclick: move |_| dialog_open.set(true),
                "Open dialog"
            }
            Button {
                variant: ButtonVariant::Secondary,
                icon: "open_in_full",
                onclick: move |_| sheet_open.set(true),
                "Open sheet"
            }
            Button {
                variant: ButtonVariant::Secondary,
                icon: "open_in_full",
                onclick: move |_| centered_sheet_open.set(true),
                "Open centered sheet"
            }

            Dialog {
                open: dialog_open(),
                on_close: move |_| dialog_open.set(false),
                headline: "Shared dialog",
                description: rsx! { "The Dialog primitive owns its portal, focus containment, and dismissal." },
                show_close_button: false,
                actions: rsx! {
                    Button { variant: ButtonVariant::Default, onclick: move |_| dialog_open.set(false), "Done" }
                },
                p { style: BODY_M_STYLE, "Dialog body content remains ordinary application markup." }
            }

            Sheet {
                open: sheet_open(),
                on_close: move |_| sheet_open.set(false),
                headline: "Shared sheet",
                side: SheetSide::Right,
                div { style: SHEET_BODY_STYLE,
                    p { style: BODY_M_STYLE,
                        "Sheet uses the same Dialog accessibility and dismissal owner with side-specific presentation."
                    }
                }
            }

            Sheet {
                open: centered_sheet_open(),
                on_close: move |_| centered_sheet_open.set(false),
                headline: "Centered sheet",
                side: SheetSide::Center,
                div { style: SHEET_BODY_STYLE,
                    p { style: BODY_M_STYLE,
                        "Centered sheets provide the shared modal presentation for focused desktop work."
                    }
                }
            }
        }
    }
}
