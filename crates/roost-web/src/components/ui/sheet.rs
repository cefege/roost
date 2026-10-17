//! `Sheet`: a `Dialog` anchored to a screen edge. It adds only the edge geometry
//! and surface; modal semantics, focus, and dismissal stay the dialog's, so a
//! sheet cannot drift from the dialog in behaviour. Parts are the dialog's own,
//! re-exposed under sheet names.

use dioxus::prelude::*;

use super::dialog::{
    Dialog, DialogClose, DialogContent, DialogDescription, DialogFooter, DialogHeader,
    DialogTitle, DialogTrigger,
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

/// The sheet panel. Each side resets the dialog's centring and sizes the panel
/// to that edge; the caller's `class` is appended last so it wins a conflict.
#[component]
pub fn SheetContent(
    #[props(default)] side: SheetSide,
    #[props(into, optional)] class: Option<String>,
    #[props(into, optional)] test_id: Option<String>,
    #[props(default = true)] show_close_button: bool,
    #[props(default = true)] close_on_backdrop_click: bool,
    on_open_auto_focus: Option<EventHandler<AutoFocusRequest>>,
    on_close_auto_focus: Option<EventHandler<AutoFocusRequest>>,
    children: Element,
) -> Element {
    let merged = format!(
        "{} bg-card shadow-lg overflow-y-auto overscroll-y-contain {}",
        side_class(side),
        class.as_deref().unwrap_or("")
    );
    rsx! {
        DialogContent {
            class: merged,
            test_id,
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
pub fn SheetDescription(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
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
            "top-0 bottom-auto left-0 right-0 translate-x-0 translate-y-0 w-full h-[400px] max-w-none max-h-none rounded-none"
        }
        SheetSide::Bottom => {
            "top-auto bottom-0 left-0 right-0 translate-x-0 translate-y-0 w-full h-[400px] max-w-none max-h-none rounded-none"
        }
        SheetSide::Left => {
            "top-0 bottom-0 left-0 right-auto translate-x-0 translate-y-0 h-full w-[400px] max-w-none max-h-none rounded-none"
        }
        SheetSide::Right => {
            "top-0 bottom-0 right-0 left-auto translate-x-0 translate-y-0 h-full w-[400px] max-w-none max-h-none rounded-none"
        }
    }
}
