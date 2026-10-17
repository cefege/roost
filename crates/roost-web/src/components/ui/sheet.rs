//! `Sheet`: a `Dialog` anchored to a screen edge. It adds only the edge geometry
//! and surface; modal semantics, focus, and dismissal stay the dialog's, so a
//! sheet cannot drift from the dialog in behaviour. Parts are the dialog's own,
//! re-exposed under sheet names.

use dioxus::prelude::*;
use tw_merge::tw_merge;

use super::dialog::{
    Dialog, DialogClose, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle,
    DialogTrigger,
};
use super::focus_scope::AutoFocusRequest;

/// The screen edge a sheet slides in from. Centred sheets are not a sheet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SheetSide {
    Top,
    #[default]
    Right,
    Bottom,
    Left,
}

/// A controlled sheet. The caller owns `open`, as for `Dialog`.
#[component]
pub fn Sheet(open: bool, on_open_change: EventHandler<bool>, children: Element) -> Element {
    rsx! { Dialog { open, on_open_change, {children} } }
}

#[component]
pub fn SheetTrigger(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    rsx! { DialogTrigger { class, {children} } }
}

/// A bottom sheet rides above the soft keyboard `--kb-offset` measures and
/// gives up the height it covers. Inside an editor region that the keyboard
/// already translates (`data-keyboard-shift`), that translate is the sheet's
/// containing block, so lifting again would push its top off the screen.
const BOTTOM_KEYBOARD_LIFT: &str = "bottom-[max(var(--kb-offset),0px)] max-h-[calc(90dvh-max(var(--kb-offset),0px))] in-data-[keyboard-shift=true]:bottom-0";

/// The sheet panel. Each side resets the dialog's centring and sizes the panel
/// to that edge; the caller's `class` is appended last so it wins a conflict.
#[component]
pub fn SheetContent(
    #[props(default)] side: SheetSide,
    #[props(into, optional)] class: Option<String>,
    #[props(into, optional)] test_id: Option<String>,
    #[props(into, optional)] style: Option<String>,
    #[props(into, optional)] scrim_test_id: Option<String>,
    #[props(default = true)] show_close_button: bool,
    #[props(default = true)] close_on_backdrop_click: bool,
    on_open_auto_focus: Option<EventHandler<AutoFocusRequest>>,
    on_close_auto_focus: Option<EventHandler<AutoFocusRequest>>,
    children: Element,
) -> Element {
    let keyboard_lift = if side == SheetSide::Bottom {
        BOTTOM_KEYBOARD_LIFT
    } else {
        ""
    };
    let merged = tw_merge!(
        side_class(side),
        "bg-card shadow-lg overflow-y-auto overscroll-y-contain",
        keyboard_lift,
        class.as_deref().unwrap_or("")
    );
    rsx! {
        DialogContent {
            class: merged,
            test_id,
            style,
            scrim_test_id,
            show_close_button: Some(show_close_button),
            close_on_backdrop_click,
            on_open_auto_focus,
            on_close_auto_focus,
            {children}
        }
    }
}

#[component]
pub fn SheetHeader(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    rsx! { DialogHeader { class, {children} } }
}

#[component]
pub fn SheetTitle(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    rsx! { DialogTitle { class, {children} } }
}

#[component]
pub fn SheetDescription(
    #[props(into, optional)] class: Option<String>,
    children: Element,
) -> Element {
    rsx! { DialogDescription { class, {children} } }
}

#[component]
pub fn SheetFooter(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    rsx! { DialogFooter { class, {children} } }
}

#[component]
pub fn SheetClose(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    rsx! { DialogClose { class, {children} } }
}

fn side_class(side: SheetSide) -> &'static str {
    match side {
        SheetSide::Top => {
            "top-0 bottom-auto left-0 right-0 translate-x-0 translate-y-0 w-full h-100 max-w-none max-h-none rounded-none"
        }
        SheetSide::Bottom => {
            "top-auto bottom-0 left-0 right-0 translate-x-0 translate-y-0 w-full h-100 max-w-none max-h-none rounded-none"
        }
        SheetSide::Left => {
            "top-0 bottom-0 left-0 right-auto translate-x-0 translate-y-0 h-full w-100 max-w-none max-h-none rounded-none"
        }
        SheetSide::Right => {
            "top-0 bottom-0 right-0 left-auto translate-x-0 translate-y-0 h-full w-100 max-w-none max-h-none rounded-none"
        }
    }
}
