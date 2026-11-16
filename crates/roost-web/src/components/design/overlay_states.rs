//! Dialog, alert dialog, sheet, context-menu and dropdown-menu specimens for the
//! design gallery. `gallery.rs` mounts the controls; local signals own their
//! open state. Shared UI primitives own anchoring, portals, focus and dismissal.

use dioxus::prelude::*;

use crate::components::sidebar::context_menu_frame::ContextMenuFrame;
use crate::components::ui::{
    AlertDialog, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter,
    AlertDialogHeader, AlertDialogTitle, Button, ButtonVariant, ContextMenuItem,
    ContextMenuSeparator, Dialog, DialogBody, DialogClose, DialogContent, DialogDescription,
    DialogFooter, DialogHeader, DialogTitle, DropdownMenu, DropdownMenuContent, DropdownMenuItem,
    DropdownMenuLabel, DropdownMenuTrigger, IconType, PhoneSheetContent, Sheet, SheetContent,
    SheetHeader, SheetSide, SheetTitle,
};

const BODY_TEXT_CLASS: &str = "m-0 text-sm text-muted-foreground";
const SHEET_BODY_CLASS: &str = "grid min-w-[min(20rem,80vw)] gap-4 p-4";

#[component]
pub fn DesignOverlayStates() -> Element {
    let mut dialog_open = use_signal(|| false);
    let mut fullscreen_dialog_open = use_signal(|| false);
    let mut sheet_open = use_signal(|| false);
    let mut bottom_sheet_open = use_signal(|| false);
    let mut phone_sheet_open = use_signal(|| false);
    let mut menu_open = use_signal(|| false);
    let mut alert_open = use_signal(|| false);
    let mut context_at = use_signal(|| None::<(f64, f64)>);

    rsx! {
        div { class: "flex flex-wrap items-center gap-3",
            Button {
                variant: ButtonVariant::Default,
                icon: IconType::SquareArrowOutUpRight,
                onclick: move |_| dialog_open.set(true),
                "Open dialog"
            }
            Button {
                variant: ButtonVariant::Destructive,
                icon: IconType::TriangleAlert,
                onclick: move |_| alert_open.set(true),
                "Open alert dialog"
            }
            Button {
                variant: ButtonVariant::Secondary,
                icon: IconType::Maximize2,
                onclick: move |_| fullscreen_dialog_open.set(true),
                "Open fullscreen dialog"
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
            Button {
                variant: ButtonVariant::Secondary,
                icon: IconType::Smartphone,
                onclick: move |_| phone_sheet_open.set(true),
                "Open phone sheet"
            }
            DropdownMenu {
                open: menu_open(),
                on_open_change: move |open: bool| menu_open.set(open),
                DropdownMenuTrigger { "Overlay menu" }
                DropdownMenuContent {
                    DropdownMenuLabel { "Open overlay" }
                    DropdownMenuItem {
                        onclick: move |_| dialog_open.set(true),
                        "Dialog"
                    }
                    DropdownMenuItem {
                        onclick: move |_| sheet_open.set(true),
                        "Sheet"
                    }
                }
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
                        DialogClose { "Done" }
                    }
                }
            }
            Dialog {
                open: fullscreen_dialog_open(),
                on_open_change: move |open: bool| fullscreen_dialog_open.set(open),
                DialogContent { fullscreen: true, show_close_button: false,
                    DialogHeader {
                        DialogTitle { "Fullscreen dialog" }
                        DialogDescription {
                            "The shared fullscreen variant fills the viewport without caller-level shape or border overrides."
                        }
                    }
                    DialogBody {
                        p { class: BODY_TEXT_CLASS,
                            "Fullscreen presentation is a shared Dialog variant; content layout remains caller-owned."
                        }
                    }
                    DialogFooter {
                        DialogClose { "Done" }
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
            Sheet {
                open: phone_sheet_open(),
                on_open_change: move |open: bool| phone_sheet_open.set(open),
                PhoneSheetContent {
                    SheetHeader {
                        SheetTitle { "Phone sheet" }
                    }
                    div { class: SHEET_BODY_CLASS,
                        p { class: BODY_TEXT_CLASS,
                            "Phone sheets rise from the lower edge with a top-only radius and a scrim, the pi-pocket look chosen for phones."
                        }
                    }
                }
            }

            AlertDialog {
                open: alert_open(),
                on_open_change: move |open: bool| alert_open.set(open),
                AlertDialogContent {
                    AlertDialogHeader {
                        AlertDialogTitle { "Discard this draft?" }
                        AlertDialogDescription {
                            "A backdrop click does nothing: the only ways out are the two buttons."
                        }
                    }
                    AlertDialogFooter {
                        AlertDialogCancel { "Keep editing" }
                        Button {
                            variant: ButtonVariant::Destructive,
                            onclick: move |_| alert_open.set(false),
                            "Discard"
                        }
                    }
                }
            }

            Button {
                variant: ButtonVariant::Secondary,
                icon: IconType::SquareArrowOutUpRight,
                onclick: move |event: MouseEvent| {
                    let point = event.client_coordinates();
                    context_at.set(Some((point.x, point.y)));
                },
                "Open context menu"
            }
            if let Some((x, y)) = context_at() {
                ContextMenuFrame {
                    x,
                    y,
                    test_id: "design-context-menu".to_owned(),
                    label: "Specimen actions",
                    on_close: move |()| context_at.set(None),
                    ContextMenuItem { "Copy" }
                    ContextMenuItem { "Find" }
                    ContextMenuSeparator {}
                    ContextMenuItem { disabled: true, "Unavailable" }
                }
            }
        }
    }
}
