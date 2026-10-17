//! `Drawer`: a `Dialog` that slides in from a screen edge, with a drag handle
//! and an optional floating surface. Like `Sheet`, it adds only geometry; the
//! modal semantics, focus, and dismissal are the dialog's. Its open state is
//! the caller's, so there is no script and no document-level listener.

use dioxus::prelude::*;

use super::dialog::{
    Dialog, DialogClose, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle,
    DialogTrigger,
};
use super::dialog_parts::DialogBody;
use super::focus_scope::AutoFocusRequest;

/// The screen edge a drawer slides in from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DrawerPosition {
    #[default]
    Bottom,
    Top,
    Left,
    Right,
}

/// `Default` is attached to its edge; `Floating` is inset from every edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DrawerVariant {
    #[default]
    Default,
    Floating,
}

/// A controlled drawer. The caller owns `open`, as for `Dialog`.
#[component]
pub fn Drawer(open: bool, on_open_change: EventHandler<bool>, children: Element) -> Element {
    rsx! { Dialog { open, on_open_change, {children} } }
}

#[component]
pub fn DrawerTrigger(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    rsx! { DialogTrigger { class, {children} } }
}

/// The drawer panel. `dismissible` decides whether a backdrop click closes it;
/// the caller's `class` is appended last so it wins a conflict.
#[component]
pub fn DrawerContent(
    #[props(into, optional)] class: Option<String>,
    #[props(into, optional)] test_id: Option<String>,
    #[props(default = true)] dismissible: bool,
    #[props(default)] position: DrawerPosition,
    #[props(default)] variant: DrawerVariant,
    on_open_auto_focus: Option<EventHandler<AutoFocusRequest>>,
    on_close_auto_focus: Option<EventHandler<AutoFocusRequest>>,
    children: Element,
) -> Element {
    let merged = format!(
        "{} {} flex flex-col pt-3 pb-6 px-6 max-h-[90vh] overflow-auto shadow-lg {}",
        position_class(position),
        variant_class(variant),
        class.as_deref().unwrap_or("")
    );
    rsx! {
        DialogContent {
            class: merged,
            test_id,
            close_on_backdrop_click: dismissible,
            on_open_auto_focus,
            on_close_auto_focus,
            {children}
        }
    }
}

/// The grab bar shown at the top of a drawer. Decorative: it has no behaviour.
#[component]
pub fn DrawerHandle() -> Element {
    rsx! {
        div { class: "block relative mx-auto mb-8 w-8 rounded-2xl opacity-70 hover:opacity-100 active:opacity-100 shrink-0 bg-muted-foreground h-[5px]" }
    }
}

#[component]
pub fn DrawerClose(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    rsx! { DialogClose { class, {children} } }
}

#[component]
pub fn DrawerHeader(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    rsx! { DialogHeader { class, {children} } }
}

#[component]
pub fn DrawerTitle(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    rsx! { DialogTitle { class, {children} } }
}

#[component]
pub fn DrawerDescription(
    #[props(into, optional)] class: Option<String>,
    children: Element,
) -> Element {
    rsx! { DialogDescription { class, {children} } }
}

/// The drawer body, centred and capped at a readable measure.
#[component]
pub fn DrawerBody(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    let merged = format!("mx-auto max-w-[500px] {}", class.as_deref().unwrap_or(""));
    rsx! { DialogBody { class: merged, {children} } }
}

#[component]
pub fn DrawerFooter(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    rsx! { DialogFooter { class, {children} } }
}

/// Recipe that resets the dialog's centring and pins the panel to one edge.
fn position_class(position: DrawerPosition) -> &'static str {
    match position {
        DrawerPosition::Bottom => {
            "top-auto bottom-0 left-0 right-0 translate-x-0 translate-y-0 w-full max-w-none max-h-[90vh] rounded-t-[10px] rounded-b-none border-t"
        }
        DrawerPosition::Top => {
            "top-0 bottom-auto left-0 right-0 translate-x-0 translate-y-0 w-full max-w-none max-h-[90vh] rounded-b-[10px] rounded-t-none border-b"
        }
        DrawerPosition::Left => {
            "top-0 bottom-0 left-0 right-auto translate-x-0 translate-y-0 h-full w-[80vw] max-w-sm max-h-none rounded-r-[10px] rounded-l-none border-r"
        }
        DrawerPosition::Right => {
            "top-0 bottom-0 right-0 left-auto translate-x-0 translate-y-0 h-full w-[80vw] max-w-sm max-h-none rounded-l-[10px] rounded-r-none border-l"
        }
    }
}

fn variant_class(variant: DrawerVariant) -> &'static str {
    match variant {
        DrawerVariant::Default => "",
        DrawerVariant::Floating => "m-4 rounded-[10px] border",
    }
}
