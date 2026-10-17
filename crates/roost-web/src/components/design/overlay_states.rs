//! The `/design` gallery's overlay specimens: a dialog with an action band, a
//! right sheet and a bottom sheet, each opened from a button and mounted by
//! `gallery.rs`. Each overlay is controlled by a signal here; the primitives
//! own portal, focus containment, and dismissal. `ui` has no centred sheet, so
//! the bottom sheet stands in for the md centred variant.

use dioxus::prelude::*;

use crate::components::ui::{
    Button, ButtonVariant, Dialog, DialogAction, DialogBody, DialogContent, DialogDescription,
    DialogFooter, DialogHeader, DialogTitle, IconType, Sheet, SheetContent, SheetHeader, SheetSide,
    SheetTitle,
};

const BODY_TEXT_CLASS: &str = "m-0 text-sm text-muted-foreground";
const SHEET_BODY_CLASS: &str = "grid min-w-[min(20rem,80vw)] gap-4 p-4";

#[component]
pub fn DesignOverlayStates() -> Element {
    let mut dialog_open = use_signal(|| false);
    let mut sheet_open = use_signal(|| false);
    let mut bottom_sheet_open = use_signal(|| false);

    rsx! {
        div { class: "flex flex-wrap items-center gap-3",
            Button {
                variant: ButtonVariant::Default,
                icon: IconType::SquareArrowOutUpRight,
                onclick: move |_| dialog_open.set(true),
                "Open dialog"
            }
            Button {
                variant: ButtonVariant::Secondary,
                icon: IconType::Maximize2,
                onclick: move |_| sheet_open.set(true),
                "Open sheet"
            }
            Button {
                variant: ButtonVariant::Secondary,
                icon: IconType::Maximize2,
                onclick: move |_| bottom_sheet_open.set(true),
                "Open bottom sheet"
            }

            Dialog {
                open: dialog_open(),
                on_open_change: move |open: bool| dialog_open.set(open),
                DialogContent { show_close_button: false,
                    DialogHeader {
                        DialogTitle { "Shared dialog" }
                        DialogDescription {
                            "The Dialog primitive owns its portal, focus containment, and dismissal."
                        }
                    }
                    DialogBody {
                        p { class: BODY_TEXT_CLASS,
                            "Dialog body content remains ordinary application markup."
                        }
                    }
                    DialogFooter {
                        DialogAction { "Done" }
                    }
                }
            }

            Sheet {
                open: sheet_open(),
                on_open_change: move |open: bool| sheet_open.set(open),
                SheetContent { side: SheetSide::Right,
                    SheetHeader {
                        SheetTitle { "Shared sheet" }
                    }
                    div { class: SHEET_BODY_CLASS,
                        p { class: BODY_TEXT_CLASS,
                            "Sheet uses the same Dialog accessibility and dismissal owner with side-specific presentation."
                        }
                    }
                }
            }

            Sheet {
                open: bottom_sheet_open(),
                on_open_change: move |open: bool| bottom_sheet_open.set(open),
                SheetContent { side: SheetSide::Bottom,
                    SheetHeader {
                        SheetTitle { "Bottom sheet" }
                    }
                    div { class: SHEET_BODY_CLASS,
                        p { class: BODY_TEXT_CLASS,
                            "Bottom sheets enter from the lower edge with the same dismissal owner as the other sheets."
                        }
                    }
                }
            }
        }
    }
}
